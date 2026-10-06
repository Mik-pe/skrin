//! Sealed, bounded, key-ordered snapshots. Unlike WAL tails, no truncation is safe.
use crate::codec::MAX_RECORD_BYTES;
use crate::directory::boundary;
use crate::log::checksum;
use crate::{Decoder, Encoder, Error, Record, Result};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const HEADER_LEN: usize = 48;

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

pub(crate) fn write<R: Record>(
    path: &Path,
    generation: u64,
    sequence: u64,
    rows: &BTreeMap<u64, R>,
) -> Result<u64> {
    boundary()?;
    let file = File::create_new(path)?;
    let mut writer = BufWriter::new(&file);
    writer.write_all(&header::<R>(generation, sequence, rows.len() as u64))?;
    boundary()?;
    for (&key, row) in rows {
        let mut encoder = Encoder::default();
        row.encode(&mut encoder)?;
        let encoded = encoder.finish();
        let mut frame = Vec::with_capacity(16 + encoded.len());
        frame.extend_from_slice(&key.to_le_bytes());
        frame.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        frame.extend_from_slice(&encoded);
        frame.extend_from_slice(&checksum(&frame).to_le_bytes());
        // Both halves pass through the real file writer; fault tests can leave
        // an incomplete record without a second, mock snapshot implementation.
        let middle = frame.len() / 2;
        writer.write_all(&frame[..middle])?;
        writer.flush()?;
        boundary()?;
        writer.write_all(&frame[middle..])?;
    }
    writer.flush()?;
    boundary()?;
    file.sync_all()?;
    boundary()?;
    Ok(file.metadata()?.len())
}

pub(crate) fn read<R: Record>(
    path: &Path,
    generation: u64,
    sequence: u64,
    count: u64,
) -> Result<BTreeMap<u64, R>> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < HEADER_LEN as u64 || count > (len - HEADER_LEN as u64) / 16 {
        return Err(Error::corrupt(
            0,
            "truncated snapshot or impossible row count",
        ));
    }
    let mut reader = BufReader::new(file);
    let mut actual = [0; HEADER_LEN];
    reader.read_exact(&mut actual)?;
    if actual.as_slice() != header::<R>(generation, sequence, count) {
        return Err(Error::corrupt(
            0,
            "snapshot does not match manifest or checksum",
        ));
    }
    let mut rows = BTreeMap::new();
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
        if previous.is_some_and(|previous| key <= previous) {
            return Err(Error::corrupt(
                offset,
                "snapshot keys are not strictly ordered",
            ));
        }
        let mut frame = Vec::with_capacity(12 + size);
        frame.extend_from_slice(&prefix);
        frame.resize(12 + size, 0);
        reader.read_exact(&mut frame[12..])?;
        let mut crc = [0; 4];
        reader.read_exact(&mut crc)?;
        if checksum(&frame) != u32::from_le_bytes(crc) {
            return Err(Error::corrupt(offset, "snapshot record checksum mismatch"));
        }
        let mut decoder = Decoder::new(&frame[12..]);
        let row =
            R::decode(&mut decoder).map_err(|error| Error::corrupt(offset, error.to_string()))?;
        decoder
            .finish()
            .map_err(|error| Error::corrupt(offset, error.to_string()))?;
        rows.insert(key, row);
        previous = Some(key);
        offset += size as u64 + 16;
    }
    if offset != len {
        return Err(Error::corrupt(offset, "trailing snapshot bytes"));
    }
    Ok(rows)
}
