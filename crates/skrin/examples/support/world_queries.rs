//! Application queries over one retained frame, using the world's own indexes.
use super::world::*;
use skrin::versioned::CatalogSnapshot;
use skrin::{Error, Result};
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

pub struct Visible {
    pub areas: RangeInclusive<u64>,
    pub x: RangeInclusive<u64>,
    pub y: RangeInclusive<u64>,
}

/// Cursor is (area, primary key), matching the area index's complete ordering.
/// Keep the same snapshot and predicates across pages for a stable traversal.
pub fn visible_entities(
    frame: &CatalogSnapshot<World>,
    query: &Visible,
    after: Option<(u64, u64)>,
    limit: usize,
) -> Result<Vec<(u64, Entity)>> {
    if query.areas.is_empty() || query.x.is_empty() || query.y.is_empty() {
        return Err(Error::InvalidOperation("reversed visible bounds".into()));
    }
    let rows = match after {
        None => frame.query(Area, query.areas.clone())?,
        Some((area, id)) => frame.query_after(Area, query.areas.clone(), (&area, id))?,
    };
    Ok(rows
        .filter(|(_, row)| query.x.contains(&row.x) && query.y.contains(&row.y))
        .take(limit)
        .map(|(key, row)| (key, *row))
        .collect())
}

#[derive(Debug, PartialEq, Eq)]
pub struct InventoryEntry {
    pub key: u64,
    pub item: Item,
    pub owner: Entity,
}

/// Filter by kind, paginate within one owner, and join the owner by primary key.
/// Both sides use the same frame; an orphan is an application consistency error.
pub fn inventory(
    frame: &CatalogSnapshot<World>,
    owner: u64,
    kind: Option<u64>,
    after: Option<u64>,
    limit: usize,
) -> Result<Vec<InventoryEntry>> {
    let rows = match after {
        None => frame.matching(Owner, &owner)?,
        Some(id) => frame.query_after(Owner, owner..=owner, (&owner, id))?,
    };
    rows.filter(|(_, row)| kind.is_none_or(|kind| row.kind == kind))
        .take(limit)
        .map(|(key, item)| {
            let owner = *frame.get::<Entities>(item.owner)?.ok_or_else(|| {
                Error::InvalidOperation("inventory references missing owner".into())
            })?;
            Ok(InventoryEntry {
                key,
                item: *item,
                owner,
            })
        })
        .collect()
}

/// Compare pages/joins against the workload's independent closed-form rows.
pub fn verify_queries(
    frame: &CatalogSnapshot<World>,
    rows: u64,
    saves: u64,
    batch: u64,
) -> Result<()> {
    let query = Visible {
        areas: 0..=3,
        x: 30..=100,
        y: 0..=0,
    };
    let mut expected: Vec<_> = (0..rows)
        .map(|key| (key, expected_entity(key, rows, saves, batch)))
        .filter(|(_, r)| {
            query.areas.contains(&r.area) && query.x.contains(&r.x) && query.y.contains(&r.y)
        })
        .collect();
    expected.sort_unstable_by_key(|(key, row)| (row.area, *key));
    let mut actual = Vec::new();
    let mut after = None;
    loop {
        let page = visible_entities(frame, &query, after, 7)?;
        assert!(page.len() <= 7);
        let Some((key, row)) = page.last() else { break };
        after = Some((row.area, *key));
        actual.extend(page);
        assert!(actual.len() <= rows as usize, "cursor did not advance");
    }
    assert_eq!(actual, expected);
    assert!(visible_entities(frame, &query, None, 0)?.is_empty());
    let mut owners: BTreeMap<u64, Vec<InventoryEntry>> = BTreeMap::new();
    for key in 0..rows {
        let item = expected_item(key, rows, saves);
        owners.entry(item.owner).or_default().push(InventoryEntry {
            key,
            item,
            owner: expected_entity(item.owner, rows, saves, batch),
        });
    }
    for owner in 0..rows {
        assert_eq!(
            inventory(frame, owner, None, None, rows as usize)?,
            owners.remove(&owner).unwrap_or_default()
        );
    }
    Ok(())
}
