#[path = "../examples/support/tasks.rs"]
#[allow(dead_code)]
mod tasks;
use skrin::Result;
use skrin::catalog::CatalogDatabase;
use std::hint::black_box;
use std::time::Instant;
use tasks::*;

fn main() -> Result<()> {
    let rows: u64 = std::env::args()
        .nth(1)
        .map(|s| s.parse().unwrap())
        .unwrap_or(10_000);
    assert!(rows > 0);
    let db = CatalogDatabase::<TaskStore>::in_memory()?;
    let expected: Vec<_> = (0..rows)
        .map(|id| {
            (
                id,
                Task {
                    request_id: format!("request/{id}"),
                    title: format!("repo/{:02}/日本語\0{id:020}", id % 64),
                    spec_md: format!("{SPEC}\nTask {id}"),
                    status: "open".into(),
                },
            )
        })
        .collect();
    db.write(|tx| {
        for (id, task) in &expected {
            tx.insert::<Tasks>(*id, task.clone())?;
        }
        Ok(())
    })?;
    let mut sorted: Vec<_> = expected.iter().collect();
    sorted.sort_by_key(|(id, task)| (&task.title, *id));
    let prefixes = ["", "repo/00/", "repo/31/日本語\0", "repo/63/", "absent"];
    macro_rules! measure {
        ($view:expr, $label:literal) => {{
            let view = $view;
            // Check full records independently, outside timing, including limits.
            for prefix in prefixes {
                let wanted: Vec<_> = sorted
                    .iter()
                    .filter(|(_, task)| task.title.starts_with(prefix))
                    .take(20)
                    .map(|(id, task)| (*id, task))
                    .collect();
                assert_eq!(
                    view.prefix(ByTitle, prefix)?.take(20).collect::<Vec<_>>(),
                    wanted
                );
                let mut scanned: Vec<_> = view
                    .scan::<Tasks>()?
                    .filter(|(_, task)| task.title.starts_with(prefix))
                    .collect();
                scanned.sort_by_key(|(id, task)| (&task.title, *id));
                scanned.truncate(20);
                assert_eq!(scanned, wanted);
            }
            let start = Instant::now();
            for _ in 0..100 {
                for prefix in prefixes {
                    for (id, row) in view.prefix(ByTitle, black_box(prefix))?.take(20) {
                        black_box((id, &row.title, &row.spec_md, &row.status, &row.request_id));
                    }
                }
            }
            println!(
                "{} indexed prefix: {:?} / 500 resident queries",
                $label,
                start.elapsed()
            );
            let start = Instant::now();
            for _ in 0..100 {
                for prefix in prefixes {
                    let mut found: Vec<_> = view
                        .scan::<Tasks>()?
                        .filter(|(_, row)| row.title.starts_with(black_box(prefix)))
                        .collect();
                    found.sort_by_key(|(id, row)| (&row.title, *id));
                    for (id, row) in found.into_iter().take(20) {
                        black_box((id, &row.title, &row.spec_md, &row.status, &row.request_id));
                    }
                }
            }
            println!(
                "{} scan/filter/sort: {:?} / 500 resident queries",
                $label,
                start.elapsed()
            );
        }};
    }
    measure!(db.read()?, "native");
    let db = db.into_snapshots(options(), footprint)?;
    measure!(db.snapshot()?, "snapshot");
    println!("Verified {rows} rows; volatile warm resident workload, no cross-engine claim");
    Ok(())
}
