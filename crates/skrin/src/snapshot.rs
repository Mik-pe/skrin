//! Sealed, bounded, key-ordered snapshots. No truncation is safe.
use crate::codec::MAX_RECORD_BYTES;
use crate::directory::boundary;
use crate::log::{Checksum, checksum};
use crate::{Decoder, Error, MaintenanceEstimate, MaintenanceOptions, Record, Result};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const HEADER_LEN: usize = 48;
const BUFFER_BYTES: usize = 64 * 1024;

fn header<R: Record>(generation: u64, sequence: u64, count: u64) -> Vec<u8> {
    let mut bytes = b"SKRSNP01".to_vec();
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(&R::SCHEMA.table_id.to_le_bytes());
    bytes.extend_from_slice(&R::SCHEMA.version.to_le_bytes());
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
    bytes
}

pub(crate) fn estimate<R: Record>(
    rows: &BTreeMap<u64, R>,
    sequence: u64,
    overhead: u64,
    options: MaintenanceOptions,
) -> Result<MaintenanceEstimate> {
    options.validate()?;
    options.check_rows(rows.len() as u64)?;
    let mut encoder = options.encoder();
    let mut snapshot_bytes = HEADER_LEN as u64;
    let mut largest_record_bytes = 0;
    options.check_bytes(snapshot_bytes.saturating_add(overhead))?;
    for row in rows.values() {
        encoder.clear();
        row.encode(&mut encoder)?;
        let size = encoder.as_slice().len();
        largest_record_bytes = largest_record_bytes.max(size);
        snapshot_bytes = snapshot_bytes
            .checked_add(size as u64 + 16)
            .ok_or(Error::SequenceExhausted)?;
        options.check_bytes(snapshot_bytes.saturating_add(overhead))?;
    }
    Ok(MaintenanceEstimate {
        rows: rows.len() as u64,
        sequence,
        snapshot_bytes,
        new_file_bytes: snapshot_bytes + overhead,
        largest_record_bytes,
    })
}

pub(crate) fn write<R: Record>(
    path: &Path,
    generation: u64,
    sequence: u64,
    rows: &BTreeMap<u64, R>,
    overhead: u64,
    options: MaintenanceOptions,
) -> Result<u64> {
    options.validate()?;
    options.check_rows(rows.len() as u64)?;
    options.check_bytes((HEADER_LEN as u64).saturating_add(overhead))?;
    boundary()?;
    let file = File::create_new(path)?;
    let mut writer = BufWriter::with_capacity(BUFFER_BYTES, &file);
    writer.write_all(&header::<R>(generation, sequence, rows.len() as u64))?;
    boundary()?;
    let mut encoder = options.encoder();
    let mut written = HEADER_LEN as u64;
    for (&key, row) in rows {
        encoder.clear();
        row.encode(&mut encoder)?;
        let encoded = encoder.as_slice();
        written = written
            .checked_add(encoded.len() as u64 + 16)
            .ok_or(Error::SequenceExhausted)?;
        options.check_bytes(written.saturating_add(overhead))?;
        let mut prefix = [0; 12];
        prefix[..8].copy_from_slice(&key.to_le_bytes());
        prefix[8..].copy_from_slice(&(encoded.len() as u32).to_le_bytes());
        let mut crc = Checksum::new();
        crc.update(&prefix);
        crc.update(encoded);
        writer.write_all(&prefix)?;
        // Partial-record fault injection is test-only. The production writer
        // never drains its buffer per row, and still syncs before publication.
        #[cfg(all(test, unix))]
        if crate::directory::faults::active() {
            let middle = encoded.len() / 2;
            writer.write_all(&encoded[..middle])?;
            writer.flush()?;
            boundary()?;
            writer.write_all(&encoded[middle..])?;
        } else {
            writer.write_all(encoded)?;
        }
        #[cfg(not(all(test, unix)))]
        writer.write_all(encoded)?;
        writer.write_all(&crc.finish().to_le_bytes())?;
    }
    writer.flush()?;
    boundary()?;
    file.sync_all()?;
    #[cfg(all(test, unix))]
    crate::persistence_model::file_synced(&file);
    boundary()?;
    Ok(written)
}

/// One production decoder serves reopen, backup and discard-as-you-go verification.
/// The caller chooses whether to retain a decoded row; no parallel parser exists.
pub(crate) fn visit<R: Record>(
    path: &Path,
    generation: u64,
    sequence: u64,
    count: u64,
    options: MaintenanceOptions,
    mut accept: impl FnMut(u64, R) -> Result<()>,
) -> Result<()> {
    options.check_rows(count)?;
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < HEADER_LEN as u64 || count > (len - HEADER_LEN as u64) / 16 {
        return Err(Error::corrupt(
            0,
            "truncated snapshot or impossible row count",
        ));
    }
    let mut reader = BufReader::with_capacity(BUFFER_BYTES, file);
    let mut actual = [0; HEADER_LEN];
    reader.read_exact(&mut actual)?;
    if actual.as_slice() != header::<R>(generation, sequence, count) {
        return Err(Error::corrupt(
            0,
            "snapshot does not match manifest or checksum",
        ));
    }
    let mut payload = Vec::new();
    let mut offset = HEADER_LEN as u64;
    let mut previous = None;
    for _ in 0..count {
        let mut prefix = [0; 12];
        reader.read_exact(&mut prefix)?;
        let key = u64::from_le_bytes(prefix[..8].try_into().unwrap());
        let size = u32::from_le_bytes(prefix[8..].try_into().unwrap()) as usize;
        if size > MAX_RECORD_BYTES || len.saturating_sub(offset) < size as u64 + 16 {
            return Err(Error::corrupt(offset, "invalid snapshot record length"));
        }
        if size > options.max_record_bytes {
            return Err(Error::LimitExceeded {
                limit: options.max_record_bytes,
            });
        }
        if previous.is_some_and(|previous| key <= previous) {
            return Err(Error::corrupt(
                offset,
                "snapshot keys are not strictly ordered",
            ));
        }
        payload.resize(size, 0);
        reader.read_exact(&mut payload)?;
        let mut actual_crc = [0; 4];
        reader.read_exact(&mut actual_crc)?;
        let mut crc = Checksum::new();
        crc.update(&prefix);
        crc.update(&payload);
        if crc.finish() != u32::from_le_bytes(actual_crc) {
            return Err(Error::corrupt(offset, "snapshot record checksum mismatch"));
        }
        let mut decoder = Decoder::new(&payload);
        let row =
            R::decode(&mut decoder).map_err(|error| Error::corrupt(offset, error.to_string()))?;
        decoder
            .finish()
            .map_err(|error| Error::corrupt(offset, error.to_string()))?;
        accept(key, row)?;
        previous = Some(key);
        offset += size as u64 + 16;
    }
    if offset != len {
        return Err(Error::corrupt(offset, "trailing snapshot bytes"));
    }
    Ok(())
}

pub(crate) fn read<R: Record>(
    path: &Path,
    generation: u64,
    sequence: u64,
    count: u64,
) -> Result<BTreeMap<u64, R>> {
    let mut rows = BTreeMap::new();
    visit::<R>(
        path,
        generation,
        sequence,
        count,
        MaintenanceOptions::default(),
        |key, row| {
            rows.insert(key, row);
            Ok(())
        },
    )?;
    Ok(rows)
}
