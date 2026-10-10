//! Immutable covering index: small contiguous leaves behind a path-copy tree.
//! Editing one entry copies at most one bounded leaf, never a whole posting.
use crate::version_tree::{VersionIter, VersionTree};
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

const LEAF_ENTRIES: usize = 64;
type Entry<K, V> = (K, V, u64);
struct Leaf<K, V> {
    entries: Vec<Entry<K, V>>,
}
type Leaves<K, V> = VersionTree<K, Arc<Leaf<K, V>>>;
type LeafEntry<K, V> = (K, Arc<Leaf<K, V>>, u64);
pub(crate) struct VersionIndex<K, V> {
    leaves: Leaves<K, V>,
}
impl<K, V> Clone for VersionIndex<K, V> {
    fn clone(&self) -> Self {
        Self {
            leaves: self.leaves.clone(),
        }
    }
}
impl<K: Ord + Clone, V> VersionIndex<K, V> {
    /// The supplied weight covers each entry's external key allocation. Inline
    /// entries, leaf capacity, Arc counters and routing nodes are added here.
    /// Shared row allocations are accounted by the coherent primary row tree.
    pub(crate) fn try_from_sorted(
        len: usize,
        mut entries: impl Iterator<Item = Result<Entry<K, V>>>,
    ) -> Result<Self> {
        let mut remaining = len;
        let mut leaves = std::iter::from_fn(|| {
            if remaining == 0 {
                return None;
            }
            let count = remaining.min(LEAF_ENTRIES);
            remaining -= count;
            let mut leaf = Vec::with_capacity(count);
            for _ in 0..count {
                match entries.next().expect("declared sorted index length") {
                    Ok(entry) => leaf.push(entry),
                    Err(error) => return Some(Err(error)),
                }
            }
            Some(Self::leaf(leaf))
        });
        let tree = VersionTree::try_from_sorted(len.div_ceil(LEAF_ENTRIES), &mut leaves)?;
        assert!(entries.next().is_none(), "declared sorted index length");
        Ok(Self { leaves: tree })
    }
    fn leaf(entries: Vec<Entry<K, V>>) -> Result<LeafEntry<K, V>> {
        assert!(!entries.is_empty() && entries.len() <= LEAF_ENTRIES);
        let key = entries[0].0.clone();
        let capacity = entries
            .capacity()
            .checked_mul(std::mem::size_of::<Entry<K, V>>());
        let bytes = capacity
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Leaf<K, V>>()))
            .and_then(|bytes| bytes.checked_add(2 * std::mem::size_of::<usize>()))
            .map(|bytes| bytes as u64)
            .and_then(|bytes| bytes.checked_add(Leaves::<K, V>::node_bytes()))
            // The routing key shares its buffer but is conservatively counted.
            .and_then(|bytes| bytes.checked_add(entries[0].2))
            .and_then(|bytes| entries.iter().try_fold(bytes, |n, e| n.checked_add(e.2)))
            .ok_or_else(|| Error::InvalidOperation("covering index footprint overflow".into()))?;
        Ok((key, Arc::new(Leaf { entries }), bytes))
    }
    pub(crate) fn accounted_bytes(&self) -> u64 {
        self.leaves.accounted_bytes()
    }
    pub(crate) fn range<R: RangeBounds<K>>(&self, bounds: R) -> IndexIter<'_, K, V, R> {
        let (start, end) = (bounds.start_bound(), bounds.end_bound());
        if let (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) =
            (start, end)
        {
            assert!(a <= b, "range start exceeds range end");
            assert!(
                a != b || !matches!((start, end), (Bound::Excluded(_), Bound::Excluded(_))),
                "equal excluded range bounds"
            );
        }
        let (first, mut leaves) = match start {
            Bound::Unbounded => (None, self.leaves.range(..)),
            Bound::Included(key) | Bound::Excluded(key) => self.leaves.range_from_floor(key),
        };
        let entries = first.or_else(|| leaves.next()).map(|(_, leaf)| {
            let at = leaf.entries.partition_point(|(key, _, _)| match start {
                Bound::Unbounded => false,
                Bound::Included(start) => key < start,
                Bound::Excluded(start) => key <= start,
            });
            let end = end_offset(&leaf.entries, bounds.end_bound());
            leaf.entries[at.min(end)..end].iter()
        });
        IndexIter {
            leaves,
            entries,
            bounds,
            finished: false,
        }
    }
}
impl<K: Ord + Clone, V: Clone> VersionIndex<K, V> {
    /// Coalesce a transaction's changes by original leaf. Each touched leaf is
    /// copied once, even when many replacements retain their index keys.
    pub(crate) fn changed(
        &self,
        removed: impl IntoIterator<Item = K>,
        added: impl IntoIterator<Item = Entry<K, V>>,
    ) -> Result<Self> {
        let mut edits = BTreeMap::new();
        for key in removed {
            edits.insert(key, None);
        }
        for (key, value, bytes) in added {
            edits.insert(key, Some((value, bytes)));
        }
        if edits.is_empty() {
            return Ok(self.clone());
        }
        let Some((first, _)) = self.leaves.range(..).next() else {
            let entries: Vec<_> = edits
                .into_iter()
                .filter_map(|(key, value)| value.map(|(value, bytes)| Ok((key, value, bytes))))
                .collect();
            return Self::try_from_sorted(entries.len(), entries.into_iter());
        };
        let mut buckets: BTreeMap<K, Vec<_>> = BTreeMap::new();
        for (key, value) in edits {
            let source = self.leaves.floor(&key).map_or(first, |(first, _)| first);
            buckets
                .entry(source.clone())
                .or_default()
                .push((key, value));
        }
        let mut leaves = self.leaves.clone();
        let mut replacements = Vec::new();
        for (first, edits) in buckets {
            let source = self.leaves.get(&first).expect("original leaf");
            let mut remaining = source.entries.len();
            for (key, value) in &edits {
                let present = source
                    .entries
                    .binary_search_by(|entry| entry.0.cmp(key))
                    .is_ok();
                match (present, value.is_some()) {
                    (false, true) => {
                        remaining = remaining.checked_add(1).ok_or_else(|| {
                            Error::InvalidOperation("index row count overflow".into())
                        })?
                    }
                    (true, false) => remaining -= 1,
                    _ => {}
                }
            }
            let mut old = source.entries.iter().peekable();
            let mut edits = edits.into_iter().peekable();
            let mut merged = std::iter::from_fn(|| {
                loop {
                    match (old.peek(), edits.peek()) {
                        (None, None) => return None,
                        (Some(_), None) => return old.next().cloned(),
                        (Some(entry), Some((key, _))) if entry.0 < *key => {
                            return old.next().cloned();
                        }
                        (Some(entry), Some((key, _))) if entry.0 == *key => {
                            old.next();
                        }
                        _ => {}
                    }
                    let (key, value) = edits.next().expect("pending edit");
                    if let Some((value, bytes)) = value {
                        return Some((key, value, bytes));
                    }
                }
            });
            let leaf_count = remaining.div_ceil(LEAF_ENTRIES);
            if leaf_count == 0 {
                leaves = leaves.remove(&first);
            }
            // Split evenly: 65 entries become 33/32, not a full leaf followed
            // by a singleton that proliferates under repeated insertions.
            for leaf_number in 0..leaf_count {
                let count =
                    remaining / leaf_count + usize::from(leaf_number < remaining % leaf_count);
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    entries.push(merged.next().expect("coalesced leaf count"));
                }
                let (key, leaf, bytes) = Self::leaf(entries)?;
                if leaf_number == 0 {
                    if key == first {
                        replacements.push((key, leaf, bytes));
                        continue;
                    }
                    leaves = leaves.remove(&first);
                }
                leaves = leaves.insert(key, leaf, bytes);
            }
            assert!(merged.next().is_none(), "coalesced leaf count");
        }
        replacements.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        leaves = leaves.replace_many(&replacements);
        Ok(Self { leaves })
    }
    #[cfg(test)]
    pub(crate) fn insert(&self, key: K, value: V, external_bytes: u64) -> Result<Self> {
        self.changed([], [(key, value, external_bytes)])
    }
    #[cfg(test)]
    pub(crate) fn remove(&self, key: &K) -> Result<Self> {
        let Some((_, old)) = self.leaves.floor(key) else {
            return Ok(self.clone());
        };
        if old
            .entries
            .binary_search_by(|entry| entry.0.cmp(key))
            .is_err()
        {
            return Ok(self.clone());
        }
        self.changed([key.clone()], [])
    }
}
fn end_offset<K: Ord, V>(entries: &[Entry<K, V>], end: Bound<&K>) -> usize {
    match end {
        Bound::Unbounded => entries.len(),
        Bound::Included(end) if entries.last().is_none_or(|entry| &entry.0 <= end) => entries.len(),
        Bound::Excluded(end) if entries.last().is_none_or(|entry| &entry.0 < end) => entries.len(),
        Bound::Included(end) => entries.partition_point(|(key, _, _)| key <= end),
        Bound::Excluded(end) => entries.partition_point(|(key, _, _)| key < end),
    }
}
pub(crate) struct IndexIter<'a, K, V, R> {
    leaves: VersionIter<'a, K, Arc<Leaf<K, V>>, std::ops::RangeFull>,
    entries: Option<std::slice::Iter<'a, Entry<K, V>>>,
    bounds: R,
    finished: bool,
}
impl<'a, K: Ord, V, R: RangeBounds<K>> Iterator for IndexIter<'a, K, V, R> {
    type Item = (&'a K, &'a V);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            if let Some((key, value, _)) = self.entries.as_mut().and_then(Iterator::next) {
                return Some((key, value));
            }
            let leaf = self.leaves.next()?.1;
            let end = end_offset(&leaf.entries, self.bounds.end_bound());
            if end == 0 {
                self.finished = true;
                return None;
            }
            self.entries = Some(leaf.entries[..end].iter());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn verify(index: &VersionIndex<u64, Arc<u64>>, map: &BTreeMap<u64, u64>) {
        for bounds in [
            (Bound::Unbounded, Bound::Unbounded),
            (Bound::Included(1), Bound::Excluded(70)),
            (Bound::Excluded(31), Bound::Included(32)),
            (Bound::Included(32), Bound::Included(32)),
            (Bound::Included(1000), Bound::Unbounded),
            (Bound::Included(u64::MAX), Bound::Included(u64::MAX)),
        ] {
            assert_eq!(
                index
                    .range(bounds)
                    .map(|(k, v)| (*k, **v))
                    .collect::<Vec<_>>(),
                map.range(bounds).map(|(k, v)| (*k, *v)).collect::<Vec<_>>()
            );
        }
        let mut counted = 0;
        let mut previous = None;
        let mut weight = 0;
        for (first, leaf) in index.leaves.range(..) {
            assert_eq!(*first, leaf.entries[0].0);
            assert!(!leaf.entries.is_empty() && leaf.entries.len() <= LEAF_ENTRIES);
            weight += Leaves::<u64, Arc<u64>>::node_bytes()
                + std::mem::size_of::<Leaf<u64, Arc<u64>>>() as u64
                + 2 * std::mem::size_of::<usize>() as u64
                + (leaf.entries.capacity() * std::mem::size_of::<Entry<u64, Arc<u64>>>()) as u64
                + leaf.entries[0].2
                + leaf.entries.iter().map(|entry| entry.2).sum::<u64>();
            for (key, _, _) in &leaf.entries {
                assert!(previous.is_none_or(|old| old < *key));
                previous = Some(*key);
                counted += 1;
            }
        }
        assert_eq!(counted, map.len());
        assert_eq!(weight, index.accounted_bytes());
    }
    #[test]
    fn leaf_boundaries_random_edits_and_retained_roots_match_independent_maps() {
        for len in [0, 1, 31, 32, 33, 63, 64, 65, 127, 128, 129, 1024] {
            let mut map: BTreeMap<_, _> = (0..len).map(|id| (id * 2, id + 1)).collect();
            map.insert(u64::MAX, 42);
            let mut index = VersionIndex::try_from_sorted(
                map.len(),
                map.iter().map(|(&k, &v)| Ok((k, Arc::new(v), 8))),
            )
            .unwrap();
            let original = index.clone();
            let original_map = map.clone();
            verify(&index, &map);
            let mut random = 7u64;
            for iteration in 0..2000 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let key = if iteration % 29 == 0 {
                    u64::MAX
                } else {
                    random % 2100
                };
                if random & 3 == 0 {
                    index = index.remove(&key).unwrap();
                    map.remove(&key);
                } else {
                    index = index.insert(key, Arc::new(random), 8).unwrap();
                    map.insert(key, random);
                }
                if iteration % 31 == 0 {
                    verify(&index, &map);
                    verify(&original, &original_map);
                }
            }
            verify(&index, &map);
            verify(&original, &original_map);
            for key in map.keys() {
                index = index.remove(key).unwrap();
            }
            assert_eq!(index.accounted_bytes(), 0);
            assert_eq!(index.range(..).count(), 0);
        }
    }
    #[test]
    fn entry_copy_is_bounded_and_untouched_leaves_are_shared() {
        use std::cell::Cell;
        struct Value<'a>(&'a Cell<usize>);
        impl Clone for Value<'_> {
            fn clone(&self) -> Self {
                self.0.set(self.0.get() + 1);
                Self(self.0)
            }
        }
        let copies = Cell::new(0);
        let old =
            VersionIndex::try_from_sorted(100000, (0..100000).map(|k| Ok((k, Value(&copies), 0))))
                .unwrap();
        assert_eq!(copies.get(), 0);
        let changed = old.insert(50000, Value(&copies), 0).unwrap();
        assert!(copies.get() <= LEAF_ENTRIES);
        assert!(Arc::ptr_eq(
            old.leaves.floor(&0).unwrap().1,
            changed.leaves.floor(&0).unwrap().1
        ));
        assert_eq!(old.range(..).count(), 100000);
        assert_eq!(changed.range(..).count(), 100000);
        copies.set(0);
        let deleted = changed.remove(&50000).unwrap();
        assert!(copies.get() <= LEAF_ENTRIES);
        assert_eq!(deleted.range(..).count(), 99999);
        assert_eq!(changed.range(..).count(), 100000);
        copies.set(0);
        deleted.remove(&50000).unwrap();
        assert_eq!(copies.get(), 0);
        // Replacing an entire leaf in a single transaction does not repeatedly
        // clone its entries. New values move into the candidate leaves.
        let first = *old.leaves.floor(&50000).unwrap().0;
        copies.set(0);
        let batch = old
            .changed(
                [],
                (first..first + LEAF_ENTRIES as u64).map(|key| (key, Value(&copies), 0)),
            )
            .unwrap();
        assert!(copies.get() <= LEAF_ENTRIES);
        assert_eq!(batch.range(..).count(), 100000);
        assert!(Arc::ptr_eq(
            old.leaves.floor(&0).unwrap().1,
            batch.leaves.floor(&0).unwrap().1
        ));
    }
    #[test]
    fn overflow_and_assessment_errors_drop_partial_builds() {
        assert!(matches!(
            VersionIndex::try_from_sorted(1, std::iter::once(Ok((0, (), u64::MAX)))),
            Err(Error::InvalidOperation(_))
        ));
        for fail in [0, 31, 32, 64] {
            let result = VersionIndex::try_from_sorted(
                65,
                (0..65).map(|k| {
                    if k == fail {
                        Err(Error::Codec("assessment".into()))
                    } else {
                        Ok((k, (), 0))
                    }
                }),
            );
            assert!(matches!(result, Err(Error::Codec(_))));
        }
    }
    #[test]
    fn overflowing_leaves_split_evenly_instead_of_proliferating_singletons() {
        let old = VersionIndex::try_from_sorted(
            LEAF_ENTRIES,
            (0..LEAF_ENTRIES).map(|k| Ok((k * 2, k, 0))),
        )
        .unwrap();
        let new = old.changed([], [(1, 1000, 0)]).unwrap();
        let sizes: Vec<_> = new
            .leaves
            .range(..)
            .map(|(_, leaf)| leaf.entries.len())
            .collect();
        assert_eq!(sizes.len(), 2);
        assert!(sizes.iter().all(|size| *size >= LEAF_ENTRIES / 2));
        assert_eq!(old.range(..).count(), LEAF_ENTRIES);
        assert_eq!(new.range(..).count(), LEAF_ENTRIES + 1);
    }
    #[test]
    #[should_panic(expected = "equal excluded range bounds")]
    fn invalid_empty_index_range_is_refused() {
        VersionIndex::<u64, ()>::try_from_sorted(0, std::iter::empty())
            .unwrap()
            .range((Bound::Excluded(1), Bound::Excluded(1)));
    }
}
