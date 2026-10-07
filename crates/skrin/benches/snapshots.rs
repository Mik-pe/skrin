//! Equivalent durable transfers with borrowed versus immutable read versions.
#[path = "../examples/support/banking.rs"]
mod banking;
use banking::*;
use skrin::catalog::{CatalogDatabase, CatalogRead, Table};
use skrin::group_commit::{GroupCommitCatalog, GroupCommitOptions};
use skrin::versioned::{
    CatalogSnapshot, CatalogSnapshotWrite, GroupSnapshotCatalog, RetentionStats, SnapshotCatalog,
    SnapshotOptions,
};
use skrin::{Error, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

fn footprint(row: &Row) -> Result<u64> {
    Ok(std::mem::size_of::<Row>() as u64
        + match row {
            Row::Account(r) => r.email.capacity() as u64,
            Row::Transfer(_) => 0,
        })
}
fn versioned_transfer(tx: &mut CatalogSnapshotWrite<'_, Banking>, id: u64) -> Result<()> {
    tx.update::<Accounts>(1, |r| {
        Ok(Account {
            email: r.email.clone(),
            balance: r.balance - 1,
        })
    })?;
    tx.update::<Accounts>(2, |r| {
        Ok(Account {
            email: r.email.clone(),
            balance: r.balance + 1,
        })
    })?;
    tx.insert::<Transfers>(
        id,
        Transfer {
            from: 1,
            to: 2,
            amount: 1,
        },
    )
}
#[derive(Clone)]
enum Client {
    Immediate(Arc<CatalogDatabase<Banking>>),
    Group(GroupCommitCatalog<Banking>),
    Snapshot(SnapshotCatalog<Banking>),
    GroupSnapshot(GroupSnapshotCatalog<Banking>),
}
struct Sample {
    latency: Duration,
    callback_wait: Duration,
    admitted_wait: Option<Duration>,
    group: u64,
    frames: usize,
    retention: Option<RetentionStats>,
}
enum ReadView<'a> {
    Borrowed(CatalogRead<'a, Banking>),
    Version(CatalogSnapshot<Banking>),
}
impl ReadView<'_> {
    fn sequence(&self) -> Result<u64> {
        match self {
            Self::Borrowed(r) => Ok(r.sequence()),
            Self::Version(r) => r.sequence(),
        }
    }
    fn get<T: Table<Banking>>(&self, key: u64) -> Result<Option<&T::Record>> {
        match self {
            Self::Borrowed(r) => r.get::<T>(key),
            Self::Version(r) => r.get::<T>(key),
        }
    }
    fn lookup<T: Table<Banking>>(&self, id: u64, key: &[u8]) -> Result<Vec<(u64, &T::Record)>> {
        match self {
            Self::Borrowed(r) => r.lookup::<T>(id, key),
            Self::Version(r) => r.lookup::<T>(id, key),
        }
    }
    fn verify(&self, count: u64) -> Result<()> {
        let progress = self.sequence()?.checked_sub(2).expect("seed sequence");
        assert!(progress <= count);
        let sender = self.get::<Accounts>(1)?.unwrap();
        let receiver = self.get::<Accounts>(2)?.unwrap();
        assert_eq!(sender.balance, count + 100 - progress);
        assert_eq!(receiver.balance, 100 + progress);
        assert_eq!(self.lookup::<Accounts>(1, b"alice@example.test")?[0].0, 1);
        assert!(
            self.lookup::<Accounts>(2, &sender.balance.to_be_bytes())?
                .iter()
                .any(|(key, row)| *key == 1 && row.balance == sender.balance)
        );
        if let Some(row) = self.get::<Transfers>(1)? {
            assert_eq!((row.from, row.to, row.amount), (1, 2, 1));
        }
        Ok(())
    }
}
impl Client {
    fn view(&self) -> Result<ReadView<'_>> {
        Ok(match self {
            Self::Immediate(db) => ReadView::Borrowed(db.read()?),
            Self::Group(db) => ReadView::Borrowed(db.read()?),
            Self::Snapshot(db) => ReadView::Version(db.snapshot()?),
            Self::GroupSnapshot(db) => ReadView::Version(db.snapshot()?),
        })
    }
    fn checkpoint(&self) -> Result<skrin::Checkpoint> {
        match self {
            Self::Immediate(db) => db.checkpoint(),
            Self::Group(db) => db.checkpoint(),
            Self::Snapshot(db) => db.checkpoint(),
            Self::GroupSnapshot(db) => db.checkpoint(),
        }
    }
    fn reclaim(&self) -> Result<skrin::ReclaimReport> {
        match self {
            Self::Immediate(db) => db.reclaim(),
            Self::Group(db) => db.reclaim(),
            Self::Snapshot(db) => db.reclaim(),
            Self::GroupSnapshot(db) => db.reclaim(),
        }
    }
    fn verify(&self, count: u64) -> Result<()> {
        let view = self.view()?;
        view.verify(count)?;
        assert_eq!(view.sequence()?, count + 2);
        let transfers = match &view {
            ReadView::Borrowed(r) => r.scan::<Transfers>()?.count(),
            ReadView::Version(r) => r.scan::<Transfers>()?.count(),
        };
        assert_eq!(transfers as u64, count);
        for id in 1..=count {
            let row = view.get::<Transfers>(id)?.unwrap();
            assert_eq!((row.from, row.to, row.amount), (1, 2, 1));
        }
        Ok(())
    }
    fn retention(&self) -> Result<Option<RetentionStats>> {
        match self {
            Self::Snapshot(db) => db.retention().map(Some),
            Self::GroupSnapshot(db) => db.retention().map(Some),
            _ => Ok(None),
        }
    }
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
            Self::Snapshot(db) => {
                let wait = db.write(|tx| {
                    let wait = start.elapsed();
                    versioned_transfer(tx, id)?;
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
            Self::GroupSnapshot(db) => {
                let receipt = db
                    .submit(move |tx| {
                        let wait = start.elapsed();
                        versioned_transfer(tx, id)?;
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
        let latency = start.elapsed();
        Ok(Sample {
            latency,
            callback_wait,
            admitted_wait,
            group,
            frames,
            retention: self.retention()?,
        })
    }
    fn into_database(self) -> Result<CatalogDatabase<Banking>> {
        match self {
            Self::Immediate(db) => Arc::try_unwrap(db).map_err(|_| Error::Busy),
            Self::Group(db) => db.into_database(),
            Self::Snapshot(db) => db.into_database(),
            Self::GroupSnapshot(db) => db.into_database(),
        }
    }
}
struct ReadSample {
    acquire: Duration,
    verify: Duration,
    retention: Option<RetentionStats>,
}
fn read_loop(
    client: Client,
    count: u64,
    barrier: Arc<Barrier>,
    stop: Arc<AtomicBool>,
    hold: Duration,
    interval: Duration,
) -> Result<Vec<ReadSample>> {
    let mut samples = Vec::new();
    // All modes enter the timed workload with the same initial read view held.
    let first_start = Instant::now();
    let first_view = client.view()?;
    let mut first = Some((first_view, first_start.elapsed()));
    barrier.wait();
    loop {
        let (view, acquire) = match first.take() {
            Some(first) => first,
            None => {
                let start = Instant::now();
                let view = client.view()?;
                (view, start.elapsed())
            }
        };
        let verify_start = Instant::now();
        view.verify(count)?;
        let verify = acquire + verify_start.elapsed();
        let retention = client.retention()?;
        if !hold.is_zero() {
            std::thread::sleep(hold);
        }
        drop(view);
        if samples.len() == 1_000_000 {
            return Err(Error::InvalidOperation(
                "reader sample capacity exceeded".into(),
            ));
        }
        samples.push(ReadSample {
            acquire,
            verify,
            retention,
        });
        if stop.load(Ordering::Acquire) {
            break;
        }
        if !interval.is_zero() {
            std::thread::sleep(interval);
        }
    }
    Ok(samples)
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
    if args.len() != 9
        || !matches!(
            args[1].as_str(),
            "immediate" | "group" | "snapshot" | "group_snapshot"
        )
    {
        return Err(Error::InvalidOperation("usage: snapshots EXISTING_PARENT immediate|group|snapshot|group_snapshot PRODUCERS TX_PER_PRODUCER INTER_REQUEST_US GROUP_DELAY_US READERS READ_HOLD_US READ_INTERVAL_US".into()));
    }
    let producers = number(&args[2], 1, 32)? as usize;
    let per = number(&args[3], 1, 10_000)?;
    let interval = Duration::from_micros(number(&args[4], 0, 1_000_000)?);
    let delay = Duration::from_micros(number(&args[5], 0, 1_000_000)?);
    let readers = number(&args[6], 1, 8)? as usize;
    let hold = Duration::from_micros(number(&args[7], 0, 1_000_000)?);
    let read_interval = Duration::from_micros(number(&args[8], 0, 1_000_000)?);
    let count = producers as u64 * per;
    let root = Path::new(&args[0]).join(format!(
        "skrin-snapshots-{}-{}",
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
            tx.update::<Accounts>(1, |r| {
                Ok(Account {
                    email: r.email.clone(),
                    balance: count + 100,
                })
            })
        })?;
        let options = GroupCommitOptions {
            max_delay: delay,
            ..Default::default()
        };
        let snapshots = SnapshotOptions {
            max_snapshots: 32,
            max_pinned_bytes: 512 * 1024 * 1024,
        };
        let client = match args[1].as_str() {
            "immediate" => Client::Immediate(Arc::new(db)),
            "group" => Client::Group(db.into_group_commit(options)?),
            "snapshot" => Client::Snapshot(db.into_snapshots(snapshots, footprint)?),
            "group_snapshot" => Client::GroupSnapshot(
                db.into_snapshots(snapshots, footprint)?
                    .into_group_commit(options)?,
            ),
            _ => unreachable!(),
        };
        println!(
            "config,mode={},producers={producers},per_producer={per},inter_request_us={},group_delay_us={},readers={readers},read_hold_us={},read_interval_us={},queue_capacity=64,max_group_requests=16,max_snapshots=32,max_pinned_bytes=536870912,initial_read_capture_before_timing=true,durability=sync_all,root={}",
            args[1],
            interval.as_micros(),
            delay.as_micros(),
            hold.as_micros(),
            read_interval.as_micros(),
            root.display()
        );
        let barrier = Arc::new(Barrier::new(producers + readers + 1));
        let stop = Arc::new(AtomicBool::new(false));
        let mut reading = Vec::new();
        let mut writing = Vec::new();
        for _ in 0..readers {
            let client = client.clone();
            let barrier = barrier.clone();
            let stop = stop.clone();
            reading.push(std::thread::spawn(move || {
                read_loop(client, count, barrier, stop, hold, read_interval)
            }));
        }
        for producer in 0..producers {
            let client = client.clone();
            let barrier = barrier.clone();
            writing.push(std::thread::spawn(move || -> Result<Vec<Sample>> {
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
        let mut failure = None;
        for thread in writing {
            match thread.join() {
                Ok(Ok(values)) => samples.extend(values),
                Ok(Err(error)) => failure = Some(error),
                Err(_) => failure = Some(Error::InvalidOperation("producer panicked".into())),
            }
        }
        let elapsed = start.elapsed();
        stop.store(true, Ordering::Release);
        let mut reads = Vec::new();
        for thread in reading {
            match thread.join() {
                Ok(Ok(values)) => reads.extend(values),
                Ok(Err(error)) => failure = Some(error),
                Err(_) => failure = Some(Error::InvalidOperation("reader panicked".into())),
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        assert_eq!(samples.len() as u64, count);
        println!(
            "workload,transactions={count},elapsed_s={:.6},tx_per_s={:.3},read_samples={},reads_per_s={:.3}",
            elapsed.as_secs_f64(),
            count as f64 / elapsed.as_secs_f64(),
            reads.len(),
            reads.len() as f64 / elapsed.as_secs_f64()
        );
        distribution("latency", samples.iter().map(|s| s.latency));
        distribution("call_to_callback", samples.iter().map(|s| s.callback_wait));
        distribution(
            "admission_to_callback",
            samples.iter().filter_map(|s| s.admitted_wait),
        );
        distribution("read_acquire", reads.iter().map(|s| s.acquire));
        distribution("read_acquire_verify", reads.iter().map(|s| s.verify));
        let mut groups = BTreeMap::new();
        for sample in &samples {
            if let Some(previous) = groups.insert(sample.group, sample.frames) {
                assert_eq!(previous, sample.frames);
            }
        }
        let sizes: Vec<_> = groups.values().copied().collect();
        println!(
            "groups,sync_groups={},mean_frames={:.3},max_frames={},histogram={:?}",
            groups.len(),
            count as f64 / groups.len() as f64,
            sizes.iter().max().unwrap(),
            sizes.iter().fold(BTreeMap::new(), |mut hist, size| {
                *hist.entry(*size).or_insert(0usize) += 1;
                hist
            })
        );
        let retention: Vec<_> = samples
            .iter()
            .filter_map(|s| s.retention)
            .chain(reads.iter().filter_map(|s| s.retention))
            .collect();
        println!(
            "retention,sampled_max_pins={},sampled_max_versions={},sampled_max_pinned_bytes={},sampled_oldest_sequence={:?},final={:?},accounting=whole_root_per_lease_allocator_overhead_excluded",
            retention.iter().map(|r| r.snapshots).max().unwrap_or(0),
            retention
                .iter()
                .map(|r| r.pinned_versions)
                .max()
                .unwrap_or(0),
            retention.iter().map(|r| r.pinned_bytes).max().unwrap_or(0),
            retention
                .iter()
                .filter_map(|r| r.oldest_pinned_sequence)
                .min(),
            client.retention()?
        );
        memory();
        client.verify(count)?;
        let before = disk(&path)?;
        let start = Instant::now();
        let cp = client.checkpoint()?;
        let cp_ms = start.elapsed().as_secs_f64() * 1e3;
        let staged = disk(&path)?;
        let start = Instant::now();
        client.reclaim()?;
        let reclaim_ms = start.elapsed().as_secs_f64() * 1e3;
        let retained = disk(&path)?;
        println!(
            "maintenance,sequence={},checkpoint_ms={cp_ms:.3},reclaim_ms={reclaim_ms:.3},snapshot_bytes={},before={before:?},staged={staged:?},retained={retained:?},readers_released=true",
            cp.sequence, cp.snapshot_bytes
        );
        memory();
        let conversion = Instant::now();
        let db = client.into_database()?;
        println!(
            "conversion,native_ms={:.3},outside_workload_and_maintenance=true",
            conversion.elapsed().as_secs_f64() * 1e3
        );
        verify(&db, count)?;
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
