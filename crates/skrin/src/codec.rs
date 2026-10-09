//! Small, explicit little-endian codecs. Strings and bytes use `u32` lengths.

use crate::{Error, Result};

/// Maximum encoded size of an individual record (8 MiB).
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Bounded encoder for a single record. No native memory layout is persisted.
pub struct Encoder {
    bytes: Vec<u8>,
    limit: usize,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::with_limit(MAX_RECORD_BYTES)
    }
}

macro_rules! integer_encoder {
    ($($ty:ident),+ $(,)?) => {$(
        #[doc = concat!("Encode a fixed-width little-endian `", stringify!($ty), "`. Signed values use two's complement.")]
        pub fn $ty(&mut self, value: $ty) -> Result<()> {
            let bytes = value.to_le_bytes();
            self.reserve(bytes.len())?;
            self.bytes.extend_from_slice(&bytes);
            Ok(())
        }
    )+};
}
macro_rules! integer_decoder {
    ($($ty:ident: $width:literal),+ $(,)?) => {$(
        #[doc = concat!("Decode a fixed-width little-endian `", stringify!($ty), "`. Signed values use two's complement.")]
        pub fn $ty(&mut self) -> Result<$ty> {
            let mut bytes = [0; $width];
            bytes.copy_from_slice(self.take($width)?);
            Ok(<$ty>::from_le_bytes(bytes))
        }
    )+};
}

impl Encoder {
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.bytes.clear();
    }
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    fn reserve(&mut self, additional: usize) -> Result<()> {
        if self.bytes.len().saturating_add(additional) > self.limit {
            return Err(Error::LimitExceeded { limit: self.limit });
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

    integer_encoder!(u16, u128, i8, i16, i32, i64, i128);

    /// Encode a boolean as exactly 0 (false) or 1 (true).
    pub fn bool(&mut self, value: bool) -> Result<()> {
        self.u8(u8::from(value))
    }

    /// Encode the exact IEEE-754 binary32 bits in little-endian order. Preserves
    /// signed zero, infinities and NaN payloads; no application range check.
    pub fn f32(&mut self, value: f32) -> Result<()> {
        self.u32(value.to_bits())
    }

    /// Encode the exact IEEE-754 binary64 bits in little-endian order. Preserves
    /// signed zero, infinities and NaN payloads; no application range check.
    pub fn f64(&mut self, value: f64) -> Result<()> {
        self.u64(value.to_bits())
    }

    /// Encode 0 for None, or 1 followed by the supplied codec for Some. A codec
    /// error leaves partial encoder bytes; the engine discards them before I/O.
    pub fn option<T>(
        &mut self,
        value: Option<&T>,
        encode: impl FnOnce(&mut Self, &T) -> Result<()>,
    ) -> Result<()> {
        match value {
            None => self.u8(0),
            Some(value) => {
                self.u8(1)?;
                encode(self, value)
            }
        }
    }

    /// Encode a length-prefixed byte string.
    pub fn bytes(&mut self, value: &[u8]) -> Result<()> {
        self.reserve(value.len().saturating_add(4))?;
        self.bytes
            .extend_from_slice(&(value.len() as u32).to_le_bytes());
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

    integer_decoder!(u16: 2, u128: 16, i8: 1, i16: 2, i32: 4, i64: 8, i128: 16);

    /// Decode a boolean, rejecting tags other than 0 and 1.
    pub fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Codec("invalid boolean tag".into())),
        }
    }

    /// Decode exact IEEE-754 binary32 bits, including NaN payloads.
    pub fn f32(&mut self) -> Result<f32> {
        self.u32().map(f32::from_bits)
    }

    /// Decode exact IEEE-754 binary64 bits, including NaN payloads.
    pub fn f64(&mut self) -> Result<f64> {
        self.u64().map(f64::from_bits)
    }

    /// Decode 0 as None, or 1 followed by the supplied codec as Some. Unknown
    /// tags and truncated Some values are errors, never implicit defaults.
    pub fn option<T>(&mut self, decode: impl FnOnce(&mut Self) -> Result<T>) -> Result<Option<T>> {
        match self.u8()? {
            0 => Ok(None),
            1 => decode(self).map(Some),
            _ => Err(Error::Codec("invalid option tag".into())),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_width_integer_extremes_have_exact_bounded_bytes() -> Result<()> {
        macro_rules! check {
            ($ty:ident, $n:expr, $bytes:expr) => {{
                let expected = $bytes;
                let mut e = Encoder::default();
                e.$ty($n)?;
                assert_eq!(e.finish(), expected);
                let mut d = Decoder::new(&expected);
                assert_eq!(d.$ty()?, $n);
                d.finish()?;
                for len in 0..expected.len() {
                    assert!(Decoder::new(&expected[..len]).$ty().is_err());
                    let mut bounded = Encoder::with_limit(len);
                    assert!(matches!(bounded.$ty($n), Err(Error::LimitExceeded { .. })));
                    assert!(bounded.finish().is_empty());
                }
            }};
        }
        check!(u16, 0x1234, [0x34, 0x12]);
        check!(u128, u128::MAX, [255; 16]);
        check!(i8, i8::MIN, [128]);
        check!(i8, i8::MAX, [127]);
        check!(i16, i16::MIN, [0, 128]);
        check!(i16, i16::MAX, [255, 127]);
        check!(i32, i32::MIN, [0, 0, 0, 128]);
        check!(i32, i32::MAX, [255, 255, 255, 127]);
        check!(i64, i64::MIN, [0, 0, 0, 0, 0, 0, 0, 128]);
        check!(i64, i64::MAX, [255, 255, 255, 255, 255, 255, 255, 127]);
        check!(
            i128,
            i128::MIN,
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128]
        );
        check!(
            i128,
            i128::MAX,
            [
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 127
            ]
        );
        Ok(())
    }
    #[test]
    fn float_codecs_preserve_zero_infinity_nan_and_subnormal_bits() -> Result<()> {
        for bits in [
            0u32, 0x80000000, 0x3f800000, 0xbf800000, 1, 0x807fffff, 0x7f7fffff, 0xff7fffff,
            0x7f800000, 0xff800000, 0x7fc00042, 0x7f800001, 0xff800001,
        ] {
            let expected = [
                (bits & 255) as u8,
                ((bits >> 8) & 255) as u8,
                ((bits >> 16) & 255) as u8,
                ((bits >> 24) & 255) as u8,
            ];
            let mut e = Encoder::default();
            e.f32(f32::from_bits(bits))?;
            assert_eq!(e.finish(), expected);
            assert_eq!(Decoder::new(&expected).f32()?.to_bits(), bits);
        }
        for bits in [
            0u64,
            0x8000000000000000,
            0x3ff0000000000000,
            0xbff0000000000000,
            1,
            0x800fffffffffffff,
            0x7fefffffffffffff,
            0xffefffffffffffff,
            0x7ff0000000000000,
            0xfff0000000000000,
            0x7ff8000000000042,
            0x7ff0000000000001,
            0xfff0000000000001,
        ] {
            let expected = std::array::from_fn::<_, 8, _>(|i| ((bits >> (i * 8)) & 255) as u8);
            let mut e = Encoder::default();
            e.f64(f64::from_bits(bits))?;
            assert_eq!(e.finish(), expected);
            assert_eq!(Decoder::new(&expected).f64()?.to_bits(), bits);
        }
        assert!(matches!(
            Encoder::with_limit(3).f32(0.0),
            Err(Error::LimitExceeded { .. })
        ));
        assert!(matches!(
            Encoder::with_limit(7).f64(0.0),
            Err(Error::LimitExceeded { .. })
        ));
        for len in 0..8 {
            assert!(Decoder::new(&[0; 8][..len]).f64().is_err());
        }
        for len in 0..4 {
            assert!(Decoder::new(&[0; 4][..len]).f32().is_err());
        }
        Ok(())
    }
    #[test]
    fn bool_and_option_tags_are_strict_and_never_default_missing_values() -> Result<()> {
        let mut e = Encoder::default();
        e.bool(false)?;
        e.bool(true)?;
        assert_eq!(e.finish(), [0, 1]);
        for tag in 2..=255 {
            assert!(Decoder::new(&[tag]).bool().is_err());
            assert!(
                Decoder::new(&[tag])
                    .option::<u8>(|_| panic!("invalid tag invoked codec"))
                    .is_err()
            );
        }
        assert_eq!(
            Decoder::new(&[0]).option::<u8>(|_| panic!("None invoked codec"))?,
            None
        );
        assert_eq!(Decoder::new(&[1, 42]).option(Decoder::u8)?, Some(42));
        assert!(Decoder::new(&[1]).option(Decoder::u8).is_err());
        assert!(Decoder::new(&[]).option(Decoder::u8).is_err());
        assert!(Decoder::new(&[]).bool().is_err());
        assert!(Encoder::with_limit(0).bool(false).is_err());
        let mut e = Encoder::with_limit(1);
        assert!(matches!(
            e.option(Some(&1u64), |e, n| e.u64(*n)),
            Err(Error::LimitExceeded { .. })
        ));
        assert_eq!(e.finish(), [1]); // Documented partial encoder, never a committed row.
        Ok(())
    }
    #[test]
    fn complete_checksummed_noncanonical_tags_prevent_production_tail_repair() {
        use crate::log::{Wal, checksum, encode_transaction, file_header};
        use crate::test_support::TestStorage;
        use crate::{Record, Schema};
        struct Tagged {
            enabled: bool,
            owner: Option<u16>,
        }
        impl Record for Tagged {
            const SCHEMA: Schema = Schema {
                table_id: 125,
                version: 1,
            };
            fn encode(&self, e: &mut Encoder) -> Result<()> {
                e.bool(self.enabled)?;
                e.option(self.owner.as_ref(), |e, n| e.u16(*n))
            }
            fn decode(d: &mut Decoder<'_>) -> Result<Self> {
                Ok(Self {
                    enabled: d.bool()?,
                    owner: d.option(Decoder::u16)?,
                })
            }
        }
        let original = encode_transaction(
            1,
            &std::collections::BTreeMap::from([(
                42,
                Some(Tagged {
                    enabled: true,
                    owner: Some(7),
                }),
            )]),
        )
        .unwrap();
        for offset in [41, 42] {
            // 24-byte frame header + 17-byte operation envelope.
            let mut frame = original.clone();
            frame[offset] = 255;
            let payload_end = frame.len() - 12;
            let crc = checksum(&frame[24..payload_end]);
            frame[16..20].copy_from_slice(&crc.to_le_bytes());
            let crc = checksum(&frame[..20]);
            frame[20..24].copy_from_slice(&crc.to_le_bytes());
            let mut image = file_header(Tagged::SCHEMA).to_vec();
            image.extend(frame);
            image.extend(b"TXN1");
            let disk = TestStorage::new(image.clone());
            assert!(matches!(
                Wal::recover::<Tagged>(Box::new(disk.clone())),
                Err(Error::Corrupt { offset: 32, reason }) if reason.contains(if offset == 41 { "invalid boolean tag" } else { "invalid option tag" })
            ));
            assert_eq!(disk.image(), image);
        }
    }
}
