//! The write-ahead log.
//!
//! Commit protocol: append `Begin`, one image per modified page, `Commit`,
//! `fsync`; only then write the pages to their heap files, `fsync` those, and
//! truncate the log. A crash can therefore leave either (a) a log without a
//! commit record — nothing happened, or (b) a commit record — replaying the
//! images brings the heaps back to the committed state. Heap files never
//! contain a transaction that the log would not roll forward.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::page::PAGE_SIZE;

const MAGIC: &[u8; 8] = b"MVWAL01\n";
const KIND_BEGIN: u8 = 1;
const KIND_PAGE: u8 = 2;
const KIND_COMMIT: u8 = 3;
const MAX_PENDING: usize = 256 * 1024;

pub struct Wal {
    path: PathBuf,
    file: File,
    /// Records of the transaction being committed. The buffer is bounded so a
    /// transaction larger than memory streams instead of growing without limit.
    pending: Vec<u8>,
    /// File offset where the current uncommitted transaction starts.
    pending_start: Option<u64>,
}

impl Wal {
    /// Opens the log, creating it with the magic header when needed.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let len = file.metadata()?.len();
        if len < MAGIC.len() as u64 {
            file.set_len(0)?;
            file.write_all(MAGIC)?;
            file.sync_all()?;
        } else {
            let mut header = [0u8; 8];
            file.seek(SeekFrom::Start(0))?;
            file.read_exact(&mut header)?;
            if &header != MAGIC {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a MiniVectorDB log", path.display()),
                ));
            }
        }
        file.seek(SeekFrom::End(0))?;
        Ok(Self { path, file, pending: Vec::new(), pending_start: None })
    }

    fn encode_record(&mut self, kind: u8, payload: &[u8]) {
        let mut header = [0u8; 5];
        header[0] = kind;
        header[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        self.pending.extend_from_slice(&header);
        self.pending.extend_from_slice(payload);
    }

    pub(crate) fn begin(&mut self, txn: u64) -> io::Result<()> {
        // A begin discards a previous transaction that was never committed.
        self.discard_pending();
        self.encode_record(KIND_BEGIN, &txn.to_le_bytes());
        Ok(())
    }

    pub(crate) fn page(&mut self, txn: u64, table: &str, page_id: u32, image: &[u8]) -> io::Result<()> {
        let name = table.as_bytes();
        let start = self.pending.len();
        self.pending.reserve(14 + name.len() + image.len());
        self.pending.resize(start + 5, 0);
        self.pending.extend_from_slice(&txn.to_le_bytes());
        self.pending.extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.pending.extend_from_slice(name);
        self.pending.extend_from_slice(&page_id.to_le_bytes());
        self.pending.extend_from_slice(image);
        let len = (self.pending.len() - start - 5) as u32;
        self.pending[start] = KIND_PAGE;
        self.pending[start + 1..start + 5].copy_from_slice(&len.to_le_bytes());
        if self.pending.len() >= MAX_PENDING {
            self.write_pending()?;
        }
        Ok(())
    }

    fn commit(&mut self, txn: u64) -> io::Result<()> {
        self.encode_record(KIND_COMMIT, &txn.to_le_bytes());
        Ok(())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        // Appended bytes and the size needed to read them are covered by
        // fdatasync; the directory entry was made durable when the log was
        // created.
        let result = self.write_pending().and_then(|_| self.file.sync_data());
        if result.is_err() {
            self.discard_pending();
        }
        self.pending_start = None;
        result
    }

    fn write_pending(&mut self) -> io::Result<()> {
        if self.pending_start.is_none() {
            self.pending_start = Some(self.file.seek(SeekFrom::End(0))?);
        }
        self.file.write_all(&self.pending)?;
        self.pending.clear();
        Ok(())
    }

    fn discard_pending(&mut self) {
        if let Some(start) = self.pending_start.take() {
            let _ = self.file.set_len(start);
            let _ = self.file.seek(SeekFrom::End(0));
        }
        self.pending.clear();
    }

    /// Durably records a transaction's page images followed by its commit mark.
    pub fn append_commit<'a, I>(&mut self, txn: u64, pages: I) -> io::Result<()>
    where
        I: IntoIterator<Item = (&'a str, u32, &'a [u8])>,
    {
        self.begin(txn)?;
        for (table, page_id, image) in pages {
            debug_assert_eq!(image.len(), PAGE_SIZE);
            self.page(txn, table, page_id, image)?;
        }
        self.commit(txn)?;
        self.flush_pending()
    }

    pub(crate) fn finish_commit(&mut self, txn: u64) -> io::Result<()> {
        self.commit(txn)?;
        self.flush_pending()
    }

    /// Empties the log; called once the heaps are durable.
    pub fn reset(&mut self) -> io::Result<()> {
        self.discard_pending();
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(MAGIC)?;
        self.file.sync_all()?;
        self.file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    /// Size of the log in bytes; used to decide when to checkpoint.
    pub fn bytes(&mut self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Every committed page image in the log, in log order. Several transactions
/// may be present: the log is only truncated by a checkpoint, so pages of
/// earlier commits may already sit (possibly torn) in the heap files and must
/// be rolled forward too. A log without a commit record replays as empty.
pub fn recover(path: &Path) -> io::Result<Vec<(String, u32, Vec<u8>)>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.len() < MAGIC.len() {
        return Ok(Vec::new());
    }
    let mut pos = MAGIC.len();
    let mut applied: Vec<(String, u32, Vec<u8>)> = Vec::new();
    let mut pending: Vec<(String, u32, Vec<u8>)> = Vec::new();
    while pos + 5 <= bytes.len() {
        let kind = bytes[pos];
        let len = u32::from_le_bytes(bytes[pos + 1..pos + 5].try_into().unwrap()) as usize;
        let start = pos + 5;
        let end = start + len;
        if end > bytes.len() {
            break; // torn tail of a record; treat as end of log
        }
        let payload = &bytes[start..end];
        match kind {
            KIND_BEGIN => pending.clear(),
            KIND_PAGE => {
                if payload.len() < 14 {
                    break;
                }
                let name_len = u16::from_le_bytes(payload[8..10].try_into().unwrap()) as usize;
                let Some(name_end) = 10usize.checked_add(name_len) else {
                    break;
                };
                let Some(table) = payload.get(10..name_end) else {
                    break;
                };
                let Ok(table) = std::str::from_utf8(table) else {
                    break;
                };
                let Some(page_bytes) = payload.get(name_end..name_end + 4) else {
                    break;
                };
                let page_id = u32::from_le_bytes(page_bytes.try_into().unwrap());
                let image = payload.get(name_end + 4..).unwrap_or_default();
                if image.len() != PAGE_SIZE {
                    break;
                }
                pending.push((table.to_string(), page_id, image.to_vec()));
            }
            KIND_COMMIT => applied.append(&mut pending),
            _ => break,
        }
        pos = end;
    }
    Ok(applied)
}

/// Stream committed page images; retain offsets, never whole transactions.
/// Replaying twice is safe because the images replace fixed heap pages.
pub fn replay<F>(path: &Path, mut apply: F) -> io::Result<()>
where F: FnMut(&str, u32, &[u8]) -> io::Result<()> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    if len < MAGIC.len() as u64 { return Ok(()); }
    let mut magic = [0; 8]; file.read_exact(&mut magic)?;
    if &magic != MAGIC { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid WAL header")); }
    let mut transaction = None;
    let mut pending: Vec<(String, u32, u64)> = Vec::new();
    loop {
        let mut header = [0; 5];
        if let Err(e) = file.read_exact(&mut header) {
            if e.kind() == io::ErrorKind::UnexpectedEof { break; }
            return Err(e);
        }
        let n = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
        if n > PAGE_SIZE + 65550 || file.stream_position()? + n as u64 > len { break; }
        let mut payload = vec![0; n]; file.read_exact(&mut payload)?;
        if n < 8 { break; }
        let txn = u64::from_le_bytes(payload[..8].try_into().unwrap());
        match header[0] {
            KIND_BEGIN if n == 8 => { pending.clear(); transaction = Some(txn); }
            KIND_PAGE if transaction == Some(txn) && n >= 14 => {
                let name_len = u16::from_le_bytes(payload[8..10].try_into().unwrap()) as usize;
                let end = 10 + name_len;
                if end + 4 + PAGE_SIZE != n { break; }
                let name = std::str::from_utf8(&payload[10..end]).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid WAL table"))?;
                let id = u32::from_le_bytes(payload[end..end+4].try_into().unwrap());
                pending.push((name.to_owned(), id, file.stream_position()? - PAGE_SIZE as u64));
            }
            KIND_COMMIT if transaction == Some(txn) && n == 8 => {
                let resume = file.stream_position()?;
                let mut image = vec![0; PAGE_SIZE];
                for (name, id, offset) in pending.drain(..) {
                    file.seek(SeekFrom::Start(offset))?;file.read_exact(&mut image)?;
                    apply(&name, id, &image)?;
                }
                file.seek(SeekFrom::Start(resume))?;
                transaction = None;
            }
            _ => break,
        }
    }
    Ok(())
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
            let path = std::env::temp_dir().join(format!("mvdb-wal-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn committed_images_are_replayed() {
        let dir = TempDir::new();
        let path = dir.0.join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        let image_a = vec![1u8; PAGE_SIZE];
        let image_b = vec![2u8; PAGE_SIZE];
        wal.append_commit(
            7,
            vec![
                ("docs", 0u32, image_a.as_slice()),
                ("docs", 1u32, image_b.as_slice()),
            ],
        )
        .unwrap();
        drop(wal);

        let images = recover(&path).unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].0, "docs");
        assert_eq!(images[0].1, 0);
        assert_eq!(images[0].2[0], 1);
        assert_eq!(images[1].1, 1);
    }

    #[test]
    fn uncommitted_transaction_replays_as_empty() {
        let dir = TempDir::new();
        let path = dir.0.join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        wal.begin(9).unwrap();
        wal.page(9, "docs", 3, &vec![5u8; PAGE_SIZE]).unwrap();
        drop(wal);
        assert!(recover(&path).unwrap().is_empty());
    }

    #[test]
    fn torn_tail_is_ignored() {
        let dir = TempDir::new();
        let path = dir.0.join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        let image = vec![3u8; PAGE_SIZE];
        wal.append_commit(1, vec![("docs", 0u32, image.as_slice())])
            .unwrap();
        drop(wal);
        // Simulate a crash in the middle of the next record.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&[KIND_PAGE, 0xff, 0xff]).unwrap();
        drop(file);
        assert_eq!(recover(&path).unwrap().len(), 1);
    }

    #[test]
    fn reset_truncates_to_header() {
        let dir = TempDir::new();
        let path = dir.0.join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        wal.append_commit(1, vec![("docs", 0u32, vec![0u8; PAGE_SIZE].as_slice())])
            .unwrap();
        wal.reset().unwrap();
        assert!(recover(&path).unwrap().is_empty());
    }

    #[test]
    fn large_commit_streams_with_a_bounded_buffer() {
        let dir = TempDir::new();
        let path = dir.0.join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        let image = vec![7u8; PAGE_SIZE];
        wal.begin(11).unwrap();
        for id in 0..100u32 {
            wal.page(11, "docs", id, &image).unwrap();
            assert!(wal.pending.len() < MAX_PENDING);
        }
        wal.finish_commit(11).unwrap();
        assert!(wal.pending.is_empty());
        let images = recover(&path).unwrap();
        assert_eq!(images.len(), 100);
        assert_eq!(images[99].1, 99);
    }
}
