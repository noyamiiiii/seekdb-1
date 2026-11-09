//! The storage engine: catalog, heap files, primary-key index, WAL and the
//! transaction boundary the executor talks to.
//!
//! A transaction buffers its page changes in memory. `commit` writes them to the
//! WAL and `fsync`s once — that single sync is what makes the commit durable —
//! then writes the pages back to their heap files without syncing them. The log
//! is truncated by a checkpoint (`checkpoint`), which syncs every heap file
//! first; until then recovery replays the committed page images, so a crash
//! either loses an uncommitted transaction or rolls a committed one forward.
//! `rollback` drops the buffered pages and undoes the in-memory index changes.

pub mod catalog;
pub mod heap;
pub mod page;
pub mod record;
pub mod wal;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ast::{CreateTable, Value};
use crate::error::{DbError, DbResult, ErrorCode};
use crate::schema::TableSchema;

use catalog::Catalog;
use heap::{HeapFile, Rid};
use wal::Wal;

/// Converts an I/O failure into the protocol's generic error.
fn io_err(context: &str, err: io::Error) -> DbError {
    DbError::new(ErrorCode::Unknown, format!("{context}: {err}"))
}

/// The log is truncated once it grows past this size: every buffered page is
/// written back and fsynced first, so replay stays short and memory bounded.
const CHECKPOINT_BYTES: u64 = 4 * 1024 * 1024;

/// fsyncs a directory so that file creations and removals survive a power cut.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

pub struct Table {
    pub schema: TableSchema,
    pub heap: HeapFile,
    /// Primary key -> record id of the current version of that row.
    pub index: HashMap<i64, Rid>,
    pub rows: u64,
}

impl Table {
    fn open(dir: &Path, schema: TableSchema) -> DbResult<Self> {
        let heap = HeapFile::open(dir.join(format!("{}.heap", schema.name)))
            .map_err(|e| io_err("cannot open heap file", e))?;
        let table = Self {
            schema,
            heap,
            index: HashMap::new(),
            rows: 0,
        };
        Ok(table)
    }

    /// Rebuilds the primary-key index from the heap. This runs once at open,
    /// which is why every record repeats its primary key in front.
    fn rebuild_index(&mut self) -> DbResult<()> {
        self.index.clear();
        let index = &mut self.index;
        self.heap.for_each_live(|page, slot, bytes| {
            if let Some(pk) = record::prefix_pk(bytes) { index.insert(pk, Rid { page, slot }); }
            Ok(())
        }).map_err(|e| io_err("cannot scan heap for index rebuild", e))?;
        self.rows = self.index.len() as u64;
        Ok(())
    }

    fn insert_row(&mut self, values: &[Value]) -> DbResult<(Rid, i64)> {
        let pk = self.schema.primary_key_value(values);
        if self.index.contains_key(&pk) {
            return Err(DbError::new(
                ErrorCode::DuplicateEntry,
                format!("duplicate primary key {pk} in table '{}'", self.schema.name),
            ));
        }
        let bytes = record::encode_row(&self.schema, values)?;
        let rid = self
            .heap
            .insert(&bytes)
            .map_err(|e| io_err("cannot write a row", e))?
            .ok_or_else(|| {
                DbError::new(
                    ErrorCode::OutOfRange,
                    format!(
                        "row is too large for a page in table '{}'",
                        self.schema.name
                    ),
                )
            })?;
        self.index.insert(pk, rid);
        self.rows += 1;
        Ok((rid, pk))
    }

    fn delete_row(&mut self, pk: i64) -> DbResult<Option<Rid>> {
        let Some(rid) = self.index.get(&pk).copied() else {
            return Ok(None);
        };
        self.heap
            .tombstone(rid)
            .map_err(|e| io_err("cannot delete a row", e))?;
        self.index.remove(&pk);
        self.rows = self.rows.saturating_sub(1);
        Ok(Some(rid))
    }

    fn read_row(&mut self, rid: Rid) -> DbResult<Option<Vec<Value>>> {
        let Some(bytes) = self
            .heap
            .record(rid)
            .map_err(|e| io_err("cannot read a row", e))?
        else {
            return Ok(None);
        };
        Ok(Some(record::decode_row(&self.schema, &bytes)?))
    }
}

/// One statement's or one `BEGIN` block's changes, with what is needed to undo
/// them in memory.
struct Txn {
    id: u64,
    auto: bool,
    undo: Vec<Undo>,
}

enum Undo {
    Insert { table: String, rid: Rid, pk: i64 },
    Delete { table: String, rid: Rid, pk: i64 },
}

pub struct Storage {
    dir: PathBuf,
    catalog: Catalog,
    tables: BTreeMap<String, Table>,
    wal: Wal,
    next_txn: u64,
    txn: Option<Txn>,
    catalog_dirty: bool,
}

impl Storage {
    /// Opens (or creates) a database directory and recovers from the WAL.
    pub fn open(dir: impl AsRef<Path>) -> DbResult<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| io_err("cannot create the data directory", e))?;

        let catalog = Catalog::load(&dir.join("catalog.bin"))
            .map_err(|e| io_err("cannot load the catalog", e))?;
        let mut tables = BTreeMap::new();
        for schema in catalog.tables.values() {
            tables.insert(schema.name.clone(), Table::open(&dir, schema.clone())?);
        }

        // Apply committed images before inspecting possibly torn heap pages.
        wal::replay(&dir.join("wal.log"), |name, id, image| {
            if let Some(table) = tables.get_mut(name) { table.heap.apply_recovered(id, image)?; }
            Ok(())
        }).map_err(|e| io_err("cannot replay the log", e))?;
        for table in tables.values_mut() {
            table.heap.sync().map_err(|e| io_err("cannot sync recovered pages", e))?;
        }

        let mut wal =
            Wal::open(dir.join("wal.log")).map_err(|e| io_err("cannot open the log", e))?;
        wal.reset().map_err(|e| io_err("cannot reset the log", e))?;

        let mut storage = Self {
            dir,
            catalog,
            tables,
            wal,
            next_txn: 0,
            txn: None,
            catalog_dirty: false,
        };
        for table in storage.tables.values_mut() {
            table.rebuild_index()?;
        }
        Ok(storage)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // ----- transactions -------------------------------------------------

    pub fn in_explicit_txn(&self) -> bool {
        matches!(&self.txn, Some(txn) if !txn.auto)
    }

    pub fn begin(&mut self) -> DbResult<()> {
        if self.txn.is_some() {
            return Err(DbError::new(
                ErrorCode::NestedTransaction,
                "a transaction is already open",
            ));
        }
        self.next_txn += 1;
        self.txn = Some(Txn {
            id: self.next_txn,
            auto: false,
            undo: Vec::new(),
        });
        Ok(())
    }

    /// Starts the implicit transaction of one autocommit statement.
    pub fn begin_statement(&mut self) -> DbResult<()> {
        if self.in_explicit_txn() {
            return Ok(());
        }
        if self.txn.is_some() {
            // A previous statement left an implicit transaction behind.
            self.rollback()?;
        }
        self.next_txn += 1;
        self.txn = Some(Txn {
            id: self.next_txn,
            auto: true,
            undo: Vec::new(),
        });
        Ok(())
    }

    pub fn commit(&mut self) -> DbResult<()> {
        let Some(txn) = self.txn.take() else {
            return Err(DbError::new(
                ErrorCode::NoActiveTransaction,
                "no transaction is open",
            ));
        };
        if let Err(err) = self.make_durable(txn.id) {
            // The commands are still in memory; dropping them keeps memory and
            // the files in agreement, since nothing was made durable.
            self.discard_buffers();
            return Err(err);
        }
        Ok(())
    }

    pub fn rollback(&mut self) -> DbResult<()> {
        let Some(txn) = self.txn.take() else {
            return Err(DbError::new(
                ErrorCode::NoActiveTransaction,
                "no transaction is open",
            ));
        };
        self.undo(txn);
        Ok(())
    }

    /// Drops the changes of the open transaction without reporting an error;
    /// used when a statement fails under autocommit.
    pub fn rollback_quietly(&mut self) {
        if let Some(txn) = self.txn.take() {
            self.undo(txn);
        }
    }

    pub fn statement_mark(&self) -> usize {
        self.txn.as_ref().map_or(0, |txn| txn.undo.len())
    }

    pub fn rollback_statement(&mut self, mark: usize) {
        let entries = self.txn.as_mut().map(|txn| txn.undo.split_off(mark)).unwrap_or_default();
        self.undo_entries(entries);
    }

    fn undo_entries(&mut self, entries: Vec<Undo>) {
        for entry in entries.into_iter().rev() {
            match entry {
                Undo::Insert { table, rid, pk } => {
                    if let Some(t) = self.tables.get_mut(&table) {
                        let _ = t.heap.tombstone(rid);
                        t.index.remove(&pk);
                        t.rows = t.rows.saturating_sub(1);
                    }
                }
                Undo::Delete { table, rid, pk } => {
                    if let Some(t) = self.tables.get_mut(&table) {
                        let _ = t.heap.revive(rid);
                        t.index.insert(pk, rid);
                        t.rows += 1;
                    }
                }
            }
        }
    }

    fn undo(&mut self, txn: Txn) {
        self.undo_entries(txn.undo);
        self.discard_buffers();
    }

    fn discard_buffers(&mut self) {
        for table in self.tables.values_mut() {
            table.heap.discard_changes();
        }
    }

    fn make_durable(&mut self, txn_id: u64) -> DbResult<()> {
        let dirty = self.tables.values().any(|t| t.heap.has_dirty());
        if dirty {
            {
                let Storage { tables, wal, .. } = self;
                wal.begin(txn_id).map_err(|e| io_err("cannot begin log transaction", e))?;
                for (name, table) in tables.iter_mut() {
                    table.heap.for_each_dirty(|id, image| wal.page(txn_id, name, id, image))
                        .map_err(|e| io_err("cannot write log pages", e))?;
                }
                wal.finish_commit(txn_id).map_err(|e| io_err("cannot commit the log", e))?;
            }
            // The log now survives a crash on its own, so the heap pages can be
            // written back without waiting for their own fsync. They are only
            // made durable by the next checkpoint, before the log is truncated.
            for table in self.tables.values_mut() {
                table
                    .heap
                    .write_dirty()
                    .map_err(|e| io_err("cannot write heap pages", e))?;
            }
            let log_bytes = self
                .wal
                .bytes()
                .map_err(|e| io_err("cannot size the log", e))?;
            if log_bytes >= CHECKPOINT_BYTES {
                self.checkpoint()?;
            }
        }
        if std::mem::take(&mut self.catalog_dirty) {
            self.catalog
                .save(&self.dir.join("catalog.bin"))
                .map_err(|e| io_err("cannot save the catalog", e))?;
        }
        Ok(())
    }

    /// Writes every buffered page back to its heap file and truncates the log.
    ///
    /// Called once the log grows past [`CHECKPOINT_BYTES`], and when the session
    /// shuts down cleanly, so that a restart does not have to replay a long
    /// log.
    pub fn checkpoint(&mut self) -> DbResult<()> {
        for table in self.tables.values_mut() {
            if table.heap.has_dirty() {
                table
                    .heap
                    .flush()
                    .map_err(|e| io_err("cannot flush heap pages", e))?;
            } else {
                table
                    .heap
                    .sync()
                    .map_err(|e| io_err("cannot flush heap pages", e))?;
            }
        }
        self.wal
            .reset()
            .map_err(|e| io_err("cannot reset the log", e))?;
        Ok(())
    }

    // ----- catalog ------------------------------------------------------

    pub fn table(&mut self, name: &str) -> DbResult<&mut Table> {
        let key = name.to_ascii_lowercase();
        self.tables.get_mut(&key).ok_or_else(|| {
            DbError::new(
                ErrorCode::NoSuchTable,
                format!("table '{name}' does not exist"),
            )
        })
    }

    pub fn schema(&self, name: &str) -> DbResult<&TableSchema> {
        self.tables
            .get(&name.to_ascii_lowercase())
            .map(|t| &t.schema)
            .ok_or_else(|| {
                DbError::new(
                    ErrorCode::NoSuchTable,
                    format!("table '{name}' does not exist"),
                )
            })
    }

    pub fn table_names(&self) -> impl Iterator<Item = &str> {
        self.tables.keys().map(|s| s.as_str())
    }

    pub fn row_count(&self, name: &str) -> DbResult<u64> {
        self.tables
            .get(&name.to_ascii_lowercase())
            .map(|t| t.rows)
            .ok_or_else(|| {
                DbError::new(
                    ErrorCode::NoSuchTable,
                    format!("table '{name}' does not exist"),
                )
            })
    }

    /// DDL is applied immediately: the catalog is durable once this returns.
    fn finish_ddl(&mut self) -> DbResult<()> {
        self.catalog_dirty = true;
        self.make_durable(0)?;
        sync_dir(&self.dir).map_err(|e| io_err("cannot sync the data directory", e))
    }

    pub fn create_table(&mut self, create: &CreateTable) -> DbResult<()> {
        if self.in_explicit_txn() {
            return Err(DbError::new(
                ErrorCode::NotSupported,
                "DDL inside a transaction is not supported",
            ));
        }
        let key = create.name.to_ascii_lowercase();
        if self.catalog.tables.contains_key(&key) {
            return Err(DbError::new(
                ErrorCode::TableExists,
                format!("table '{}' already exists", create.name),
            ));
        }
        let mut schema = TableSchema::from_create(create)?;
        schema.name = key.clone();
        // A previous DROP may have died after catalog replacement but before
        // unlinking its heap. An absent catalog name always means a new heap.
        self.remove_index_metadata(&key)?;
        let heap_path = self.dir.join(format!("{}.heap", key));
        if heap_path.exists() { fs::remove_file(&heap_path).map_err(|e| io_err("cannot remove orphan heap", e))?; }
        let table = Table::open(&self.dir, schema.clone())?;
        self.tables.insert(key.clone(), table);
        self.catalog.tables.insert(key, schema);
        self.finish_ddl()
    }

    pub fn declare_vector_index(&mut self, table: &str, name: &str, column: &str) -> DbResult<()> {
        use std::io::Write;
        let path = self.dir.join(format!("{}.{}.vidx",table.to_ascii_lowercase(),name.to_ascii_lowercase()));
        if path.exists() { return Err(DbError::new(ErrorCode::DuplicateEntry,"vector index already exists")); }
        let tmp = path.with_extension("vidx.tmp");
        let mut out = fs::File::create(&tmp).map_err(|e| io_err("cannot create index metadata",e))?;
        writeln!(out,"MVFLAT01\n{}\n{}",table.to_ascii_lowercase(),column.to_ascii_lowercase()).map_err(|e| io_err("cannot write index metadata",e))?;
        out.sync_all().map_err(|e| io_err("cannot sync index metadata",e))?;
        fs::rename(tmp,path).map_err(|e| io_err("cannot publish index metadata",e))?;
        sync_dir(&self.dir).map_err(|e| io_err("cannot sync index directory",e))
    }

    pub fn drop_vector_index(&mut self, table: &str, name: &str) -> DbResult<()> {
        self.schema(table)?;
        let path = self.dir.join(format!("{}.{}.vidx",table.to_ascii_lowercase(),name.to_ascii_lowercase()));
        fs::remove_file(path).map_err(|e| io_err("cannot drop vector index",e))?;
        sync_dir(&self.dir).map_err(|e| io_err("cannot sync index directory",e))
    }

    fn remove_index_metadata(&self, table: &str) -> DbResult<()> {
        for item in fs::read_dir(&self.dir).map_err(|e| io_err("cannot list index metadata",e))? {
            let path = item.map_err(|e| io_err("cannot read index metadata",e))?.path();
            let name = path.file_name().and_then(|s|s.to_str()).unwrap_or_default();
            if name.starts_with(&format!("{}.",table.to_ascii_lowercase())) && name.ends_with(".vidx") {
                fs::remove_file(path).map_err(|e| io_err("cannot remove stale index metadata",e))?;
            }
        }
        Ok(())
    }

    pub fn drop_table(&mut self, name: &str) -> DbResult<()> {
        if self.in_explicit_txn() {
            return Err(DbError::new(
                ErrorCode::NotSupported,
                "DDL inside a transaction is not supported",
            ));
        }
        let key = name.to_ascii_lowercase();
        self.checkpoint()?;
        let Some(table) = self.tables.remove(&key) else {
            return Err(DbError::new(
                ErrorCode::NoSuchTable,
                format!("table '{name}' does not exist"),
            ));
        };
        let heap_path = table.heap.path().to_path_buf();
        drop(table);
        self.catalog.tables.remove(&key);
        self.catalog_dirty = true;
        self.make_durable(0)?;
        let _ = fs::remove_file(heap_path);
        self.remove_index_metadata(&key)?;
        sync_dir(&self.dir).map_err(|e| io_err("cannot sync the data directory", e))?;
        Ok(())
    }

    /// Rewrites one table's heap without tombstoned records and rebuilds its
    /// primary-key index. Called by maintenance, never inside a transaction.
    pub fn compact(&mut self, table: &str) -> DbResult<()> {
        if self.txn.is_some() {
            return Err(DbError::new(
                ErrorCode::NotSupported,
                "cannot compact a table inside a transaction",
            ));
        }
        // The rewrite renumbers pages, so no log record may survive it.
        self.checkpoint()?;
        let key = table.to_ascii_lowercase();
        let Some(t) = self.tables.get_mut(&key) else {
            return Err(DbError::new(
                ErrorCode::NoSuchTable,
                format!("table '{table}' does not exist"),
            ));
        };
        t.heap
            .compact(|_, _, _| Ok(true))
            .map_err(|e| io_err("cannot compact the heap file", e))?;
        t.rebuild_index()
    }

    // ----- row operations ----------------------------------------------

    /// Inserts validated rows; returns the affected row count.
    pub fn insert(&mut self, table: &str, rows: &[Vec<Value>]) -> DbResult<u64> {
        let key = table.to_ascii_lowercase();
        let mut inserted = 0u64;
        for values in rows {
            let (rid, pk) = {
                let t = self.table(&key)?;
                let values = t.schema.coerce_row(values)?;
                t.insert_row(&values)?
            };
            if let Some(txn) = &mut self.txn {
                txn.undo.push(Undo::Insert {
                    table: key.clone(),
                    rid,
                    pk,
                });
            }
            inserted += 1;
        }
        Ok(inserted)
    }

    pub fn delete(&mut self, table: &str, pk: i64) -> DbResult<bool> {
        let key = table.to_ascii_lowercase();
        let rid = {
            let t = self.table(&key)?;
            t.delete_row(pk)?
        };
        if let Some(rid) = rid {
            if let Some(txn) = &mut self.txn {
                txn.undo.push(Undo::Delete {
                    table: key,
                    rid,
                    pk,
                });
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Replaces the row with `pk`; the primary key itself cannot change.
    pub fn update(&mut self, table: &str, pk: i64, values: &[Value]) -> DbResult<bool> {
        let key = table.to_ascii_lowercase();
        let (old_pk, new_values) = {
            let t = self.table(&key)?;
            let values = t.schema.coerce_row(values)?;
            let new_pk = t.schema.primary_key_value(&values);
            (new_pk, values)
        };
        if old_pk != pk {
            return Err(DbError::new(
                ErrorCode::NotSupported,
                "updating the primary key is not supported",
            ));
        }
        let old_rid = {
            let t = self.table(&key)?;
            t.index.get(&pk).copied()
        };
        let Some(_old_rid) = old_rid else {
            return Ok(false);
        };
        self.delete(&key, pk)?;
        let (new_rid, _) = self.table(&key)?.insert_row(&new_values)?;
        if let Some(txn) = &mut self.txn {
            txn.undo.push(Undo::Insert {
                table: key,
                rid: new_rid,
                pk,
            });
        }
        Ok(true)
    }

    pub fn get(&mut self, table: &str, pk: i64) -> DbResult<Option<Vec<Value>>> {
        let key = table.to_ascii_lowercase();
        let rid = {
            let t = self.table(&key)?;
            t.index.get(&pk).copied()
        };
        let Some(rid) = rid else {
            return Ok(None);
        };
        let t = self.table(&key)?;
        t.read_row(rid)
    }

    /// Streams every live row to `visit`; returning `false` stops the scan.
    /// Pages are read one at a time, so the table size does not bound memory.
    pub fn for_each_row<F>(&mut self, table: &str, mut visit: F) -> DbResult<()>
    where
        F: FnMut(&[Value]) -> DbResult<bool>,
    {
        let key = table.to_ascii_lowercase();
        let t = self.table(&key)?;
        let schema = t.schema.clone();
        let mut failure: Option<DbError> = None;
        let mut stop = false;
        t.heap
            .for_each_live(|_, _, bytes| {
                if stop || failure.is_some() {
                    return Ok(());
                }
                match record::decode_row(&schema, bytes) {
                    Ok(row) => match visit(&row) {
                        Ok(true) => {}
                        Ok(false) => stop = true,
                        Err(err) => failure = Some(err),
                    },
                    Err(err) => failure = Some(err),
                }
                Ok(())
            })
            .map_err(|e| io_err("cannot scan the table", e))?;
        match failure {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Streams every live row as encoded bytes; returning `false` stops the
    /// scan. The distance scan uses this to avoid building a `Vec<Value>` per
    /// row when only a couple of columns are needed.
    pub fn for_each_raw<F>(&mut self, table: &str, mut visit: F) -> DbResult<()>
    where
        F: FnMut(&[u8]) -> DbResult<bool>,
    {
        let key = table.to_ascii_lowercase();
        let t = self.table(&key)?;
        let mut failure: Option<DbError> = None;
        let mut stop = false;
        t.heap
            .for_each_live(|_, _, bytes| {
                if stop || failure.is_some() {
                    return Ok(());
                }
                match visit(bytes) {
                    Ok(true) => {}
                    Ok(false) => stop = true,
                    Err(err) => failure = Some(err),
                }
                Ok(())
            })
            .map_err(|e| io_err("cannot scan the table", e))?;
        match failure {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// Returns the encoded bytes of one column of a stored row, if the row is well
/// formed. Cheap for the first columns, linear in the column count overall.
pub fn column_bytes<'a>(
    schema: &TableSchema,
    bytes: &'a [u8],
    index: usize,
) -> Option<&'a [u8]> {
    let mut pos = 8usize;
    for (position, def) in schema.columns.iter().enumerate() {
        let len = match def.ty {
            crate::ast::ColumnType::BigInt => 8,
            crate::ast::ColumnType::Varchar(_) => {
                let raw = bytes.get(pos..pos + 4)?;
                let len = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
                pos += 4;
                if position == index {
                    return bytes.get(pos..pos + len);
                }
                pos += len;
                continue;
            }
            crate::ast::ColumnType::Vector(dim) => dim * 4,
        };
        if position == index {
            return bytes.get(pos..pos + len);
        }
        pos += len;
    }
    None
}
