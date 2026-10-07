//! Equivalent independent durable indexed transfers, immediate versus shared sync.
#[path = "../examples/support/banking.rs"]
mod banking;
use banking::*;
use skrin::catalog::CatalogDatabase;
use skrin::group_commit::{GroupCommitCatalog, GroupCommitOptions};
use skrin::{Error, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

#[derive(Clone)]
enum Client {
    Immediate(Arc<CatalogDatabase<Banking>>),
    Group(GroupCommitCatalog<Banking>),
}
struct Sample {
    latency: Duration,
    callback_wait: Duration,
    admitted_wait: Option<Duration>,
    group: u64,
    frames: usize,
}
impl Client {
    fn write(&self, id: u64) -> Result<Sample> {
        let start = Instant::now();
        let (callback_wait, admitted_wait, group, frames) = match self {
            Self::Immediate(db) => {
                let wait = db.write(|tx| {
                    let wait = start.elapsed();
                    transfer(tx, id, 1, 2, 1)?;
                    Ok(wait)
                })?;
                (wait, None, id, 1)
            }
            Self::Group(db) => {
                let receipt = db
                    .submit(move |tx| {
                        let wait = start.elapsed();
                        transfer(tx, id, 1, 2, 1)?;
                        Ok(wait)
                    })?
                    .wait()?;
                (
                    receipt.value,
                    Some(receipt.queue_time),
                    receipt.synchronized_sequence,
                    receipt.transactions_in_group,
                )
            }
        };
        Ok(Sample {
            latency: start.elapsed(),
            callback_wait,
            admitted_wait,
            group,
            frames,
        })
    }
    fn into_database(self) -> Result<CatalogDatabase<Banking>> {
        match self {
            Self::Immediate(db) => Arc::try_unwrap(db).map_err(|_| Error::Busy),
            Self::Group(db) => db.into_database(),
        }
    }
}
fn verify(db: &CatalogDatabase<Banking>, count: u64) -> Result<()> {
    let read = db.read()?;
    assert_eq!(read.sequence(), count + 2);
    assert_eq!(read.get::<Accounts>(1)?.unwrap().balance, 100);
    assert_eq!(read.get::<Accounts>(2)?.unwrap().balance, count + 100);
    assert_eq!(read.lookup::<Accounts>(2, &100u64.to_be_bytes())?[0].0, 1);
    assert_eq!(read.lookup::<Accounts>(1, b"alice@example.test")?[0].0, 1);
    assert_eq!(read.scan::<Transfers>()?.count() as u64, count);
    for id in 1..=count {
        let row = read.get::<Transfers>(id)?.unwrap();
        assert_eq!((row.from, row.to, row.amount), (1, 2, 1));
    }
    Ok(())
}
fn memory() {
    #[cfg(target_os = "linux")]
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status
            .lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
        {
            println!("memory,{line}");
        }
    }
    #[cfg(not(target_os = "linux"))]
    println!("memory,unavailable");
}
fn disk(path: &Path) -> Result<(u64, Option<u64>)> {
    let mut logical = 0;
    let mut allocated = if cfg!(unix) { Some(0) } else { None };
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            let (bytes, blocks) = disk(&entry.path())?;
            logical += bytes;
            allocated = allocated.zip(blocks).map(|(a, b)| a + b);
        } else if ty.is_file() {
            let meta = entry.metadata()?;
            logical += meta.len();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                allocated = allocated.map(|n| n + meta.blocks() * 512);
            }
        }
    }
    Ok((logical, allocated))
}
fn distribution(name: &str, values: impl Iterator<Item = Duration>) {
    let mut values: Vec<_> = values.collect();
    if values.is_empty() {
        println!("{name},unavailable");
        return;
    }
    values.sort();
    let percentile =
        |p: usize| values[(values.len() * p).div_ceil(100).saturating_sub(1)].as_secs_f64() * 1e6;
    println!(
        "{name},samples={},p50_us={:.3},p95_us={:.3},p99_us={:.3},max_us={:.3}",
        values.len(),
        percentile(50),
        percentile(95),
        percentile(99),
        values.last().unwrap().as_secs_f64() * 1e6
    );
}
fn number(text: &str, min: u64, max: u64) -> Result<u64> {
    let value: u64 = text
        .parse()
        .map_err(|_| Error::InvalidOperation("invalid benchmark number".into()))?;
    if !(min..=max).contains(&value) {
        return Err(Error::InvalidOperation(
            "benchmark number out of range".into(),
        ));
    }
    Ok(value)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .collect();
    if args.first().is_some_and(|a| a == "--reopen") && args.len() == 3 {
        let start = Instant::now();
        let db = CatalogDatabase::<Banking>::open_dir(&args[1])?;
        let elapsed = start.elapsed();
        memory();
        verify(&db, number(&args[2], 1, 320_000)?)?;
        println!(
            "reopen,open_ms={:.3},cache_policy=no_advice,includes_recovery_sync=true",
            elapsed.as_secs_f64() * 1e3
        );
        return Ok(());
    }
    if args.len() != 6 || !matches!(args[1].as_str(), "immediate" | "group") {
        return Err(Error::InvalidOperation("usage: group_commit EXISTING_DIRECTORY immediate|group PRODUCERS TRANSACTIONS_PER_PRODUCER INTER_REQUEST_US GROUP_DELAY_US".into()));
    }
    let producers = number(&args[2], 1, 32)? as usize;
    let per = number(&args[3], 1, 10_000)?;
    let interval = Duration::from_micros(number(&args[4], 0, 1_000_000)?);
    let delay = Duration::from_micros(number(&args[5], 0, 1_000_000)?);
    let count = producers as u64 * per;
    let root = Path::new(&args[0]).join(format!(
        "skrin-group-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root)?;
    let work = || -> Result<()> {
        let path = root.join("database");
        let db = CatalogDatabase::<Banking>::create_dir(&path)?;
        seed(&db)?;
        db.write(|tx| {
            tx.update::<Accounts>(1, |row| {
                Ok(Account {
                    email: row.email.clone(),
                    balance: count + 100,
                })
            })
        })?;
        println!(
            "config,mode={},producers={producers},per_producer={per},inter_request_us={},group_delay_us={},queue_capacity=64,max_group_requests=16,durability=sync_all,retained_versions=0,root={}",
            args[1],
            interval.as_micros(),
            delay.as_micros(),
            root.display()
        );
        let client = if args[1] == "immediate" {
            Client::Immediate(Arc::new(db))
        } else {
            Client::Group(db.into_group_commit(GroupCommitOptions {
                max_delay: delay,
                ..Default::default()
            })?)
        };
        let barrier = Arc::new(Barrier::new(producers + 1));
        let mut threads = Vec::new();
        for producer in 0..producers {
            let client = client.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || -> Result<Vec<Sample>> {
                let mut samples = Vec::new();
                barrier.wait();
                for n in 0..per {
                    samples.push(client.write(producer as u64 * per + n + 1)?);
                    if !interval.is_zero() && n + 1 < per {
                        std::thread::sleep(interval);
                    }
                }
                Ok(samples)
            }));
        }
        let start = Instant::now();
        barrier.wait();
        let mut samples = Vec::new();
        for thread in threads {
            samples.extend(
                thread
                    .join()
                    .map_err(|_| Error::InvalidOperation("benchmark producer panicked".into()))??,
            );
        }
        let elapsed = start.elapsed();
        println!(
            "workload,transactions={count},elapsed_s={:.6},tx_per_s={:.3}",
            elapsed.as_secs_f64(),
            count as f64 / elapsed.as_secs_f64()
        );
        distribution("latency", samples.iter().map(|s| s.latency));
        distribution("call_to_callback", samples.iter().map(|s| s.callback_wait));
        distribution(
            "admission_to_callback",
            samples.iter().filter_map(|s| s.admitted_wait),
        );
        let mut groups = BTreeMap::new();
        for sample in &samples {
            if let Some(previous) = groups.insert(sample.group, sample.frames) {
                assert_eq!(previous, sample.frames);
            }
        }
        let mut sizes: Vec<_> = groups.values().copied().collect();
        sizes.sort();
        println!(
            "groups,sync_groups={},mean_frames={:.3},max_frames={},histogram={:?}",
            groups.len(),
            count as f64 / groups.len() as f64,
            sizes.last().unwrap(),
            sizes.iter().fold(BTreeMap::new(), |mut hist, size| {
                *hist.entry(*size).or_insert(0usize) += 1;
                hist
            })
        );
        memory();
        let db = client.into_database()?;
        verify(&db, count)?;
        let before = disk(&path)?;
        let start = Instant::now();
        let cp = db.checkpoint()?;
        let cp_ms = start.elapsed().as_secs_f64() * 1e3;
        let staged = disk(&path)?;
        let start = Instant::now();
        db.reclaim()?;
        let reclaim_ms = start.elapsed().as_secs_f64() * 1e3;
        let retained = disk(&path)?;
        println!(
            "maintenance,sequence={},checkpoint_ms={cp_ms:.3},reclaim_ms={reclaim_ms:.3},snapshot_bytes={},before={before:?},staged={staged:?},retained={retained:?}",
            cp.sequence, cp.snapshot_bytes
        );
        drop(db);
        memory();
        let child = std::process::Command::new(std::env::current_exe()?)
            .arg("--reopen")
            .arg(&path)
            .arg(count.to_string())
            .output()?;
        print!("{}", String::from_utf8_lossy(&child.stdout));
        if !child.status.success() {
            return Err(Error::InvalidOperation(format!(
                "reopen child failed: {}",
                String::from_utf8_lossy(&child.stderr)
            )));
        }
        Ok(())
    };
    let result = work();
    if result.is_ok() {
        fs::remove_dir_all(root)?;
    }
    result
}
