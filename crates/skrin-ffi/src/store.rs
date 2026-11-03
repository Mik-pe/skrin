//! Safe ownership/staging adapter; no Rust guard outlives a C call.
#![forbid(unsafe_code)]

use crate::{Failure, Result, SKRIN_BUSY, SKRIN_PANIC};
use skrin::{Database, Decoder, Encoder, Record, Schema};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct Bytes(pub(crate) Vec<u8>);
impl Record for Bytes {
    const SCHEMA: Schema = Schema {
        table_id: 0x534b_5243_4259_5431, // SKRCBYT1, byte-record adapter v1.
        version: 1,
    };
    fn encode(&self, encoder: &mut Encoder) -> skrin::Result<()> {
        encoder.bytes(&self.0)
    }
    fn decode(decoder: &mut Decoder<'_>) -> skrin::Result<Self> {
        Ok(Self(decoder.bytes()?.to_vec()))
    }
}

pub(crate) struct Inner {
    pub(crate) engine: Database<Bytes>,
    writer: AtomicBool,
    failed: AtomicBool,
}
impl Inner {
    pub(crate) fn new(engine: Database<Bytes>) -> Arc<Self> {
        Arc::new(Self {
            engine,
            writer: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        })
    }
    pub(crate) fn run<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.failed.load(Ordering::Acquire) {
            return Err(skrin::Error::Poisoned.into());
        }
        match catch_unwind(AssertUnwindSafe(operation)) {
            Ok(result) => result,
            Err(_) => {
                self.failed.store(true, Ordering::Release);
                Err(Failure::new(
                    SKRIN_PANIC,
                    "Rust panic; close and reopen the database",
                ))
            }
        }
    }
    pub(crate) fn lease(self: &Arc<Self>) -> Result<Lease> {
        self.writer
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Failure::new(SKRIN_BUSY, "a write or maintenance operation is active"))?;
        let lease = Lease(Arc::clone(self));
        // Refuse poisoned engine state even for an otherwise empty transaction.
        self.engine.stats()?;
        Ok(lease)
    }
}

pub(crate) struct Lease(pub(crate) Arc<Inner>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.writer.store(false, Ordering::Release);
    }
}

pub(crate) struct Write {
    pub(crate) lease: Lease,
    changes: BTreeMap<u64, Option<Bytes>>,
}
impl Write {
    pub(crate) fn new(lease: Lease) -> Self {
        Self {
            lease,
            changes: BTreeMap::new(),
        }
    }
    pub(crate) fn get<T>(
        &self,
        key: u64,
        use_value: impl FnOnce(Option<&[u8]>) -> Result<T>,
    ) -> Result<T> {
        match self.changes.get(&key) {
            Some(row) => use_value(row.as_ref().map(|r| r.0.as_slice())),
            None => {
                let read = self.lease.0.engine.read()?;
                use_value(read.get(key).map(|r| r.0.as_slice()))
            }
        }
    }
    pub(crate) fn put(&mut self, key: u64, value: Vec<u8>, insert: bool) -> Result<()> {
        if insert && self.get(key, |row| Ok(row.is_some()))? {
            return Err(skrin::Error::DuplicateKey(key).into());
        }
        self.changes.insert(key, Some(Bytes(value)));
        Ok(())
    }
    pub(crate) fn remove(&mut self, key: u64) -> Result<bool> {
        if !self.get(key, |row| Ok(row.is_some()))? {
            return Ok(false);
        }
        self.changes.insert(key, None);
        Ok(true)
    }
    pub(crate) fn commit(mut self) -> Result<()> {
        let db = Arc::clone(&self.lease.0);
        db.run(|| {
            let changes = std::mem::take(&mut self.changes);
            db.engine.write(|tx| {
                for (key, row) in changes {
                    match row {
                        Some(row) => tx.put(key, row),
                        None => {
                            tx.remove(key);
                        }
                    }
                }
                Ok(())
            })?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SKRIN_BUSY, SKRIN_POISONED};

    #[test]
    fn adapter_codec_is_explicit_and_versioned() {
        assert_eq!(
            Bytes::SCHEMA,
            Schema {
                table_id: 0x534b_5243_4259_5431,
                version: 1
            }
        );
        let value = Bytes(vec![0, 195, 169]);
        let mut encoder = Encoder::default();
        value.encode(&mut encoder).unwrap();
        // Independent u32 little-endian length followed by unchanged bytes.
        let expected = [3, 0, 0, 0, 0, 195, 169];
        assert_eq!(encoder.finish(), expected);
        let mut decoder = Decoder::new(&expected);
        assert_eq!(Bytes::decode(&mut decoder).unwrap().0, [0, 195, 169]);
        decoder.finish().unwrap();
        for end in 0..expected.len() {
            assert!(Bytes::decode(&mut Decoder::new(&expected[..end])).is_err());
        }
    }

    #[test]
    fn panic_poison_and_unwind_release_writer_admission() {
        let db = Inner::new(Database::in_memory());
        let result = db.run(|| -> Result<()> {
            let _lease = db.lease()?;
            assert_eq!(db.lease().err().unwrap().code, SKRIN_BUSY);
            panic!("controlled boundary panic");
        });
        assert_eq!(result.err().unwrap().code, SKRIN_PANIC);
        assert!(!db.writer.load(Ordering::Acquire));
        assert_eq!(db.run(|| Ok(())).err().unwrap().code, SKRIN_POISONED);
    }
}
