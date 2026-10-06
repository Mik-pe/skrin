//! Small, explicit little-endian codecs. Strings and bytes use `u32` lengths.

use crate::{Error, Result};

/// Maximum encoded size of an individual record (8 MiB).
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Bounded encoder for a single record. No native memory layout is persisted.
#[derive(Default)]
pub struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn reserve(&mut self, additional: usize) -> Result<()> {
        if self.bytes.len().saturating_add(additional) > MAX_RECORD_BYTES {
            return Err(Error::LimitExceeded {
                limit: MAX_RECORD_BYTES,
            });
        }
        self.bytes.reserve(additional);
        Ok(())
    }

    /// Encode one byte.
    pub fn u8(&mut self, value: u8) -> Result<()> {
        self.reserve(1)?;
        self.bytes.push(value);
        Ok(())
    }

    /// Encode a little-endian unsigned 32-bit integer.
    pub fn u32(&mut self, value: u32) -> Result<()> {
        self.reserve(4)?;
        self.bytes.extend_from_slice(&value.to_le_bytes());
        Ok(())
    }

    /// Encode a little-endian unsigned 64-bit integer.
    pub fn u64(&mut self, value: u64) -> Result<()> {
        self.reserve(8)?;
        self.bytes.extend_from_slice(&value.to_le_bytes());
        Ok(())
    }

    /// Encode a length-prefixed byte string.
    pub fn bytes(&mut self, value: &[u8]) -> Result<()> {
        self.reserve(value.len().saturating_add(4))?;
        self.bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    /// Encode length-prefixed UTF-8 text.
    pub fn string(&mut self, value: &str) -> Result<()> {
        self.bytes(value.as_bytes())
    }

    /// Finish encoding and take ownership of the encoded bytes.
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Bounds-checked decoder borrowing the input. Length fields never allocate.
pub struct Decoder<'a> {
    remaining: &'a [u8],
}

impl<'a> Decoder<'a> {
    /// Decode a byte slice. Call `finish` to reject unconsumed trailing data.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let Some(bytes) = self.remaining.get(..len) else {
            return Err(Error::Codec("truncated field".into()));
        };
        self.remaining = &self.remaining[len..];
        Ok(bytes)
    }

    /// Decode one byte.
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// Decode a little-endian unsigned 32-bit integer.
    pub fn u32(&mut self) -> Result<u32> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    /// Decode a little-endian unsigned 64-bit integer.
    pub fn u64(&mut self) -> Result<u64> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    /// Decode a length-prefixed byte string without allocating.
    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    /// Decode length-prefixed, validated UTF-8 text without allocating.
    pub fn string(&mut self) -> Result<&'a str> {
        std::str::from_utf8(self.bytes()?)
            .map_err(|error| Error::Codec(format!("invalid UTF-8: {error}")))
    }

    /// Reject an input that was not consumed in full.
    pub fn finish(self) -> Result<()> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(Error::Codec("trailing bytes".into()))
        }
    }
}
