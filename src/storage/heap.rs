//! The heap file of one table: an append-only array of [`Page`]s with an
//! in-memory set of pages modified by the current transaction.
//!
//! Uncommitted pages never reach the file: the caller commits through the WAL
//! ([`crate::storage::wal`]) and only then flushes. Rolling back is therefore
//! just dropping the modified pages.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::page::{Page, PageView, PAGE_SIZE};

/// Resident transaction pages are bounded by bytes, not by count, so shrinking
/// the page size does not make a large transaction spill eight times earlier.
const DIRTY_BUDGET: usize = 1024 * 1024;

/// Where a record lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rid {
    pub page: u32,
    pub slot: u16,
}

pub struct HeapFile {
    path: PathBuf,
    file: File,
    num_pages: u32,
    /// Pages touched by the current transaction, in page order.
    dirty: BTreeMap<u32, Page>,
    /// Page that still had room at the last insert.
    open_page: Option<u32>,
    scratch: Vec<u8>,
    spill: File,
    spilled: BTreeSet<u32>,
}

impl HeapFile {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let len = file.metadata()?.len();
        // A killed heap write may leave a partial final page. WAL replay
        // repairs it before any row is inspected.
        let spill = OpenOptions::new().read(true).write(true).create(true)
            .truncate(true).open(path.with_extension("txn"))?;
        Ok(Self {
            path,
            file,
            num_pages: len.div_ceil(PAGE_SIZE as u64) as u32,
            dirty: BTreeMap::new(),
            open_page: None,
            scratch: vec![0u8; PAGE_SIZE],
            spill,
            spilled: BTreeSet::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn num_pages(&self) -> u32 {
        self.num_pages
    }

    /// Reads a page, preferring the current transaction's version.
    pub fn read_page(&mut self, id: u32) -> io::Result<Page> {
        if let Some(page) = self.dirty.get(&id) {
            return Ok(page.clone());
        }
        if self.spilled.contains(&id) {
            self.spill.seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
            self.spill.read_exact(&mut self.scratch)?;
            return Page::from_bytes(&self.scratch).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid transaction page"));
        }
        self.read_from_disk(id)
    }

    fn read_from_disk(&mut self, id: u32) -> io::Result<Page> {
        let offset = id as u64 * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; PAGE_SIZE];
        self.file.read_exact(&mut buf)?;
        Page::from_bytes(&buf).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("corrupt page {id} in {}", self.path.display()),
            )
        })
    }

    /// The writable version of a page, loading it into the transaction's set.
    pub fn page_mut(&mut self, id: u32) -> io::Result<&mut Page> {
        if !self.dirty.contains_key(&id) {
            let page = if id < self.num_pages || self.spilled.contains(&id) {
                self.read_page(id)?
            } else {
                Page::new()
            };
            // Bound resident transaction pages independently of table size.
            if self.dirty.len() >= DIRTY_BUDGET / PAGE_SIZE {
                let (&victim, _) = self.dirty.first_key_value().unwrap();
                let old = self.dirty.remove(&victim).unwrap();
                self.spill.seek(SeekFrom::Start(victim as u64 * PAGE_SIZE as u64))?;
                self.spill.write_all(old.as_bytes())?;
                self.spilled.insert(victim);
            }
            self.dirty.insert(id, page);
        }
        Ok(self.dirty.get_mut(&id).unwrap())
    }

    /// Replaces a page with a recovered image, growing the file when needed.
    pub fn put_page(&mut self, id: u32, page: Page) {
        if id >= self.num_pages {
            self.num_pages = id + 1;
        }
        self.dirty.insert(id, page);
    }

    /// Appends a record, allocating a page when the tail of the heap is full.
    /// Returns `None` when the record is larger than a page can hold.
    pub fn insert(&mut self, payload: &[u8]) -> io::Result<Option<Rid>> {
        if payload.len() > super::page::MAX_RECORD_SIZE {
            let first = self.num_pages;
            let chunks = payload.chunks(PAGE_SIZE - 16);
            let count = chunks.len();
            for (i, chunk) in chunks.enumerate() {
                let id = first + i as u32;
                let next = (i + 1 < count).then_some(id + 1);
                *self.page_mut(id)? = Page::overflow(chunk, next, i == 0);
                self.num_pages = id + 1;
            }
            return Ok(Some(Rid { page: first, slot: 0 }));
        }
        // The heap is append-only between explicit compactions. Probe only
        // its tail, never rescan old full pages after a rollback or restart.
        let tail = self.open_page.or_else(|| self.num_pages.checked_sub(1));
        if let Some(id) = tail {
            if let Some(slot) = self.page_mut(id)?.insert(payload) {
                self.open_page = Some(id);
                return Ok(Some(Rid { page: id, slot }));
            }
        }
        let id = self.num_pages;
        let slot = self
            .page_mut(id)?
            .insert(payload)
            .expect("a fresh page fits any valid record");
        self.num_pages += 1;
        self.open_page = Some(id);
        Ok(Some(Rid { page: id, slot }))
    }

    pub fn record(&mut self, rid: Rid) -> io::Result<Option<Vec<u8>>> {
        if rid.page >= self.num_pages && !self.dirty.contains_key(&rid.page) {
            return Ok(None);
        }
        let page = self.read_page(rid.page)?;
        if page.is_overflow() {
            if rid.slot != 0 || !page.is_overflow_live() { return Ok(None); }
            let mut payload = page.overflow_payload().to_vec();
            let mut next = page.overflow_next();
            let mut visited = 0;
            while let Some(id) = next {
                if id >= self.num_pages || visited >= self.num_pages { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid overflow chain")); }
                let part = self.read_page(id)?;
                if !part.is_overflow() || part.is_overflow_head() { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid overflow continuation")); }
                payload.extend_from_slice(part.overflow_payload());
                next = part.overflow_next(); visited += 1;
            }
            return Ok(Some(payload));
        }
        Ok(page.record(rid.slot).map(|r| r.to_vec()))
    }

    pub fn tombstone(&mut self, rid: Rid) -> io::Result<bool> {
        if rid.page >= self.num_pages {
            return Ok(false);
        }
        Ok(self.page_mut(rid.page)?.tombstone(rid.slot))
    }

    pub fn revive(&mut self, rid: Rid) -> io::Result<bool> {
        if rid.page >= self.num_pages {
            return Ok(false);
        }
        Ok(self.page_mut(rid.page)?.revive(rid.slot))
    }

    /// Iterates every live record without keeping pages in memory.
    pub fn for_each_live<F>(&mut self, mut visit: F) -> io::Result<()>
    where
        F: FnMut(u32, u16, &[u8]) -> io::Result<()>,
    {
        let pages = self.num_pages;
        for id in 0..pages {
            // One reusable buffer serves the whole scan: no per-page
            // allocation and no 8 KiB copy per page.
            if let Some(page) = self.dirty.get(&id) {
                self.scratch.copy_from_slice(page.as_bytes());
            } else if self.spilled.contains(&id) {
                self.spill.seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
                self.spill.read_exact(&mut self.scratch)?;
            } else {
                self.file.seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
                self.file.read_exact(&mut self.scratch)?;
            }
            let view = PageView::new(&self.scratch).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt page {id} in {}", self.path.display()),
                )
            })?;
            if view.is_overflow() {
                if view.is_overflow_live() {
                    if let Some(record) = self.record(Rid { page: id, slot: 0 })? {
                        visit(id, 0, &record)?;
                    }
                }
                continue;
            }
            for (slot, record) in view.live_records() {
                visit(id, slot, record)?;
            }
        }
        Ok(())
    }

    /// Snapshot of every page modified by the current transaction.
    pub fn dirty_pages(&self) -> impl Iterator<Item = (u32, &[u8])> {
        self.dirty.iter().map(|(id, page)| (*id, page.as_bytes()))
    }

    pub fn has_dirty(&self) -> bool {
        !self.dirty.is_empty() || !self.spilled.is_empty()
    }

    pub fn for_each_dirty<F>(&mut self, mut visit: F) -> io::Result<()>
    where F: FnMut(u32, &[u8]) -> io::Result<()> {
        for id in &self.spilled {
            if self.dirty.contains_key(id) { continue; }
            self.spill.seek(SeekFrom::Start(*id as u64 * PAGE_SIZE as u64))?;
            self.spill.read_exact(&mut self.scratch)?;
            visit(*id, &self.scratch)?;
        }
        for (id, page) in &self.dirty { visit(*id, page.as_bytes())?; }
        Ok(())
    }

    pub fn apply_recovered(&mut self, id: u32, image: &[u8]) -> io::Result<()> {
        if Page::from_bytes(image).is_none() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid recovered page"));
        }
        self.file.seek(SeekFrom::Start(id as u64 * PAGE_SIZE as u64))?;
        self.file.write_all(image)?;
        self.num_pages = self.num_pages.max(id + 1);
        Ok(())
    }

    /// Writes modified pages to the file and drops them from memory. The caller
    /// must have made the matching WAL records durable first.
    pub fn flush(&mut self) -> io::Result<()> {
        self.write_dirty()?;
        self.file.sync_all()
    }

    /// Writes modified pages to the file without flushing them to the device.
    ///
    /// Safe only once the log holds the same images: a crash may leave a torn
    /// page on disk, and recovery rewrites it from the log.
    pub fn write_dirty(&mut self) -> io::Result<()> {
        for id in &self.spilled {
            if self.dirty.contains_key(id) { continue; }
            self.spill.seek(SeekFrom::Start(*id as u64 * PAGE_SIZE as u64))?;
            self.spill.read_exact(&mut self.scratch)?;
            self.file.seek(SeekFrom::Start(*id as u64 * PAGE_SIZE as u64))?;
            self.file.write_all(&self.scratch)?;
        }
        self.spilled.clear();
        self.spill.set_len(0)?;
        for (id, page) in std::mem::take(&mut self.dirty) {
            self.scratch.copy_from_slice(page.as_bytes());
            let offset = id as u64 * PAGE_SIZE as u64;
            self.file.seek(SeekFrom::Start(offset))?;
            self.file.write_all(&self.scratch)?;
        }
        Ok(())
    }

    /// Pushes everything written so far to the device.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }

    /// Drops uncommitted changes; the next read comes from the file again.
    pub fn discard_changes(&mut self) {
        self.dirty.clear();
        self.spilled.clear();
        let _ = self.spill.set_len(0);
        self.open_page = None;
        // Pages allocated since the last commit exist only in memory, so taking
        // the page count from the file keeps reads from running past its end.
        if let Ok(meta) = self.file.metadata() {
            self.num_pages = (meta.len() / PAGE_SIZE as u64) as u32;
        }
    }

    /// Rewrites the file from scratch, dropping tombstones and shrinking it.
    ///
    /// The live records are streamed into a sibling `*.heap.tmp` file, which is
    /// fsynced and then renamed over the original, so a crash leaves either the
    /// old file or the new one, never a half-written heap. Pages buffered by
    /// the current transaction are included, which commits them to the file;
    /// the caller must persist the matching index state.
    pub fn compact<F>(&mut self, mut keep: F) -> io::Result<()>
    where
        F: FnMut(u32, u16, &[u8]) -> io::Result<bool>,
    {
        let tmp_path = tmp_path(&self.path);
        if tmp_path.exists() { std::fs::remove_file(&tmp_path)?; }
        let mut out = HeapFile::open(tmp_path.clone())?;
        self.for_each_live(|id, slot, record| {
            if keep(id, slot, record)? {
                out.insert(record)?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cannot compact row"))?;
            }
            Ok(())
        })?;
        let written = out.num_pages;
        out.flush()?;
        drop(out);
        std::fs::rename(&tmp_path, &self.path)?;
        sync_parent(&self.path)?;

        self.file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        self.num_pages = written;
        self.dirty.clear();
        self.open_page = None;
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Makes a directory entry change durable on filesystems that need it.
fn sync_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("mvdb-heap-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn inserts_survive_flush_and_reopen() {
        let dir = TempDir::new();
        let path = dir.file("t.heap");
        let mut heap = HeapFile::open(&path).unwrap();
        let mut rids = Vec::new();
        for i in 0..1000u32 {
            let payload = format!("row-{i:04}");
            rids.push(heap.insert(payload.as_bytes()).unwrap().unwrap());
        }
        assert!(heap.num_pages() >= 1);
        heap.flush().unwrap();
        drop(heap);

        let mut heap = HeapFile::open(&path).unwrap();
        for (i, rid) in rids.iter().enumerate() {
            let got = heap.record(*rid).unwrap().unwrap();
            assert_eq!(got, format!("row-{i:04}").into_bytes());
        }
    }

    #[test]
    fn discard_rolls_back_uncommitted_pages() {
        let dir = TempDir::new();
        let path = dir.file("t.heap");
        let mut heap = HeapFile::open(&path).unwrap();
        heap.insert(b"committed").unwrap().unwrap();
        heap.flush().unwrap();
        let rid = heap.insert(b"pending").unwrap().unwrap();
        assert_eq!(heap.record(rid).unwrap().unwrap(), b"pending");
        heap.discard_changes();
        assert_eq!(heap.record(rid).unwrap(), None);
        drop(heap);

        let mut heap = HeapFile::open(&path).unwrap();
        assert!(heap
            .for_each_live(|_, _, r| {
                assert_eq!(r, b"committed");
                Ok(())
            })
            .is_ok());
    }

    #[test]
    fn compaction_drops_tombstones() {
        let dir = TempDir::new();
        let path = dir.file("t.heap");
        let mut heap = HeapFile::open(&path).unwrap();
        let mut rids = Vec::new();
        for i in 0..500u32 {
            let payload = format!("row-{i:04}");
            rids.push(heap.insert(payload.as_bytes()).unwrap().unwrap());
        }
        for rid in &rids[..400] {
            assert!(heap.tombstone(*rid).unwrap());
        }
        heap.compact(|_, _, _| Ok(true)).unwrap();
        let mut live = 0u32;
        heap.for_each_live(|_, _, _| {
            live += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(live, 100);
    }
}
