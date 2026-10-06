use crate::codec::MAX_RECORD_BYTES;
use crate::{Decoder, Encoder, Error, Record, Result, Schema};
use std::collections::BTreeMap;
use std::fs::{File, TryLockError};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub(crate) const HEADER_LEN: usize = 32;
const FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"SKRIN\0\r\n";
const FRAME_MAGIC: &[u8; 4] = b"TXN1";
const END_MAGIC: &[u8; 4] = b"END!";
const FRAME_HEADER_LEN: usize = 24;
const FRAME_END_LEN: usize = 12;
/// Includes the operation count and all encoded operations, not frame headers.
pub(crate) const MAX_TRANSACTION_BYTES: usize = 16 * 1024 * 1024;

// The I/O seam is deliberately below the real codec/recovery implementation.
// Tests can inject short writes and sync failures without a second engine.
pub(crate) trait Storage: Read + Write + Seek + Send + Sync {
    fn size(&self) -> io::Result<u64>;
    fn truncate(&mut self, len: u64) -> io::Result<()>;
    fn sync(&self) -> io::Result<()>;
}

impl Storage for File {
    fn size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.set_len(len)
    }

    fn sync(&self) -> io::Result<()> {
        self.sync_all()
    }
}

pub(crate) struct Wal {
    io: Box<dyn Storage>,
    pub(crate) bytes: u64,
}

pub(crate) struct Recovered<R> {
    pub(crate) rows: BTreeMap<u64, R>,
    pub(crate) sequence: u64,
    pub(crate) discarded: u64,
}

fn lock(file: &File) -> Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(Error::Busy),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn supported_platform() -> Result<()> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Err(Error::UnsupportedPlatform)
}

impl Wal {
    pub(crate) fn create<R: Record>(path: &Path) -> Result<Self> {
        supported_platform()?;
        let mut file = File::create_new(path)?;
        lock(&file)?;
        file.write_all(&file_header(R::SCHEMA))?;
        file.sync_all()?;
        sync_parent(path)?;
        Ok(Self {
            io: Box::new(file),
            bytes: HEADER_LEN as u64,
        })
    }

    pub(crate) fn open<R: Record>(path: &Path) -> Result<(Self, Recovered<R>)> {
        supported_platform()?;
        let file = File::options().read(true).write(true).open(path)?;
        lock(&file)?;
        let result = Self::recover::<R>(Box::new(file))?;
        sync_parent(path)?;
        Ok(result)
    }

    pub(crate) fn recover<R: Record>(mut io: Box<dyn Storage>) -> Result<(Self, Recovered<R>)> {
        let (rows, sequence, valid_len) = replay::<R>(&mut *io)?;
        let original_len = io.size()?;
        if original_len != valid_len {
            io.truncate(valid_len)?;
        }
        // A complete frame may originate from an uncertain previous commit.
        // Sync it before exposing the recovered state, even without truncation.
        io.sync()?;
        io.seek(SeekFrom::Start(valid_len))?;
        Ok((
            Self {
                io,
                bytes: valid_len,
            },
            Recovered {
                rows,
                sequence,
                discarded: original_len - valid_len,
            },
        ))
    }

    pub(crate) fn append(&mut self, frame: &[u8]) -> io::Result<()> {
        let bytes = self
            .bytes
            .checked_add(frame.len() as u64)
            .ok_or_else(|| io::Error::other("log size exhausted"))?;
        self.io.write_all(frame)?;
        self.io.sync()?;
        self.bytes = bytes;
        Ok(())
    }
}

fn file_header(schema: Schema) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&schema.table_id.to_le_bytes());
    bytes.extend_from_slice(&schema.version.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
    bytes
}

fn check_file_header<R: Record>(bytes: &[u8; HEADER_LEN]) -> Result<()> {
    if &bytes[..8] != MAGIC || checksum(&bytes[..28]) != read_u32(&bytes[28..]) {
        return Err(Error::corrupt(0, "invalid file header or checksum"));
    }
    let version = read_u32(&bytes[8..]);
    if version != FORMAT_VERSION {
        return Err(Error::UnsupportedFormat(version));
    }
    if read_u32(&bytes[24..]) != 0 {
        return Err(Error::corrupt(24, "nonzero reserved header bits"));
    }
    let found = Schema {
        table_id: read_u64(&bytes[12..]),
        version: read_u32(&bytes[20..]),
    };
    if found != R::SCHEMA {
        return Err(Error::SchemaMismatch {
            expected: R::SCHEMA,
            found,
        });
    }
    Ok(())
}

pub(crate) fn encode_transaction<R: Record>(
    sequence: u64,
    changes: &BTreeMap<u64, Option<R>>,
) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    let count = u32::try_from(changes.len()).map_err(|_| Error::LimitExceeded {
        limit: MAX_TRANSACTION_BYTES,
    })?;
    payload.extend_from_slice(&count.to_le_bytes());
    for (&key, row) in changes {
        let encoded = if let Some(row) = row {
            let mut encoder = Encoder::default();
            row.encode(&mut encoder)?;
            Some(encoder.finish())
        } else {
            None
        };
        let extra = 9 + encoded.as_ref().map_or(0, |bytes| 4 + bytes.len());
        if payload.len().saturating_add(extra) > MAX_TRANSACTION_BYTES {
            return Err(Error::LimitExceeded {
                limit: MAX_TRANSACTION_BYTES,
            });
        }
        payload.push(if row.is_some() { 1 } else { 2 });
        payload.extend_from_slice(&key.to_le_bytes());
        if let Some(encoded) = encoded {
            payload.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            payload.extend_from_slice(&encoded);
        }
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len() + FRAME_END_LEN);
    frame.extend_from_slice(FRAME_MAGIC);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&checksum(&payload).to_le_bytes());
    frame.extend_from_slice(&checksum(&frame).to_le_bytes());
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(END_MAGIC);
    Ok(frame)
}

fn decode_transaction<R: Record>(payload: &[u8]) -> Result<BTreeMap<u64, Option<R>>> {
    let mut decoder = Decoder::new(payload);
    let count = decoder.u32()? as usize;
    if count == 0 || count > (payload.len() - 4) / 9 {
        return Err(Error::Codec("invalid operation count".into()));
    }
    let mut changes = BTreeMap::new();
    for _ in 0..count {
        let tag = decoder.u8()?;
        let key = decoder.u64()?;
        let row = match tag {
            1 => {
                let bytes = decoder.bytes()?;
                if bytes.len() > MAX_RECORD_BYTES {
                    return Err(Error::LimitExceeded {
                        limit: MAX_RECORD_BYTES,
                    });
                }
                let mut record = Decoder::new(bytes);
                let value = R::decode(&mut record)?;
                record.finish()?;
                Some(value)
            }
            2 => None,
            _ => return Err(Error::Codec("unknown operation".into())),
        };
        if changes.insert(key, row).is_some() {
            return Err(Error::Codec("duplicate key inside frame".into()));
        }
    }
    decoder.finish()?;
    Ok(changes)
}

fn replay<R: Record>(io: &mut dyn Storage) -> Result<(BTreeMap<u64, R>, u64, u64)> {
    let file_len = io.size()?;
    if file_len < HEADER_LEN as u64 {
        return Err(Error::corrupt(0, "truncated file header"));
    }
    io.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(io);
    let mut header = [0; HEADER_LEN];
    reader.read_exact(&mut header)?;
    check_file_header::<R>(&header)?;
    let mut rows = BTreeMap::new();
    let mut sequence = 0u64;
    let mut offset = HEADER_LEN as u64;
    while offset < file_len {
        let available = (file_len - offset).min(FRAME_HEADER_LEN as u64) as usize;
        let mut header = [0; FRAME_HEADER_LEN];
        reader.read_exact(&mut header[..available])?;
        let prefix_len = available.min(FRAME_MAGIC.len());
        if header[..prefix_len] != FRAME_MAGIC[..prefix_len] {
            return Err(Error::corrupt(offset, "invalid transaction magic"));
        }
        if available < FRAME_HEADER_LEN {
            break;
        }
        if checksum(&header[..20]) != read_u32(&header[20..]) {
            return Err(Error::corrupt(offset, "transaction header checksum mismatch"));
        }
        let payload_len = read_u32(&header[4..]) as usize;
        if !(4..=MAX_TRANSACTION_BYTES).contains(&payload_len) {
            return Err(Error::corrupt(offset, "invalid transaction length"));
        }
        let next = sequence.checked_add(1).ok_or(Error::SequenceExhausted)?;
        if read_u64(&header[8..]) != next {
            return Err(Error::corrupt(offset, "nonconsecutive transaction sequence"));
        }
        let payload_start = offset + FRAME_HEADER_LEN as u64;
        if file_len - payload_start < payload_len as u64 {
            break;
        }
        let mut payload = vec![0; payload_len];
        reader.read_exact(&mut payload)?;
        if checksum(&payload) != read_u32(&header[16..]) {
            return Err(Error::corrupt(offset, "transaction payload checksum mismatch"));
        }
        let end_start = payload_start + payload_len as u64;
        let available = (file_len - end_start).min(FRAME_END_LEN as u64) as usize;
        let mut end = [0; FRAME_END_LEN];
        reader.read_exact(&mut end[..available])?;
        let mut expected = [0; FRAME_END_LEN];
        expected[..8].copy_from_slice(&next.to_le_bytes());
        expected[8..].copy_from_slice(END_MAGIC);
        if end[..available] != expected[..available] {
            return Err(Error::corrupt(offset, "invalid transaction trailer"));
        }
        if available < FRAME_END_LEN {
            break;
        }
        let changes = decode_transaction::<R>(&payload)
            .map_err(|error| Error::corrupt(offset, error.to_string()))?;
        for (key, row) in changes {
            match row {
                Some(row) => {
                    rows.insert(key, row);
                }
                None => {
                    rows.remove(&key);
                }
            }
        }
        sequence = next;
        offset = end_start + FRAME_END_LEN as u64;
    }
    Ok((rows, sequence, offset))
}

fn read_u32(bytes: &[u8]) -> u32 {
    let mut value = [0; 4];
    value.copy_from_slice(&bytes[..4]);
    u32::from_le_bytes(value)
}

fn read_u64(bytes: &[u8]) -> u64 {
    let mut value = [0; 8];
    value.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(value)
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ 0xedb8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

const CRC_TABLE: [u32; 256] = crc_table();

fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc = CRC_TABLE[((crc as u8) ^ byte) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_standard_vectors() {
        assert_eq!(checksum(b""), 0);
        assert_eq!(checksum(b"123456789"), 0xcbf4_3926);
        assert_eq!(checksum(b"The quick brown fox jumps over the lazy dog"), 0x414f_a339);
    }
}
