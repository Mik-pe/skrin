use crate::catalog::{self, Catalog, IndexDefinition, Table};
use crate::{Error, Result};
use std::ops::{Bound, RangeBounds};

/// Explicit ordered encoding shared by index projection and native queries.
///
/// Bytes must preserve the key's intended order lexicographically and equality
/// must have one canonical representation. Implementations must be pure and
/// deterministic. Changing encoding or meaning requires an index version and
/// catalog migration. These are index bytes, independent of record codecs.
/// Built-in unsigned integers use fixed-width big endian; signed integers flip
/// the high sign bit of their big-endian two's-complement representation. Bool
/// uses 0/1. Floating-point keys require an explicit application codec/collation.
/// `str` uses raw UTF-8
/// (case-sensitive byte order) and `[u8]` uses its unchanged bytes.
/// Pairs order first by the first component, then by the second. Their encoding
/// escapes each zero in the first component as `00 ff`, terminates that component
/// with `00 00`, and appends the second component unchanged. This preserves
/// variable-length prefix order and keeps distinct component boundaries unique.
pub trait IndexKey {
    /// Encode one key; encoded keys may not exceed the record-byte limit.
    fn encode_key(&self) -> Result<Vec<u8>>;
}

macro_rules! unsigned_key {
    ($($ty:ty),+) => {$(
        impl IndexKey for $ty {
            fn encode_key(&self) -> Result<Vec<u8>> {
                Ok(self.to_be_bytes().to_vec())
            }
        }
    )+};
}
unsigned_key!(u8, u16, u32, u64, u128);
macro_rules! signed_key {
    ($($ty:ty),+) => {$(
        impl IndexKey for $ty {
            fn encode_key(&self) -> Result<Vec<u8>> {
                let mut bytes = self.to_be_bytes();
                bytes[0] ^= 0x80;
                Ok(bytes.to_vec())
            }
        }
    )+};
}
signed_key!(i8, i16, i32, i64, i128);
impl IndexKey for bool {
    fn encode_key(&self) -> Result<Vec<u8>> {
        Ok(vec![u8::from(*self)])
    }
}
impl IndexKey for str {
    fn encode_key(&self) -> Result<Vec<u8>> {
        self.as_bytes().encode_key()
    }
}
impl IndexKey for [u8] {
    fn encode_key(&self) -> Result<Vec<u8>> {
        check_size(self.len())?;
        Ok(self.to_vec())
    }
}

impl<A: IndexKey, B: IndexKey> IndexKey for (A, B) {
    fn encode_key(&self) -> Result<Vec<u8>> {
        let first = encode(&self.0)?;
        let second = encode(&self.1)?;
        let size = first
            .len()
            .saturating_add(first.iter().filter(|&&byte| byte == 0).count())
            .saturating_add(2)
            .saturating_add(second.len());
        check_size(size)?;
        let mut bytes = Vec::with_capacity(size);
        for byte in first {
            bytes.push(byte);
            if byte == 0 {
                bytes.push(255);
            }
        }
        bytes.extend([0, 0]);
        bytes.extend(second);
        Ok(bytes)
    }
}

/// Schema-owned index marker binding its table, native key and full definition.
///
/// `catalog!` generates these markers from typed projections. Manual catalogs
/// can implement this trait, delegating `Catalog::index_key` to `project` so
/// queries and stored projections use the same `Key::encode_key` contract.
/// Runtime calls also verify the table schema, version and uniqueness. A trait
/// cannot detect an application changing projection semantics without a version.
///
/// Key types are checked before a query can run:
/// ```compile_fail
/// struct Player { score: u64 }
/// impl skrin::Record for Player {
///     const SCHEMA: skrin::Schema = skrin::Schema { table_id: 1, version: 1 };
///     fn encode(&self, e: &mut skrin::Encoder) -> skrin::Result<()> { e.u64(self.score) }
///     fn decode(d: &mut skrin::Decoder<'_>) -> skrin::Result<Self> { Ok(Self { score: d.u64()? }) }
/// }
/// skrin::catalog! {
///     Game, Row {
///         schema: (100, 1), tables: { Players: Player },
///         indexes: { ByScore: Players {
///             id: 1, version: 1, unique: false, key: u64 => |p| p.score
///         } }
///     }
/// }
/// let db = skrin::catalog::CatalogDatabase::<Game>::in_memory().unwrap();
/// let read = db.read().unwrap();
/// read.matching(ByScore, "wrong key type");
/// ```
///
/// A marker is bound to its own catalog:
/// ```compile_fail
/// struct Player { score: u64 }
/// impl skrin::Record for Player {
///     const SCHEMA: skrin::Schema = skrin::Schema { table_id: 1, version: 1 };
///     fn encode(&self, e: &mut skrin::Encoder) -> skrin::Result<()> { e.u64(self.score) }
///     fn decode(d: &mut skrin::Decoder<'_>) -> skrin::Result<Self> { Ok(Self { score: d.u64()? }) }
/// }
/// skrin::catalog! {
///     Game, Row {
///         schema: (100, 1), tables: { Players: Player },
///         indexes: { ByScore: Players {
///             id: 1, version: 1, unique: false, key: u64 => |p| p.score
///         } }
///     }
/// }
/// skrin::catalog! {
///     OtherGame, OtherRow {
///         schema: (101, 1), tables: { OtherPlayers: Player }, indexes: {}
///     }
/// }
/// let db = skrin::catalog::CatalogDatabase::<OtherGame>::in_memory().unwrap();
/// let read = db.read().unwrap();
/// read.query(ByScore, ..);
/// ```
pub trait Index<C: Catalog> {
    /// Declared table whose typed records are returned.
    type Table: Table<C>;
    /// Native key; unsized string and byte keys can be borrowed directly.
    type Key: IndexKey + ?Sized;
    /// Persistent identity and semantics, exactly matching the catalog.
    const DEFINITION: IndexDefinition;
    /// Pure row projection using this index's key encoding.
    fn project(record: &<Self::Table as Table<C>>::Record) -> Result<Vec<u8>>;
}

pub(crate) fn validate<C: Catalog, I: Index<C>>() -> Result<()> {
    let definition = catalog::index::<C, I::Table>(I::DEFINITION.id)?;
    if *definition != I::DEFINITION {
        return Err(Error::InvalidOperation(
            "typed index definition differs from catalog".into(),
        ));
    }
    Ok(())
}
fn check_size(len: usize) -> Result<()> {
    if len > crate::codec::MAX_RECORD_BYTES {
        Err(Error::LimitExceeded {
            limit: crate::codec::MAX_RECORD_BYTES,
        })
    } else {
        Ok(())
    }
}
fn encode<K: IndexKey + ?Sized>(key: &K) -> Result<Vec<u8>> {
    let bytes = key.encode_key()?;
    check_size(bytes.len())?;
    Ok(bytes)
}
pub(crate) type KeyBounds = (Bound<Vec<u8>>, Bound<Vec<u8>>);
pub(crate) fn bounds<K: IndexKey + ?Sized>(range: impl RangeBounds<K>) -> Result<KeyBounds> {
    let encode_bound = |bound| match bound {
        Bound::Unbounded => Ok(Bound::Unbounded),
        Bound::Included(key) => encode(key).map(Bound::Included),
        Bound::Excluded(key) => encode(key).map(Bound::Excluded),
    };
    let (start, end) = (
        encode_bound(range.start_bound())?,
        encode_bound(range.end_bound())?,
    );
    if let (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) =
        (&start, &end)
        && (a > b || (a == b && matches!((&start, &end), (Bound::Excluded(_), Bound::Excluded(_)))))
    {
        return Err(Error::InvalidOperation("invalid typed index range".into()));
    }
    Ok((start, end))
}
pub(crate) struct Resume {
    pub(crate) keys: KeyBounds,
    // Only the first included key needs its posting seeked after this ID.
    pub(crate) after: Option<u64>,
    pub(crate) empty: bool,
}
pub(crate) fn resume<K: IndexKey + ?Sized>(
    range: impl RangeBounds<K>,
    cursor: (&K, u64),
) -> Result<Resume> {
    let mut keys = bounds(range)?;
    let key = encode(cursor.0)?;
    let empty = match &keys.1 {
        Bound::Unbounded => false,
        Bound::Included(end) => key > *end,
        Bound::Excluded(end) => key >= *end,
    };
    let before = match &keys.0 {
        Bound::Unbounded => false,
        Bound::Included(start) => key < *start,
        Bound::Excluded(start) => key <= *start,
    };
    let after = if empty || before {
        None
    } else {
        keys.0 = Bound::Included(key);
        Some(cursor.1)
    };
    Ok(Resume { keys, after, empty })
}
pub(crate) struct EqualKey(Vec<u8>);
impl EqualKey {
    pub(crate) fn new<K: IndexKey + ?Sized>(key: &K) -> Result<Self> {
        encode(key).map(Self)
    }
}
impl RangeBounds<Vec<u8>> for EqualKey {
    fn start_bound(&self) -> Bound<&Vec<u8>> {
        Bound::Included(&self.0)
    }
    fn end_bound(&self) -> Bound<&Vec<u8>> {
        Bound::Included(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct Bytes(Vec<u8>);
    impl IndexKey for Bytes {
        fn encode_key(&self) -> Result<Vec<u8>> {
            self.0.as_slice().encode_key()
        }
    }
    #[test]
    fn pairs_preserve_component_prefixes_zeroes_and_signed_order() -> Result<()> {
        assert_eq!((0u8, 7u8).encode_key()?, [0, 255, 0, 0, 7]);
        assert_eq!((1u8, 7u8).encode_key()?, [1, 0, 0, 7]);
        assert_eq!(
            (Bytes(vec![0, 1, 0]), Bytes(vec![0, 255])).encode_key()?,
            [0, 255, 1, 0, 255, 0, 0, 0, 255]
        );
        let first = [
            vec![],
            vec![0],
            vec![0, 0],
            vec![0, 1],
            vec![1],
            vec![1, 0],
            vec![255],
            vec![255, 0],
        ];
        let mut pairs = Vec::new();
        for a in first {
            for b in [i64::MIN, -1, 0, 1, i64::MAX] {
                pairs.push((Bytes(a.clone()), b));
            }
        }
        pairs.sort();
        let bytes: Vec<_> = pairs
            .iter()
            .map(IndexKey::encode_key)
            .collect::<Result<_>>()?;
        assert!(bytes.windows(2).all(|w| w[0] < w[1]));
        // Recursion also preserves order; equal first components defer to second.
        assert!(((0u8, 0u8), i64::MIN).encode_key()? < ((0u8, 0u8), i64::MAX).encode_key()?);
        Ok(())
    }
    struct Refused;
    impl IndexKey for Refused {
        fn encode_key(&self) -> Result<Vec<u8>> {
            Err(Error::Codec("refused component".into()))
        }
    }
    #[test]
    fn pair_limits_cover_components_escaping_and_terminator() -> Result<()> {
        let max = crate::codec::MAX_RECORD_BYTES;
        for pair in [
            (Bytes(vec![1; max - 2]), Bytes(vec![])),
            (Bytes(vec![]), Bytes(vec![1; max - 2])),
        ] {
            assert_eq!(pair.encode_key()?.len(), max);
        }
        for pair in [
            (Bytes(vec![1; max - 1]), Bytes(vec![])),
            (Bytes(vec![0; max / 2]), Bytes(vec![])),
            (Bytes(vec![]), Bytes(vec![1; max + 1])),
        ] {
            assert!(matches!(pair.encode_key(),Err(Error::LimitExceeded { limit }) if limit==max));
        }
        assert!(matches!((Refused, 0u8).encode_key(), Err(Error::Codec(_))));
        assert!(matches!((0u8, Refused).encode_key(), Err(Error::Codec(_))));
        Ok(())
    }
    #[test]
    fn signed_key_extremes_sort_numerically_and_have_canonical_bytes() -> Result<()> {
        macro_rules! check {
            ($ty:ident, $width:literal) => {{
                let values = [<$ty>::MIN, -1, 0, 1, <$ty>::MAX];
                let encoded: Vec<_> = values
                    .iter()
                    .map(IndexKey::encode_key)
                    .collect::<Result<_>>()?;
                assert!(encoded.windows(2).all(|w| w[0] < w[1]));
                assert_eq!(encoded[0], vec![0; $width]);
                assert_eq!(encoded[4], vec![255; $width]);
                let mut zero = vec![0; $width];
                zero[0] = 128;
                assert_eq!(encoded[2], zero);
            }};
        }
        check!(i8, 1);
        check!(i16, 2);
        check!(i32, 4);
        check!(i64, 8);
        check!(i128, 16);
        assert_eq!(false.encode_key()?, [0]);
        assert_eq!(true.encode_key()?, [1]);
        assert_eq!(u16::MAX.encode_key()?, [255, 255]);
        assert_eq!(u128::MAX.encode_key()?, [255; 16]);
        Ok(())
    }
}
