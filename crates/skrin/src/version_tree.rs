// Immutable AVL roots. Updates copy only the search/rotation paths.
use std::cmp::Ordering;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

type Link<K, V> = Option<Arc<Node<K, V>>>;
struct Node<K, V> {
    key: K,
    value: V,
    own_weight: u64,
    left: Link<K, V>,
    right: Link<K, V>,
    height: u32,
    count: usize,
    weight: u64,
}
pub(crate) struct VersionTree<K, V> {
    root: Link<K, V>,
}
impl<K, V> Clone for VersionTree<K, V> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
        }
    }
}
impl<K, V> Default for VersionTree<K, V> {
    fn default() -> Self {
        Self { root: None }
    }
}
fn height<K, V>(node: &Link<K, V>) -> u32 {
    node.as_ref().map_or(0, |n| n.height)
}
fn count<K, V>(node: &Link<K, V>) -> usize {
    node.as_ref().map_or(0, |n| n.count)
}
fn weight<K, V>(node: &Link<K, V>) -> u64 {
    node.as_ref().map_or(0, |n| n.weight)
}
fn node<K, V>(
    key: K,
    value: V,
    own_weight: u64,
    left: Link<K, V>,
    right: Link<K, V>,
) -> Arc<Node<K, V>> {
    Arc::new(Node {
        key,
        value,
        own_weight,
        height: 1 + height(&left).max(height(&right)),
        count: 1 + count(&left) + count(&right),
        weight: own_weight
            .saturating_add(weight(&left))
            .saturating_add(weight(&right)),
        left,
        right,
    })
}
// Consume an ordered resident source in-order. Allocate each final node once,
// with only logarithmic recursion space and no temporary full-size vector.
fn build_sorted<K, V>(
    len: usize,
    entries: &mut impl Iterator<Item = crate::Result<(K, V, u64)>>,
) -> crate::Result<Link<K, V>> {
    if len == 0 {
        return Ok(None);
    }
    let left_len = len / 2;
    let left = build_sorted(left_len, entries)?;
    let (key, value, own_weight) = entries.next().expect("declared sorted source length")?;
    let right = build_sorted(len - left_len - 1, entries)?;
    Ok(Some(node(key, value, own_weight, left, right)))
}
impl<K, V> VersionTree<K, V> {
    /// Build once from exactly `len` entries with strictly increasing keys.
    /// Callers supply validated ordered maps/postings; values need not be cloned.
    pub(crate) fn try_from_sorted(
        len: usize,
        mut entries: impl Iterator<Item = crate::Result<(K, V, u64)>>,
    ) -> crate::Result<Self> {
        let root = build_sorted(len, &mut entries)?;
        assert!(entries.next().is_none(), "declared sorted source length");
        Ok(Self { root })
    }
}
fn balanced<K: Clone, V: Clone>(
    key: K,
    value: V,
    own_weight: u64,
    left: Link<K, V>,
    right: Link<K, V>,
) -> Arc<Node<K, V>> {
    if height(&left) > height(&right) + 1 {
        let l = left.as_ref().unwrap();
        if height(&l.left) >= height(&l.right) {
            return node(
                l.key.clone(),
                l.value.clone(),
                l.own_weight,
                l.left.clone(),
                Some(node(key, value, own_weight, l.right.clone(), right)),
            );
        }
        let pivot = l.right.as_ref().unwrap();
        return node(
            pivot.key.clone(),
            pivot.value.clone(),
            pivot.own_weight,
            Some(node(
                l.key.clone(),
                l.value.clone(),
                l.own_weight,
                l.left.clone(),
                pivot.left.clone(),
            )),
            Some(node(key, value, own_weight, pivot.right.clone(), right)),
        );
    }
    if height(&right) > height(&left) + 1 {
        let r = right.as_ref().unwrap();
        if height(&r.right) >= height(&r.left) {
            return node(
                r.key.clone(),
                r.value.clone(),
                r.own_weight,
                Some(node(key, value, own_weight, left, r.left.clone())),
                r.right.clone(),
            );
        }
        let pivot = r.left.as_ref().unwrap();
        return node(
            pivot.key.clone(),
            pivot.value.clone(),
            pivot.own_weight,
            Some(node(key, value, own_weight, left, pivot.left.clone())),
            Some(node(
                r.key.clone(),
                r.value.clone(),
                r.own_weight,
                pivot.right.clone(),
                r.right.clone(),
            )),
        );
    }
    node(key, value, own_weight, left, right)
}
fn insert<K: Ord + Clone, V: Clone>(
    root: &Link<K, V>,
    key: K,
    value: V,
    own_weight: u64,
) -> Arc<Node<K, V>> {
    let Some(old) = root else {
        return node(key, value, own_weight, None, None);
    };
    match key.cmp(&old.key) {
        Ordering::Less => balanced(
            old.key.clone(),
            old.value.clone(),
            old.own_weight,
            Some(insert(&old.left, key, value, own_weight)),
            old.right.clone(),
        ),
        Ordering::Greater => balanced(
            old.key.clone(),
            old.value.clone(),
            old.own_weight,
            old.left.clone(),
            Some(insert(&old.right, key, value, own_weight)),
        ),
        Ordering::Equal => node(key, value, own_weight, old.left.clone(), old.right.clone()),
    }
}
// Keys already exist and are strictly ordered. Replacements preserve the AVL
// shape, so each affected ancestor is copied once without rotations or deletes.
fn replace_many<K: Ord + Clone, V: Clone>(
    root: &Link<K, V>,
    updates: &[(K, V, u64)],
) -> Link<K, V> {
    if updates.is_empty() {
        return root.clone();
    }
    let old = root.as_ref().expect("replacement key exists");
    let split = updates.partition_point(|(key, _, _)| key < &old.key);
    let found = updates.get(split).filter(|(key, _, _)| key == &old.key);
    let right_start = split + usize::from(found.is_some());
    let left = replace_many(&old.left, &updates[..split]);
    let right = replace_many(&old.right, &updates[right_start..]);
    let (value, own_weight) = found.map_or((&old.value, old.own_weight), |(_, value, weight)| {
        (value, *weight)
    });
    Some(node(
        old.key.clone(),
        value.clone(),
        own_weight,
        left,
        right,
    ))
}
fn remove<K: Ord + Clone, V: Clone>(root: &Link<K, V>, key: &K) -> Link<K, V> {
    let old = root.as_ref()?;
    match key.cmp(&old.key) {
        Ordering::Less => Some(balanced(
            old.key.clone(),
            old.value.clone(),
            old.own_weight,
            remove(&old.left, key),
            old.right.clone(),
        )),
        Ordering::Greater => Some(balanced(
            old.key.clone(),
            old.value.clone(),
            old.own_weight,
            old.left.clone(),
            remove(&old.right, key),
        )),
        Ordering::Equal => {
            if old.left.is_none() {
                return old.right.clone();
            }
            if old.right.is_none() {
                return old.left.clone();
            }
            let mut next = old.right.as_ref().unwrap();
            while let Some(left) = &next.left {
                next = left;
            }
            Some(balanced(
                next.key.clone(),
                next.value.clone(),
                next.own_weight,
                old.left.clone(),
                remove(&old.right, &next.key),
            ))
        }
    }
}
impl<K: Ord + Clone, V: Clone> VersionTree<K, V> {
    pub(crate) fn node_bytes() -> u64 {
        // Arc's two counters are included; allocator rounding/metadata is not.
        (std::mem::size_of::<Node<K, V>>()
            + (2 * std::mem::size_of::<usize>())
                .next_multiple_of(std::mem::align_of::<Node<K, V>>())) as u64
    }
    pub(crate) fn len(&self) -> usize {
        count(&self.root)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.root.is_none()
    }
    pub(crate) fn accounted_bytes(&self) -> u64 {
        weight(&self.root)
    }
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        let mut next = self.root.as_deref();
        while let Some(node) = next {
            match key.cmp(&node.key) {
                Ordering::Less => next = node.left.as_deref(),
                Ordering::Greater => next = node.right.as_deref(),
                Ordering::Equal => return Some(&node.value),
            }
        }
        None
    }
    pub(crate) fn insert(&self, key: K, value: V, own_weight: u64) -> Self {
        Self {
            root: Some(insert(&self.root, key, value, own_weight)),
        }
    }
    /// Replace existing keys in strictly increasing order, sharing untouched
    /// subtrees. Internal preconditions are checked before tree traversal.
    pub(crate) fn replace_many(&self, updates: &[(K, V, u64)]) -> Self {
        assert!(
            updates.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "ordered distinct replacements"
        );
        Self {
            root: replace_many(&self.root, updates),
        }
    }
    pub(crate) fn remove(&self, key: &K) -> Self {
        if self.get(key).is_none() {
            return self.clone();
        }
        Self {
            root: remove(&self.root, key),
        }
    }
    pub(crate) fn range<R: RangeBounds<K>>(&self, bounds: R) -> VersionIter<'_, K, V, R> {
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
        let mut stack = Vec::new();
        let mut next = self.root.as_deref();
        while let Some(node) = next {
            let before = match bounds.start_bound() {
                Bound::Unbounded => false,
                Bound::Included(k) => node.key < *k,
                Bound::Excluded(k) => node.key <= *k,
            };
            if before {
                next = node.right.as_deref();
            } else {
                stack.push(node);
                next = node.left.as_deref();
            }
        }
        VersionIter {
            stack,
            bounds,
            finished: false,
        }
    }
}
pub(crate) struct VersionIter<'a, K, V, R> {
    stack: Vec<&'a Node<K, V>>,
    bounds: R,
    finished: bool,
}
impl<'a, K: Ord, V, R: RangeBounds<K>> Iterator for VersionIter<'a, K, V, R> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let node = self.stack.pop()?;
        let past = match self.bounds.end_bound() {
            Bound::Unbounded => false,
            Bound::Included(k) => node.key > *k,
            Bound::Excluded(k) => node.key >= *k,
        };
        if past {
            self.finished = true;
            self.stack.clear();
            return None;
        }
        let mut next = node.right.as_deref();
        while let Some(node) = next {
            self.stack.push(node);
            next = node.left.as_deref();
        }
        Some((&node.key, &node.value))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn invariant(root: &Link<u64, Arc<u64>>) -> (u32, usize, u64) {
        let Some(n) = root else { return (0, 0, 0) };
        let (lh, lc, lw) = invariant(&n.left);
        let (rh, rc, rw) = invariant(&n.right);
        assert!(lh.abs_diff(rh) <= 1);
        assert_eq!(n.height, 1 + lh.max(rh));
        assert_eq!(n.count, 1 + lc + rc);
        assert_eq!(n.weight, n.own_weight + lw + rw);
        (n.height, n.count, n.weight)
    }
    #[test]
    fn sorted_build_and_subsequent_versions_match_independent_maps() {
        for len in [0, 1, 2, 3, 4, 7, 8, 9, 63, 64, 65, 1024] {
            let mut map: BTreeMap<_, _> = (0..len).map(|key| (key * 2, key + 1)).collect();
            map.insert(u64::MAX, 42);
            let mut tree = VersionTree::try_from_sorted(
                map.len(),
                map.iter().map(|(&k, &v)| Ok((k, Arc::new(v), v))),
            )
            .unwrap();
            let old = tree.clone();
            let old_map = map.clone();
            invariant(&tree.root);
            // Mutate median-built trees, including both insertion rotations
            // and deletions, while the initial root remains pinned.
            for key in 0..len * 2 {
                if key % 3 == 0 {
                    tree = tree.remove(&key);
                    map.remove(&key);
                } else {
                    tree = tree.insert(key, Arc::new(key + 7), key + 7);
                    map.insert(key, key + 7);
                }
                let (_, count, weight) = invariant(&tree.root);
                assert_eq!(count, map.len());
                assert_eq!(weight, map.values().sum());
            }
            for (view, expected) in [(&tree, &map), (&old, &old_map)] {
                for bounds in [
                    (std::ops::Bound::Unbounded, std::ops::Bound::Unbounded),
                    (std::ops::Bound::Included(2), std::ops::Bound::Excluded(9)),
                    (
                        std::ops::Bound::Included(u64::MAX),
                        std::ops::Bound::Included(u64::MAX),
                    ),
                ] {
                    assert_eq!(
                        view.range(bounds)
                            .map(|(&k, v)| (k, **v))
                            .collect::<BTreeMap<_, _>>(),
                        expected.range(bounds).map(|(&k, &v)| (k, v)).collect()
                    );
                }
            }
        }
        let empty = VersionTree::<u64, Arc<u64>>::try_from_sorted(0, std::iter::empty()).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.accounted_bytes(), 0);
    }
    #[test]
    fn sorted_build_moves_values_and_drops_partial_trees_on_assessment_error() {
        use std::cell::Cell;
        struct Value<'a>(&'a Cell<usize>); // Deliberately has no Clone impl.
        impl Drop for Value<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let drops = Cell::new(0);
        let tree = VersionTree::try_from_sorted(15, (0..15).map(|key| Ok((key, Value(&drops), 1))))
            .unwrap();
        assert_eq!(drops.get(), 0);
        drop(tree);
        assert_eq!(drops.get(), 15);
        for fail_at in 0..15 {
            drops.set(0);
            let result = VersionTree::try_from_sorted(
                15,
                (0..15).map(|key| {
                    if key == fail_at {
                        Err(crate::Error::InvalidOperation("assessment failed".into()))
                    } else {
                        Ok((key, Value(&drops), 1))
                    }
                }),
            );
            assert!(matches!(result, Err(crate::Error::InvalidOperation(_))));
            assert_eq!(drops.get(), fail_at);
        }
    }
    #[test]
    fn randomized_versions_and_ranges_match_independent_maps() {
        let mut tree = VersionTree::default();
        let mut map = BTreeMap::new();
        let mut retained = Vec::new();
        let mut rng = 7u64;
        for iteration in 0..10000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let key = rng % 300;
            if rng & 3 == 0 {
                tree = tree.remove(&key);
                map.remove(&key);
            } else {
                tree = tree.insert(key, Arc::new(rng), 17);
                map.insert(key, rng);
            }
            let (_, len, bytes) = invariant(&tree.root);
            assert_eq!(len, map.len());
            assert_eq!(bytes, len as u64 * 17);
            if iteration % 137 == 0 {
                retained.push((tree.clone(), map.clone()));
            }
            if iteration % 31 == 0 {
                let bounds = 50..=100;
                let actual: Vec<_> = tree.range(bounds.clone()).map(|(k, v)| (*k, **v)).collect();
                assert_eq!(
                    actual,
                    map.range(bounds).map(|(k, v)| (*k, *v)).collect::<Vec<_>>()
                );
            }
        }
        for (tree, map) in retained {
            assert_eq!(
                tree.range(..)
                    .map(|(k, v)| (*k, **v))
                    .collect::<BTreeMap<_, _>>(),
                map
            );
        }
    }
    #[test]
    fn sorted_insert_and_delete_stay_balanced_and_preserve_untouched_values() {
        let mut tree = VersionTree::default();
        for key in 0..10000 {
            tree = tree.insert(key, Arc::new(key), 1);
        }
        assert!(height(&tree.root) < 20);
        let old = tree.clone();
        let value = old.get(&5000).unwrap().clone();
        for key in 0..5000 {
            tree = tree.remove(&key);
        }
        assert!(Arc::ptr_eq(tree.get(&5000).unwrap(), &value));
        assert_eq!(old.len(), 10000);
        assert_eq!(tree.len(), 5000);
        invariant(&tree.root);
        for key in 5000..10000 {
            tree = tree.remove(&key);
        }
        assert!(tree.is_empty());
        assert_eq!(tree.accounted_bytes(), 0);
        assert_eq!(old.len(), 10000);
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    #[test]
    fn replacement_batch_preserves_old_roots_shape_weight_and_untouched_subtrees() {
        let mut old = VersionTree::default();
        for key in 0..1024u64 {
            old = old.insert(key, key, 1);
        }
        for stride in [1, 3, 17, 1025] {
            let updates: Vec<_> = (0..1024u64)
                .step_by(stride)
                .map(|k| (k, k + 10000, 3))
                .collect();
            let changed = old.replace_many(&updates);
            assert_eq!(changed.len(), old.len());
            assert_eq!(height(&changed.root), height(&old.root));
            assert_eq!(changed.accounted_bytes(), 1024 + updates.len() as u64 * 2);
            for key in 0..1024u64 {
                assert_eq!(old.get(&key), Some(&key));
                assert_eq!(
                    changed.get(&key),
                    Some(&(key + if key % stride as u64 == 0 { 10000 } else { 0 }))
                );
            }
        }
        let changed = old.replace_many(&[(0, 99, 5)]);
        assert!(Arc::ptr_eq(
            old.root.as_ref().unwrap().right.as_ref().unwrap(),
            changed.root.as_ref().unwrap().right.as_ref().unwrap()
        ));
        let empty = old.replace_many(&[]);
        assert!(Arc::ptr_eq(
            old.root.as_ref().unwrap(),
            empty.root.as_ref().unwrap()
        ));
    }
    #[test]
    #[should_panic(expected = "replacement key exists")]
    fn missing_replacement_is_an_internal_error() {
        VersionTree::<u64, u64>::default().replace_many(&[(1, 1, 1)]);
    }
}
