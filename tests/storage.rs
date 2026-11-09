//! Storage-engine tests: persistence across reopen, transaction atomicity,
//! rollback, and crash recovery through the write-ahead log.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use minivectordb::ast::Value;
use minivectordb::error::ErrorCode;
use minivectordb::parser::parse;
use minivectordb::storage::heap::HeapFile;
use minivectordb::storage::record::encode_row;
use minivectordb::storage::wal::Wal;
use minivectordb::storage::Storage;

static NEXT: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mvdb-storage-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn create_docs(storage: &mut Storage) {
    let minivectordb::ast::Statement::CreateTable(create) =
        parse("CREATE TABLE docs (id BIGINT PRIMARY KEY, title VARCHAR(16), emb VECTOR(3))")
            .unwrap()
    else {
        panic!("not a create table")
    };
    storage.create_table(&create).unwrap();
}

fn row(pk: i64, title: &str) -> Vec<Value> {
    vec![
        Value::Int(pk),
        Value::Text(title.to_string()),
        Value::Vector(vec![pk as f32, 0.0, 1.0]),
    ]
}

fn all_ids(storage: &mut Storage, table: &str) -> Vec<i64> {
    let mut ids = Vec::new();
    storage
        .for_each_row(table, |row| {
            if let Value::Int(pk) = row[0] {
                ids.push(pk);
            }
            Ok(true)
        })
        .unwrap();
    ids
}

#[test]
fn rows_survive_reopen() {
    let dir = TempDir::new();
    {
        let mut storage = Storage::open(&dir.0).unwrap();
        create_docs(&mut storage);
        storage.begin_statement().unwrap();
        storage
            .insert("docs", &[row(1, "one"), row(2, "two"), row(3, "three")])
            .unwrap();
        storage.commit().unwrap();
    }

    let mut storage = Storage::open(&dir.0).unwrap();
    assert_eq!(storage.row_count("docs").unwrap(), 3);
    assert_eq!(
        storage.get("docs", 2).unwrap().unwrap(),
        row(2, "two"),
        "values are stored exactly"
    );
    assert_eq!(all_ids(&mut storage, "docs"), vec![1, 2, 3]);
}

#[test]
fn duplicate_primary_key_is_rejected() {
    let dir = TempDir::new();
    let mut storage = Storage::open(&dir.0).unwrap();
    create_docs(&mut storage);
    storage.begin_statement().unwrap();
    storage.insert("docs", &[row(1, "one")]).unwrap();
    storage.commit().unwrap();

    storage.begin_statement().unwrap();
    let err = storage.insert("docs", &[row(1, "again")]).unwrap_err();
    assert_eq!(err.code, ErrorCode::DuplicateEntry);
    storage.rollback_quietly();
    assert_eq!(storage.row_count("docs").unwrap(), 1);
}

#[test]
fn failed_statement_leaves_no_partial_rows() {
    let dir = TempDir::new();
    let mut storage = Storage::open(&dir.0).unwrap();
    create_docs(&mut storage);
    storage.begin_statement().unwrap();
    storage.insert("docs", &[row(10, "ten")]).unwrap();
    storage.commit().unwrap();

    // The second row of the batch collides with the first: nothing is kept.
    storage.begin_statement().unwrap();
    assert!(storage
        .insert("docs", &[row(11, "eleven"), row(10, "dup")])
        .is_err());
    storage.rollback_quietly();
    assert_eq!(storage.row_count("docs").unwrap(), 1);
    assert!(storage.get("docs", 11).unwrap().is_none());
}

#[test]
fn explicit_rollback_discards_inserts_and_deletes() {
    let dir = TempDir::new();
    let mut storage = Storage::open(&dir.0).unwrap();
    create_docs(&mut storage);
    storage.begin_statement().unwrap();
    storage
        .insert("docs", &[row(1, "one"), row(2, "two")])
        .unwrap();
    storage.commit().unwrap();

    storage.begin().unwrap();
    storage.insert("docs", &[row(3, "three")]).unwrap();
    assert!(storage.delete("docs", 1).unwrap());
    assert_eq!(storage.row_count("docs").unwrap(), 2);
    storage.rollback().unwrap();

    assert_eq!(storage.row_count("docs").unwrap(), 2);
    assert_eq!(storage.get("docs", 1).unwrap().unwrap(), row(1, "one"));
    assert!(storage.get("docs", 3).unwrap().is_none());
    drop(storage);

    // The rolled-back transaction must not reappear after a restart.
    let mut storage = Storage::open(&dir.0).unwrap();
    assert_eq!(all_ids(&mut storage, "docs"), vec![1, 2]);
}

#[test]
fn update_replaces_values_and_survives_rollback() {
    let dir = TempDir::new();
    let mut storage = Storage::open(&dir.0).unwrap();
    create_docs(&mut storage);
    storage.begin_statement().unwrap();
    storage.insert("docs", &[row(7, "seven")]).unwrap();
    storage.commit().unwrap();

    storage.begin().unwrap();
    assert!(storage.update("docs", 7, &row(7, "SEVEN")).unwrap());
    assert_eq!(
        storage.get("docs", 7).unwrap().unwrap()[1],
        Value::Text("SEVEN".into())
    );
    storage.rollback().unwrap();
    assert_eq!(storage.get("docs", 7).unwrap().unwrap(), row(7, "seven"));

    storage.begin_statement().unwrap();
    storage.update("docs", 7, &row(7, "SEVEN")).unwrap();
    storage.commit().unwrap();
    drop(storage);

    let mut storage = Storage::open(&dir.0).unwrap();
    assert_eq!(
        storage.get("docs", 7).unwrap().unwrap()[1],
        Value::Text("SEVEN".into())
    );
    assert_eq!(storage.row_count("docs").unwrap(), 1);
}

#[test]
fn committed_wal_image_is_rolled_forward_on_open() {
    let dir = TempDir::new();
    let schema;
    {
        let mut storage = Storage::open(&dir.0).unwrap();
        create_docs(&mut storage);
        storage.begin_statement().unwrap();
        storage.insert("docs", &[row(1, "one")]).unwrap();
        storage.commit().unwrap();
        schema = storage.schema("docs").unwrap().clone();
    }

    // Stage a second row in the heap *without* flushing it, and record the page
    // image in the log as if the process died right after the commit mark.
    let heap_path = dir.0.join("docs.heap");
    let mut heap = HeapFile::open(&heap_path).unwrap();
    let encoded = encode_row(&schema, &row(2, "two")).unwrap();
    heap.insert(&encoded).unwrap().unwrap();
    let images: Vec<(u32, Vec<u8>)> = heap
        .dirty_pages()
        .map(|(id, image)| (id, image.to_vec()))
        .collect();
    let mut wal = Wal::open(dir.0.join("wal.log")).unwrap();
    wal.append_commit(
        42,
        images
            .iter()
            .map(|(id, image)| ("docs", *id, image.as_slice())),
    )
    .unwrap();
    drop(wal);
    drop(heap); // the images never reached the heap file

    let mut storage = Storage::open(&dir.0).unwrap();
    assert_eq!(storage.row_count("docs").unwrap(), 2);
    assert_eq!(storage.get("docs", 2).unwrap().unwrap(), row(2, "two"));
    drop(storage);

    // Recovery is idempotent and leaves an empty log behind.
    let mut storage = Storage::open(&dir.0).unwrap();
    assert_eq!(storage.row_count("docs").unwrap(), 2);
    assert_eq!(all_ids(&mut storage, "docs"), vec![1, 2]);
}

#[test]
fn a_corrupt_committed_page_image_fails_to_open() {
    let dir = TempDir::new();
    {
        let mut storage = Storage::open(&dir.0).unwrap();
        create_docs(&mut storage);
        storage.begin_statement().unwrap();
        storage.insert("docs", &[row(1, "one")]).unwrap();
        storage.commit().unwrap();
    }

    let mut wal = Wal::open(dir.0.join("wal.log")).unwrap();
    // All-0xFF bytes are not a valid page header.
    let garbage = vec![0xFFu8; minivectordb::storage::page::PAGE_SIZE];
    wal.append_commit(7, vec![("docs", 0u32, garbage.as_slice())])
        .unwrap();
    drop(wal);

    assert!(Storage::open(&dir.0).is_err());
}

#[test]
fn compaction_rewrites_the_heap_and_rebuilds_the_index() {
    const ROWS: i64 = 4000;
    let dir = TempDir::new();
    {
        let mut storage = Storage::open(&dir.0).unwrap();
        create_docs(&mut storage);
        // Enough rows to span several 64 KiB pages, otherwise the file cannot
        // physically shrink and the assertion below would be meaningless.
        let rows: Vec<Vec<Value>> = (1..=ROWS).map(|pk| row(pk, "x")).collect();
        storage.begin_statement().unwrap();
        storage.insert("docs", &rows).unwrap();
        storage.commit().unwrap();

        let before = std::fs::metadata(dir.0.join("docs.heap")).unwrap().len();

        // Delete every even row, then compact: the file must shrink and the
        // surviving rows must still be addressable by primary key.
        storage.begin_statement().unwrap();
        for pk in (2..=ROWS).step_by(2) {
            assert!(storage.delete("docs", pk).unwrap());
        }
        storage.commit().unwrap();
        storage.compact("docs").unwrap();
        let after = std::fs::metadata(dir.0.join("docs.heap")).unwrap().len();
        assert!(after < before, "compaction must shrink the heap");

        assert_eq!(storage.row_count("docs").unwrap(), ROWS as u64 / 2);
        assert_eq!(storage.get("docs", 1).unwrap().unwrap(), row(1, "x"));
        assert_eq!(storage.get("docs", 2).unwrap(), None);
        assert_eq!(all_ids(&mut storage, "docs").len(), ROWS as usize / 2);
    }

    // The compacted file reopens as a normal heap.
    let mut storage = Storage::open(&dir.0).unwrap();
    assert_eq!(storage.row_count("docs").unwrap(), ROWS as u64 / 2);
    assert_eq!(all_ids(&mut storage, "docs").len(), ROWS as usize / 2);
}

#[test]
fn schema_is_persisted_in_the_catalog() {
    let dir = TempDir::new();
    {
        let mut storage = Storage::open(&dir.0).unwrap();
        create_docs(&mut storage);
    }

    let storage = Storage::open(&dir.0).unwrap();
    let schema = storage.schema("docs").unwrap();
    assert_eq!(schema.name, "docs");
    assert_eq!(schema.columns.len(), 3);
    assert_eq!(schema.primary_key, 0);
    assert_eq!(schema.vector_column, Some(2));
    assert_eq!(schema.column_index("TITLE"), Some(1));
}
