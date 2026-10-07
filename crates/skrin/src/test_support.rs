use crate::log::Storage;
use crate::{Decoder, Encoder, Record, Result, Schema};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Item(pub(crate) u64);

impl Record for Item {
    const SCHEMA: Schema = Schema {
        table_id: 7,
        version: 1,
    };

    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.u64(self.0)
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(decoder.u64()?))
    }
}

struct Disk {
    cursor: Cursor<Vec<u8>>,
    write_remaining: Option<usize>,
    sync_error: Option<io::ErrorKind>,
    write_error: io::ErrorKind,
    read_error: bool,
    truncate_error: bool,
    syncs: usize,
}

/// One shared test device, beneath the production WAL implementation.
#[derive(Clone)]
pub(crate) struct TestStorage(Arc<Mutex<Disk>>);

impl TestStorage {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(Mutex::new(Disk {
            cursor: Cursor::new(bytes),
            write_remaining: None,
            sync_error: None,
            write_error: io::ErrorKind::Other,
            read_error: false,
            truncate_error: false,
            syncs: 0,
        })))
    }

    pub(crate) fn image(&self) -> Vec<u8> {
        self.0.lock().unwrap().cursor.get_ref().clone()
    }

    pub(crate) fn fail_write_after(&self, bytes: usize) {
        let mut disk = self.0.lock().unwrap();
        disk.write_remaining = Some(bytes);
        disk.write_error = io::ErrorKind::Other;
    }

    pub(crate) fn fail_sync(&self) {
        self.0.lock().unwrap().sync_error = Some(io::ErrorKind::Other);
    }

    pub(crate) fn fail_enospc_after(&self, bytes: usize) {
        let mut disk = self.0.lock().unwrap();
        disk.write_remaining = Some(bytes);
        disk.write_error = io::ErrorKind::StorageFull;
    }
    pub(crate) fn fail_sync_enospc(&self) {
        self.0.lock().unwrap().sync_error = Some(io::ErrorKind::StorageFull);
    }

    pub(crate) fn fail_read(&self) {
        self.0.lock().unwrap().read_error = true;
    }

    pub(crate) fn fail_truncate(&self) {
        self.0.lock().unwrap().truncate_error = true;
    }

    pub(crate) fn clear_faults(&self) {
        let mut disk = self.0.lock().unwrap();
        disk.write_remaining = None;
        disk.sync_error = None;
        disk.read_error = false;
        disk.truncate_error = false;
    }

    pub(crate) fn syncs(&self) -> usize {
        self.0.lock().unwrap().syncs
    }
}

impl Read for TestStorage {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut disk = self.0.lock().unwrap();
        if disk.read_error {
            return Err(io::Error::other("injected read failure"));
        }
        disk.cursor.read(bytes)
    }
}

impl Write for TestStorage {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut disk = self.0.lock().unwrap();
        let count = if let Some(remaining) = &mut disk.write_remaining {
            if *remaining == 0 && !bytes.is_empty() {
                return Err(io::Error::new(disk.write_error, "injected write failure"));
            }
            let count = bytes.len().min(*remaining);
            *remaining -= count;
            count
        } else {
            bytes.len()
        };
        disk.cursor.write(&bytes[..count])
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for TestStorage {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.lock().unwrap().cursor.seek(position)
    }
}

impl Storage for TestStorage {
    fn size(&self) -> io::Result<u64> {
        Ok(self.0.lock().unwrap().cursor.get_ref().len() as u64)
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        let mut disk = self.0.lock().unwrap();
        if disk.truncate_error {
            return Err(io::Error::other("injected truncate failure"));
        }
        disk.cursor.get_mut().resize(len as usize, 0);
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        let mut disk = self.0.lock().unwrap();
        if let Some(kind) = disk.sync_error {
            return Err(io::Error::new(kind, "injected sync failure"));
        }
        disk.syncs += 1;
        Ok(())
    }
}
