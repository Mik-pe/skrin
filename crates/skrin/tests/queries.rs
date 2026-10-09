#[path = "../examples/support/banking.rs"]
mod banking;
use banking::*;
use skrin::catalog::{CatalogDatabase, Index, IndexDefinition, IndexKey, Table};
use skrin::versioned::SnapshotOptions;
use skrin::{Error, Result};
use std::cell::Cell;
use std::ops::Bound::{Excluded, Included, Unbounded};

thread_local! { static BORROWS: Cell<usize> = const { Cell::new(0) }; }
struct Counted;
impl Table<Banking> for Counted {
    type Record = Account;
    fn into_row(r: Account) -> Row {
        Row::Account(r)
    }
    fn borrow(r: &Row) -> Option<&Account> {
        BORROWS.with(|calls| calls.set(calls.get() + 1));
        <Accounts as Table<Banking>>::borrow(r)
    }
}
struct ByCountedBalance;
impl Index<Banking> for ByCountedBalance {
    type Table = Counted;
    type Key = u64;
    const DEFINITION: IndexDefinition = IndexDefinition {
        id: 2,
        table_id: 1,
        version: 1,
        unique: false,
    };
    fn project(row: &Account) -> Result<Vec<u8>> {
        row.balance.encode_key()
    }
}
fn options() -> SnapshotOptions {
    SnapshotOptions {
        max_snapshots: 4,
        max_pinned_bytes: 64 * 1024 * 1024,
    }
}
fn footprint(row: &Row) -> Result<u64> {
    Ok(std::mem::size_of::<Row>() as u64
        + match row {
            Row::Account(r) => r.email.capacity() as u64,
            Row::Transfer(_) => 0,
        })
}
fn seeded() -> Result<CatalogDatabase<Banking>> {
    let db = CatalogDatabase::in_memory()?;
    seed(&db)?;
    db.write(|tx| transfer(tx, 7, 1, 2, 0))?;
    db.write(|tx| {
        for key in (3..53).rev().chain([u64::MAX]) {
            tx.insert::<Accounts>(
                key,
                Account {
                    email: format!("query-{key}"),
                    balance: if key == u64::MAX { key } else { key % 5 },
                },
            )?;
        }
        Ok(())
    })?;
    Ok(db)
}

#[test]
fn indexed_queries_are_lazy_before_filter_projection_and_limit() -> Result<()> {
    let db = seeded()?;
    {
        let read = db.read()?;
        BORROWS.with(|calls| calls.set(0));
        let query = read.query(ByCountedBalance, ..)?;
        assert_eq!(BORROWS.with(Cell::get), 0);
        assert_eq!(query.take(0).count(), 0);
        assert_eq!(BORROWS.with(Cell::get), 0);
        let ids: Vec<_> = read
            .query(ByCountedBalance, ..)?
            .take(3)
            .map(|(key, _)| key)
            .collect();
        assert_eq!(ids, [5, 10, 15]);
        assert_eq!(BORROWS.with(Cell::get), 3);
    }
    let db = db.into_snapshots(options(), footprint)?;
    let view = db.snapshot()?;
    BORROWS.with(|calls| calls.set(0));
    let query = view.query(ByCountedBalance, ..)?;
    assert_eq!(BORROWS.with(Cell::get), 0);
    let ids: Vec<_> = query.take(3).map(|(key, _)| key).collect();
    assert_eq!(ids, [5, 10, 15]);
    assert_eq!(BORROWS.with(Cell::get), 3);
    BORROWS.with(|calls| calls.set(0));
    assert_eq!(view.matching(ByCountedBalance, &0)?.take(0).count(), 0);
    assert_eq!(BORROWS.with(Cell::get), 0);
    assert_eq!(view.matching(ByCountedBalance, &0)?.take(2).count(), 2);
    assert_eq!(BORROWS.with(Cell::get), 2);
    let filtered: Vec<_> = view
        .index_scan::<Accounts>(2, 2u64.to_be_bytes().to_vec()..=4u64.to_be_bytes().to_vec())?
        .filter(|(key, row)| key % 2 == 0 && row.balance > 2)
        .take(4)
        .map(|(key, row)| (key, row.balance))
        .collect();
    assert_eq!(filtered, [(8, 3), (18, 3), (28, 3), (38, 3)]);
    Ok(())
}

#[test]
fn included_excluded_empty_and_unbounded_queries_match_independent_sorted_rows() -> Result<()> {
    let db = seeded()?;
    let mut reference: Vec<_> = [(1, 100), (2, 100)]
        .into_iter()
        .chain((3..53).map(|key| (key, key % 5)))
        .chain([(u64::MAX, u64::MAX)])
        .map(|(key, balance)| (balance.to_be_bytes().to_vec(), key, balance))
        .collect();
    reference.sort();
    let cases = [
        (Unbounded, Unbounded),
        (
            Included(2u64.to_be_bytes().to_vec()),
            Excluded(4u64.to_be_bytes().to_vec()),
        ),
        (
            Excluded(2u64.to_be_bytes().to_vec()),
            Included(4u64.to_be_bytes().to_vec()),
        ),
        (Included(vec![]), Included(vec![])),
        (
            Included(1000u64.to_be_bytes().to_vec()),
            Excluded(u64::MAX.to_be_bytes().to_vec()),
        ),
        (
            Included(u64::MAX.to_be_bytes().to_vec()),
            Included(u64::MAX.to_be_bytes().to_vec()),
        ),
        (Excluded(u64::MAX.to_be_bytes().to_vec()), Unbounded),
    ];
    {
        let read = db.read()?;
        for bounds in &cases {
            let expected: Vec<_> = reference
                .iter()
                .filter(|(k, _, _)| std::ops::RangeBounds::contains(bounds, k))
                .map(|(_, key, balance)| (*key, *balance))
                .collect();
            assert_eq!(
                read.index_scan::<Accounts>(2, bounds.clone())?
                    .map(|(key, row)| (key, row.balance))
                    .collect::<Vec<_>>(),
                expected
            );
        }
        assert!(matches!(
            read.index_scan::<Transfers>(2, ..),
            Err(Error::InvalidOperation(_))
        ));
        assert!(matches!(
            read.index_scan::<Accounts>(99, ..),
            Err(Error::InvalidOperation(_))
        ));
    }
    let db = db.into_snapshots(options(), footprint)?;
    let view = db.snapshot()?;
    for bounds in &cases {
        let expected: Vec<_> = reference
            .iter()
            .filter(|(k, _, _)| std::ops::RangeBounds::contains(bounds, k))
            .map(|(_, key, balance)| (*key, *balance))
            .collect();
        assert_eq!(
            view.index_scan::<Accounts>(2, bounds.clone())?
                .map(|(key, row)| (key, row.balance))
                .collect::<Vec<_>>(),
            expected
        );
    }
    assert!(matches!(
        view.index_scan::<Transfers>(2, ..),
        Err(Error::InvalidOperation(_))
    ));
    assert!(matches!(
        view.index_scan::<Accounts>(99, ..),
        Err(Error::InvalidOperation(_))
    ));
    Ok(())
}

#[test]
fn invalid_bounds_are_refused_even_when_indexes_have_no_rows() -> Result<()> {
    let db = CatalogDatabase::<Banking>::in_memory()?;
    let invalid = [
        (Included(vec![2]), Included(vec![1])),
        (Excluded(vec![1]), Excluded(vec![1])),
    ];
    {
        let read = db.read()?;
        assert_eq!(read.index_scan::<Accounts>(2, ..)?.count(), 0);
        for bounds in &invalid {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = read.index_scan::<Accounts>(2, bounds.clone());
                }))
                .is_err()
            );
        }
    }
    let db = db.into_snapshots(options(), footprint)?;
    let view = db.snapshot()?;
    assert_eq!(view.index_scan::<Accounts>(2, ..)?.count(), 0);
    for bounds in &invalid {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = view.index_scan::<Accounts>(2, bounds.clone());
            }))
            .is_err()
        );
    }
    Ok(())
}
