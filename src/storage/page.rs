//! Slotted heap pages.
//!
//! A page is a fixed-size buffer (8 KiB). The front holds a slot table
//! (`offset`, `length`); record payloads grow downwards from the end. A slot
//! with the high length bit set is a tombstone: the payload bytes stay until the page is
//! compacted, which keeps record ids stable across deletes.
//!
//! Layout of the buffer:
//!
//! ```text
//! [0..2]  slot count
//! [2..4]  start of the free gap
//! [4..6]  end of the free gap
//! [6..7]  flags (overflow head/continuation and tombstone)
//! [7]     reserved
//! [8..]   slot table: (offset u32, length u32, tombstone flag in bit 31)
//! ```

/// Pages are kept small on purpose: a benchmark batch of 100 rows writes about
/// 55 KiB, and every page a batch touches is logged in full. At 64 KiB that is
/// a 64 KiB log record (plus the same bytes again on the heap); at 8 KiB the
/// same batch logs about 56 KiB and the tail page that is rewritten by the next
/// batch is eight times cheaper.
pub const PAGE_SIZE: usize = 8 * 1024;
/// Largest payload a page can hold, leaving room for one slot.
pub const MAX_RECORD_SIZE: usize = PAGE_SIZE - HEADER - SLOT_SIZE;

const HEADER: usize = 8;
const SLOT_SIZE: usize = 8;
/// Bit 31 of the length marks a tombstone slot.
const TOMBSTONE: u32 = 0x8000_0000;

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    buf: Box<[u8]>,
}

impl Default for Page {
    fn default() -> Self {
        Self::new()
    }
}

impl Page {
    pub fn new() -> Self {
        let mut buf = vec![0u8; PAGE_SIZE].into_boxed_slice();
        buf[2..4].copy_from_slice(&(HEADER as u16).to_le_bytes());
        buf[4..6].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
        Self { buf }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let view = PageView::new(bytes)?;
        // A freshly allocated file region reads as zeros: treat it as empty
        // rather than as a corrupt page.
        if view.slot_count() == 0 && view.free_start() == 0 {
            return Some(Self::new());
        }
        Some(Self { buf: bytes.to_vec().into_boxed_slice() })
    }

    pub fn overflow(payload: &[u8], next: Option<u32>, head: bool) -> Self {
        let mut page = Self::new();
        page.buf[0..2].copy_from_slice(&1u16.to_le_bytes());
        page.buf[6] = if head { 0x81 } else { 0x82 };
        page.buf[8..12].copy_from_slice(&next.unwrap_or(u32::MAX).to_le_bytes());
        page.buf[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        page.buf[16..16+payload.len()].copy_from_slice(payload);
        page
    }
    pub fn is_overflow(&self) -> bool { self.buf[6] & 0x80 != 0 }
    pub fn is_overflow_head(&self) -> bool { self.buf[6] & 0x83 == 0x81 }
    pub fn overflow_next(&self) -> Option<u32> {
        let id = u32::from_le_bytes(self.buf[8..12].try_into().unwrap());
        (id != u32::MAX).then_some(id)
    }
    pub fn overflow_payload(&self) -> &[u8] {
        let n = u32::from_le_bytes(self.buf[12..16].try_into().unwrap()) as usize;
        &self.buf[16..16+n]
    }
    pub fn is_overflow_live(&self) -> bool { self.is_overflow_head() && self.buf[6] & 0x40 == 0 }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn slot_count(&self) -> u16 {
        u16::from_le_bytes([self.buf[0], self.buf[1]])
    }

    fn free_start(&self) -> u16 {
        u16::from_le_bytes([self.buf[2], self.buf[3]])
    }

    fn free_end(&self) -> usize {
        let end = u16::from_le_bytes([self.buf[4], self.buf[5]]);
        if end == 0 { PAGE_SIZE } else { end as usize }
    }

    fn set_free(&mut self, start: u16, end: usize) {
        self.buf[2..4].copy_from_slice(&start.to_le_bytes());
        self.buf[4..6].copy_from_slice(&(end as u16).to_le_bytes());
    }

    fn slot_at(&self, slot: u16) -> (u32, u32) {
        let base = HEADER + slot as usize * SLOT_SIZE;
        let off = u32::from_le_bytes(self.buf[base..base + 4].try_into().unwrap());
        let raw = u32::from_le_bytes(self.buf[base + 4..base + 8].try_into().unwrap());
        (off, raw)
    }

    fn set_slot(&mut self, slot: u16, off: u32, raw_len: u32) {
        let base = HEADER + slot as usize * SLOT_SIZE;
        self.buf[base..base + 4].copy_from_slice(&off.to_le_bytes());
        self.buf[base + 4..base + 8].copy_from_slice(&raw_len.to_le_bytes());
    }

    /// Free bytes available for one more record of `len` (slot included).
    pub fn can_fit(&self, len: usize) -> bool {
        !self.is_overflow() && len <= MAX_RECORD_SIZE && self.free_bytes() >= len + SLOT_SIZE
    }

    fn free_bytes(&self) -> usize {
        self.free_end() as usize - self.free_start() as usize
    }

    /// Bytes occupied by tombstoned payloads.
    pub fn garbage(&self) -> usize {
        (0..self.slot_count())
            .filter_map(|s| {
                let (_, raw) = self.slot_at(s);
                (raw & TOMBSTONE != 0).then_some((raw & !TOMBSTONE) as usize)
            })
            .sum()
    }

    /// Appends `payload` and returns its slot id, compacting the page if the
    /// tail of the page is exhausted but garbage would make room.
    pub fn insert(&mut self, payload: &[u8]) -> Option<u16> {
        // Never compact or reuse tombstones here: live RIDs and transaction
        // undo records must retain their original slots and payloads.
        if !self.can_fit(payload.len()) { return None; }
        let slot = self.slot_count();
        let off = self.free_end() - payload.len();
        self.buf[off as usize..off as usize + payload.len()].copy_from_slice(payload);
        self.set_slot(slot, off as u32, payload.len() as u32);
        self.buf[0..2].copy_from_slice(&(slot + 1).to_le_bytes());
        self.set_free(self.free_start() + SLOT_SIZE as u16, off);
        Some(slot)
    }

    /// Payload of a live record; `None` for tombstones and invalid slots.
    pub fn record(&self, slot: u16) -> Option<&[u8]> {
        if self.is_overflow() { return (slot == 0 && self.is_overflow_live()).then(|| self.overflow_payload()); }
        if slot >= self.slot_count() {
            return None;
        }
        let (off, raw) = self.slot_at(slot);
        if raw & TOMBSTONE != 0 {
            return None;
        }
        let len = raw as usize;
        Some(&self.buf[off as usize..off as usize + len])
    }

    pub fn tombstone(&mut self, slot: u16) -> bool {
        if self.is_overflow() {
            if slot != 0 || !self.is_overflow_live() { return false; }
            self.buf[6] |= 0x40; return true;
        }
        if slot >= self.slot_count() {
            return false;
        }
        let (off, raw) = self.slot_at(slot);
        if raw & TOMBSTONE != 0 {
            return false;
        }
        self.set_slot(slot, off, raw | TOMBSTONE);
        true
    }

    /// Revives a tombstoned record, used when a transaction rolls back.
    pub fn revive(&mut self, slot: u16) -> bool {
        if self.is_overflow() {
            if slot != 0 || !self.is_overflow_head() || self.buf[6] & 0x40 == 0 { return false; }
            self.buf[6] &= !0x40; return true;
        }
        if slot >= self.slot_count() {
            return false;
        }
        let (off, raw) = self.slot_at(slot);
        if raw & TOMBSTONE == 0 {
            return false;
        }
        self.set_slot(slot, off, raw & !TOMBSTONE);
        true
    }

    /// Rewrites the page with only live records, in slot order.
    pub fn compact(&mut self) {
        let live: Vec<Vec<u8>> = (0..self.slot_count())
            .filter_map(|s| self.record(s).map(|r| r.to_vec()))
            .collect();
        *self = Self::new();
        for record in live {
            // Compaction cannot fail: the page only shrinks.
            self.insert(&record);
        }
    }

    /// Iterates live `(slot, payload)` pairs in slot order.
    pub fn live_records(&self) -> impl Iterator<Item = (u16, &[u8])> + '_ {
        let view = PageView::new(&self.buf).expect("page buffers are validated on construction");
        view.live_records()
    }
}

/// A borrowed page image, validated without copying it.
///
/// Scans read a whole heap file through one reusable buffer, so they check each
/// page in place instead of allocating an owned [`Page`] per 8 KiB image.
#[derive(Clone, Copy)]
pub struct PageView<'a> {
    bytes: &'a [u8],
}

impl<'a> PageView<'a> {
    /// Validates a page image; `None` when the bytes cannot be a page.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() != PAGE_SIZE {
            return None;
        }
        let view = Self { bytes };
        // A freshly allocated file region reads as zeros and stands for an
        // empty page rather than a corrupt one.
        if view.slot_count() == 0 && view.free_start() == 0 {
            return Some(view);
        }
        if view.is_overflow() {
            let n = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
            return (n <= PAGE_SIZE - 16).then_some(view);
        }
        let count = view.slot_count() as usize;
        let free_start = view.free_start() as usize;
        let free_end = view.free_end();
        if count > (PAGE_SIZE - HEADER) / SLOT_SIZE
            || free_start < HEADER
            || free_end > PAGE_SIZE
            || free_start > free_end
            || free_start != HEADER + count * SLOT_SIZE
        {
            return None;
        }
        Some(view)
    }

    pub fn is_overflow(&self) -> bool {
        self.bytes[6] & 0x80 != 0
    }

    pub fn is_overflow_live(&self) -> bool {
        self.bytes[6] & 0x83 == 0x81 && self.bytes[6] & 0x40 == 0
    }

    fn slot_count(&self) -> u16 {
        u16::from_le_bytes([self.bytes[0], self.bytes[1]])
    }

    fn free_start(&self) -> u16 {
        u16::from_le_bytes([self.bytes[2], self.bytes[3]])
    }

    fn free_end(&self) -> usize {
        let end = u16::from_le_bytes([self.bytes[4], self.bytes[5]]);
        if end == 0 {
            PAGE_SIZE
        } else {
            end as usize
        }
    }

    /// Payload of a live slot; `None` for tombstones, overflow images and slots
    /// that point outside the page.
    pub fn record(&self, slot: u16) -> Option<&'a [u8]> {
        if self.is_overflow() || slot >= self.slot_count() {
            return None;
        }
        let base = HEADER + slot as usize * SLOT_SIZE;
        let off = u32::from_le_bytes(self.bytes[base..base + 4].try_into().ok()?) as usize;
        let raw = u32::from_le_bytes(self.bytes[base + 4..base + 8].try_into().ok()?);
        if raw & TOMBSTONE != 0 {
            return None;
        }
        self.bytes.get(off..off.checked_add(raw as usize)?)
    }

    /// Iterates live `(slot, payload)` pairs in slot order.
    pub fn live_records(self) -> impl Iterator<Item = (u16, &'a [u8])> {
        (0..self.slot_count()).filter_map(move |slot| self.record(slot).map(|r| (slot, r)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_read() {
        let mut page = Page::new();
        let a = page.insert(b"alpha").unwrap();
        let b = page.insert(b"beta").unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(page.record(a), Some(&b"alpha"[..]));
        assert_eq!(page.record(b), Some(&b"beta"[..]));
    }

    #[test]
    fn tombstone_hides_record_but_keeps_slot() {
        let mut page = Page::new();
        let a = page.insert(b"alpha").unwrap();
        assert!(page.tombstone(a));
        assert_eq!(page.record(a), None);
        assert_eq!(page.slot_count(), 1);
        assert_eq!(page.garbage(), 5);
        assert!(!page.tombstone(a), "double tombstone is a no-op");
    }

    #[test]
    fn roundtrips_through_bytes() {
        let mut page = Page::new();
        page.insert(b"alpha").unwrap();
        page.insert(b"beta").unwrap();
        page.tombstone(0);
        let restored = Page::from_bytes(page.as_bytes()).unwrap();
        assert_eq!(restored.live_records().count(), 1);
        assert_eq!(restored.record(1), Some(&b"beta"[..]));
    }

    #[test]
    fn borrowed_view_sees_the_same_live_records() {
        let mut page = Page::new();
        page.insert(b"alpha").unwrap();
        page.insert(b"beta").unwrap();
        page.tombstone(0);
        let view = PageView::new(page.as_bytes()).unwrap();
        let owned: Vec<(u16, Vec<u8>)> =
            page.live_records().map(|(slot, r)| (slot, r.to_vec())).collect();
        let borrowed: Vec<(u16, Vec<u8>)> =
            view.live_records().map(|(slot, r)| (slot, r.to_vec())).collect();
        assert_eq!(owned, borrowed);
        assert_eq!(owned, vec![(1, b"beta".to_vec())]);
    }

    #[test]
    fn borrowed_view_rejects_bad_images() {
        assert!(PageView::new(&[0u8; PAGE_SIZE - 1]).is_none());
        let mut bytes = [0u8; PAGE_SIZE];
        bytes[0..2].copy_from_slice(&1u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&999u16.to_le_bytes());
        assert!(PageView::new(&bytes).is_none(), "slot table must abut the header");
    }

    #[test]
    fn zeroed_region_reads_as_empty_page() {
        let page = Page::from_bytes(&vec![0u8; PAGE_SIZE]).unwrap();
        assert_eq!(page.slot_count(), 0);
        assert_eq!(page.live_records().count(), 0);
    }

    #[test]
    fn compaction_reclaims_room_and_preserves_records() {
        let mut page = Page::new();
        let mut slots = Vec::new();
        // Fill most of the page with records, then delete every other one.
        let payload = vec![7u8; PAGE_SIZE / 4];
        while let Some(slot) = page.insert(&payload) {
            slots.push(slot);
        }
        assert!(slots.len() >= 3, "page held {} records", slots.len());
        for slot in slots.iter().step_by(2) {
            page.tombstone(*slot);
        }
        let before_garbage = page.garbage();
        assert!(before_garbage > 0);
        // Force compaction through the free-space path.
        page.compact();
        assert_eq!(page.garbage(), 0);
        let live: Vec<u16> = page.live_records().map(|(s, _)| s).collect();
        assert_eq!(live.len(), slots.len() / 2);
        assert!(page.can_fit(PAGE_SIZE / 4));
    }

    #[test]
    fn oversized_record_is_rejected() {
        let mut page = Page::new();
        assert!(page.insert(&vec![0u8; MAX_RECORD_SIZE + 1]).is_none());
        assert!(page.insert(&vec![0u8; MAX_RECORD_SIZE]).is_some());
    }
}
