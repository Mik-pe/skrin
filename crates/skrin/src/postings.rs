//! Ordered row IDs and physical slots for one derived index key. Small groups need no heap node;
//! large groups retain logarithmic insertion/removal through BTreeMap.
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) enum Posting {
    #[default]
    Empty,
    One((u64, u64)),
    Two([(u64, u64); 2]),
    Many(BTreeMap<u64, u64>),
}
impl Posting {
    pub(crate) fn after(&self, key: u64) -> PostingScan<'_> {
        use std::ops::Bound::{Excluded, Unbounded};
        match self {
            Self::Empty => PostingScan::Full(PostingIter::Inline([].iter())),
            Self::One(old) => {
                let keys = std::slice::from_ref(old);
                PostingScan::Full(PostingIter::Inline(
                    keys[usize::from(old.0 <= key)..].iter(),
                ))
            }
            Self::Two(keys) => PostingScan::Full(PostingIter::Inline(
                keys[keys.partition_point(|old| old.0 <= key)..].iter(),
            )),
            Self::Many(keys) => PostingScan::After(keys.range((Excluded(key), Unbounded))),
        }
    }
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Two(_) => 2,
            Self::Many(keys) => keys.len(),
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
    pub(crate) fn insert(&mut self, key: u64, slot: u64) -> bool {
        let entry = (key, slot);
        match self {
            Self::Empty => *self = Self::One(entry),
            Self::One(old) => {
                if old.0 == key {
                    old.1 = slot;
                    return false;
                }
                *self = Self::Two(if old.0 < key {
                    [*old, entry]
                } else {
                    [entry, *old]
                });
            }
            Self::Two(keys) => {
                if let Some(old) = keys.iter_mut().find(|old| old.0 == key) {
                    old.1 = slot;
                    return false;
                }
                *self = Self::Many(BTreeMap::from([keys[0], keys[1], entry]));
            }
            Self::Many(keys) => return keys.insert(key, slot).is_none(),
        }
        true
    }
    pub(crate) fn remove(&mut self, key: &u64) -> bool {
        match self {
            Self::Empty => return false,
            Self::One(old) => {
                if old.0 != *key {
                    return false;
                }
                *self = Self::Empty;
            }
            Self::Two(keys) => {
                if keys[0].0 == *key {
                    *self = Self::One(keys[1]);
                } else if keys[1].0 == *key {
                    *self = Self::One(keys[0]);
                } else {
                    return false;
                }
            }
            Self::Many(keys) => {
                if keys.remove(key).is_none() {
                    return false;
                }
                if keys.len() == 2 {
                    let mut remaining = keys.iter();
                    let (&a, &av) = remaining.next().unwrap();
                    let (&b, &bv) = remaining.next().unwrap();
                    *self = Self::Two([(a, av), (b, bv)]);
                }
            }
        }
        true
    }
}
// Keep full traversal's exact-size iterator while allowing a logarithmic
// suffix seek into a large posting; BTreeMap::Range is not ExactSizeIterator.
pub(crate) enum PostingScan<'a> {
    Full(PostingIter<'a>),
    After(std::collections::btree_map::Range<'a, u64, u64>),
}
impl<'a> Iterator for PostingScan<'a> {
    type Item = (u64, u64);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Full(iter) => iter.next(),
            Self::After(iter) => iter.next().map(|(&key, &slot)| (key, slot)),
        }
    }
}
pub(crate) enum PostingIter<'a> {
    Inline(std::slice::Iter<'a, (u64, u64)>),
    Tree(std::collections::btree_map::Iter<'a, u64, u64>),
}
impl<'a> IntoIterator for &'a Posting {
    type Item = (u64, u64);
    type IntoIter = PostingIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        match self {
            Posting::Empty => PostingIter::Inline([].iter()),
            Posting::One(key) => PostingIter::Inline(std::slice::from_ref(key).iter()),
            Posting::Two(keys) => PostingIter::Inline(keys.iter()),
            Posting::Many(keys) => PostingIter::Tree(keys.iter()),
        }
    }
}
impl<'a> Iterator for PostingIter<'a> {
    type Item = (u64, u64);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Inline(i) => i.next().copied(),
            Self::Tree(i) => i.next().map(|(&key, &slot)| (key, slot)),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Inline(i) => i.size_hint(),
            Self::Tree(i) => i.size_hint(),
        }
    }
}
impl ExactSizeIterator for PostingIter<'_> {}
impl DoubleEndedIterator for PostingIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Inline(i) => i.next_back().copied(),
            Self::Tree(i) => i.next_back().map(|(&key, &slot)| (key, slot)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn verify(posting: &Posting, expected: &BTreeMap<u64, u64>) {
        assert_eq!(posting.len(), expected.len());
        assert_eq!(posting.is_empty(), expected.is_empty());
        assert_eq!(
            posting.into_iter().collect::<Vec<_>>(),
            expected
                .iter()
                .map(|(&key, &slot)| (key, slot))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            posting.into_iter().rev().collect::<Vec<_>>(),
            expected
                .iter()
                .rev()
                .map(|(&key, &slot)| (key, slot))
                .collect::<Vec<_>>()
        );
        assert_eq!(posting.into_iter().len(), expected.len());
        for cursor in [0, 1, 2, 42, 66, 99, u64::MAX] {
            assert_eq!(
                posting.after(cursor).collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|(&key, &slot)| (key, slot))
                    .filter(|(key, _)| *key > cursor)
                    .collect::<Vec<_>>()
            );
        }
        assert!(match posting {
            Posting::Empty => expected.is_empty(),
            Posting::One(_) => expected.len() == 1,
            Posting::Two(_) => expected.len() == 2,
            Posting::Many(keys) => keys.len() >= 3,
        });
    }
    #[test]
    fn promotion_demotion_replaced_slots_and_full_u64_keys_match_ordered_map() {
        let mut posting = Posting::default();
        let mut expected = BTreeMap::new();
        for iteration in 0..8 {
            for key in [u64::MAX, 0, 42, 1, 3, 7, 42, u64::MAX] {
                let slot = key.rotate_left(17).wrapping_add(iteration);
                assert_eq!(
                    posting.insert(key, slot),
                    expected.insert(key, slot).is_none()
                );
                verify(&posting, &expected);
            }
            for key in [99, 42, 0, 1, 3, 7, u64::MAX, 42] {
                assert_eq!(posting.remove(&key), expected.remove(&key).is_some());
                verify(&posting, &expected);
            }
        }
        let mut random = 1234567u64;
        for _ in 0..10000 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let key = random % 67;
            if random & 128 != 0 {
                let slot = random;
                assert_eq!(
                    posting.insert(key, slot),
                    expected.insert(key, slot).is_none()
                );
            } else {
                assert_eq!(posting.remove(&key), expected.remove(&key).is_some());
            }
            verify(&posting, &expected);
        }
    }
}
