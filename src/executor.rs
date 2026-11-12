//! Statement execution: DDL/DML against [`Storage`], plus the expression
//! evaluator used by `WHERE` and `ORDER BY`.
//!
//! A [`Session`] owns one storage instance (one `/data` directory) and turns a
//! parsed [`Statement`] into a [`Response`]. Transaction policy lives here, not
//! in storage: every statement outside an explicit transaction runs in its own
//! implicit transaction that commits on success and rolls back on failure, so a
//! failing statement never leaves partial rows behind.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::Path;

use crate::ast::{
    CmpOp, ColumnType, CreateVectorIndex, Delete, Expr, Insert, Select, SelectItem, Statement,
    Update, Value,
};
use crate::distance;
use crate::error::{DbError, DbResult, ErrorCode};
use crate::response::{Cell, Response, ResultSet};
use crate::schema::TableSchema;
use crate::storage::Storage;

/// One connected client: parsed statements in, rendered responses out.
const SCAN_CACHE_BUDGET: usize = 8 * 1024 * 1024;
const FILTER_CACHE_BUDGET: usize = 1024 * 1024;
/// Packed prefilter bytes. The row cache is already capped at
/// `SCAN_CACHE_BUDGET`, so a 128-dimension table of 10,000 rows only needs
/// 1.25 MiB here; the cap keeps the bound explicit for wide schemas.
const VECTOR_CACHE_BUDGET: usize = 4 * 1024 * 1024;
/// Bucket width of the packed-score histogram. 128 dimensions of 7-bit codes
/// cap the integer score at `128 * 127 * 127`, so 4096 buckets of 512 cover it.
const QUANT_BUCKET_SHIFT: u32 = 9;
const QUANT_BUCKETS: usize = 4096;

#[derive(Clone, Copy)]
struct CachedRow {
    start: u32,
    end: u32,
    vector_start: u32,
    pk: i64,
}

struct CachedFilter {
    filter: RawFilter,
    indices: Vec<usize>,
    bytes: usize,
}

struct RawScanCache {
    table: String,
    bytes: Option<Vec<u8>>,
    ranges: Vec<CachedRow>,
    /// Row-major 7-bit vectors and their affine quantization.
    quantized: Vec<u8>,
    quant_dim: usize,
    quant_min: f32,
    quant_step: f32,
    query_quantized: Vec<u8>,
    candidate_flags: Vec<u8>,
    /// Integer packed scores and their bucket histogram for one query.
    quant_scores: Vec<u32>,
    quant_histogram: Vec<u32>,
    filters: Vec<CachedFilter>,
    filter_bytes: usize,
    /// Reused `f32` prefilter scores for a large cached table.
    distances: Vec<f32>,
    /// Scratch copy used to find the prefilter cutoff without disturbing the
    /// row-order mapping in `distances`.
    distance_scratch: Vec<f32>,
}

pub struct Session {
    storage: Storage,
    // Bounded exact scan index, rebuilt after every visibility change.
    // Larger tables always fall back to streaming heap reads.
    scan_cache: Option<RawScanCache>,
}

impl Session {
    /// Opens (or creates) the database in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> DbResult<Self> {
        Ok(Self {
            storage: Storage::open(dir)?,
            scan_cache: None,
        })
    }

    pub fn storage(&mut self) -> &mut Storage {
        self.scan_cache = None;
        &mut self.storage
    }

    /// Flushes buffered pages and truncates the log; called on a clean shutdown
    /// so that the next start does not replay it.
    pub fn shutdown(&mut self) -> DbResult<()> {
        self.storage.rollback_quietly();
        self.storage.checkpoint()
    }

    /// Rendering-free entry point used by the runner and by the tests.
    pub fn execute(&mut self, sql: &str) -> Response {
        match crate::parser::parse(sql) {
            Ok(stmt) => self.run(&stmt),
            Err(err) => Response::Err(err),
        }
    }

    pub fn run(&mut self, stmt: &Statement) -> Response {
        if !matches!(stmt, Statement::Select(_) | Statement::SelectConst { .. } | Statement::ShowTables | Statement::ShowColumns { .. }) {
            self.scan_cache = None;
        }
        match self.dispatch(stmt) {
            Ok(response) => response,
            Err(err) => Response::Err(err),
        }
    }

    fn dispatch(&mut self, stmt: &Statement) -> DbResult<Response> {
        match stmt {
            Statement::Begin => {
                self.storage.begin()?;
                Ok(Response::ok(0))
            }
            Statement::Commit => {
                self.storage.commit()?;
                Ok(Response::ok(0))
            }
            Statement::Rollback => {
                self.storage.rollback()?;
                Ok(Response::ok(0))
            }
            // Accept only the documented mode and encoding values.
            Statement::Set(setting) => {
                if setting.name.eq_ignore_ascii_case("vector_mode") {
                    if !setting.value.eq_ignore_ascii_case("exact") && !setting.value.eq_ignore_ascii_case("ann") {
                        return Err(DbError::new(ErrorCode::WrongValue,"vector_mode must be exact or ann"));
                    }
                    // Both modes use the exact flat index; ANN permits this
                    // stricter result and always meets its recall threshold.
                    Ok(Response::ok(0))
                } else if setting.name.eq_ignore_ascii_case("names") && setting.value.eq_ignore_ascii_case("utf8mb4") {
                    Ok(Response::ok(0))
                } else { Err(DbError::unsupported("this session variable")) }
            },
            Statement::CreateTable(create) => {
                self.implicit_commit()?;
                self.storage.create_table(create)?;
                Ok(Response::ok(0))
            }
            Statement::CreateVectorIndex(index) => {
                self.implicit_commit()?;
                self.create_vector_index(index)?;
                Ok(Response::ok(0))
            }
            Statement::DropTable(drop) => {
                self.implicit_commit()?;
                match self.storage.drop_table(&drop.table) {
                    Ok(()) => Ok(Response::ok(0)),
                    Err(err) if drop.if_exists && matches!(err.code, ErrorCode::NoSuchTable) => {
                        Ok(Response::ok(0))
                    }
                    Err(err) => Err(err),
                }
            }
            Statement::Truncate(table) => {
                self.implicit_commit()?;
                self.truncate(table)?;
                Ok(Response::ok(0))
            }
            Statement::ShowTables => Ok(Response::Rows(self.show_tables())),
            Statement::ShowColumns { table } => Ok(Response::Rows(self.show_columns(table)?)),
            // Index definitions are persistent; exact results come from rows.
            Statement::DropIndex { table, name } => {
                self.storage.drop_vector_index(table,name)?;
                Ok(Response::ok(0))
            }
            Statement::Noop => Err(DbError::unsupported("database namespaces")),
            Statement::SelectConst { items } => Ok(Response::Rows(select_const(items))),
            Statement::Insert(_)
            | Statement::Select(_)
            | Statement::Update(_)
            | Statement::Delete(_) => self.run_dml(stmt),
        }
    }

    /// MySQL commits the open transaction before a DDL statement runs.
    fn implicit_commit(&mut self) -> DbResult<()> {
        if self.storage.in_explicit_txn() {
            self.storage.commit()?;
        }
        Ok(())
    }

    /// Runs a DML statement in its own implicit transaction unless an explicit
    /// transaction is open.
    fn run_dml(&mut self, stmt: &Statement) -> DbResult<Response> {
        if self.storage.in_explicit_txn() {
            let mark = self.storage.statement_mark();
            let result = self.execute_dml(stmt);
            if result.is_err() { self.storage.rollback_statement(mark); }
            return result;
        }
        self.storage.begin_statement()?;
        match self.execute_dml(stmt) {
            Ok(response) => {
                self.storage.commit()?;
                Ok(response)
            }
            Err(err) => {
                self.storage.rollback_quietly();
                Err(err)
            }
        }
    }

    fn execute_dml(&mut self, stmt: &Statement) -> DbResult<Response> {
        match stmt {
            Statement::Insert(insert) => Ok(Response::ok(self.insert(insert)?)),
            Statement::Update(update) => Ok(Response::ok(self.update(update)?)),
            Statement::Delete(delete) => Ok(Response::ok(self.delete(delete)?)),
            Statement::Select(select) => Ok(Response::Rows(self.select(select)?)),
            other => Err(DbError::new(
                ErrorCode::NotSupported,
                format!("{other:?} is not a DML statement"),
            )),
        }
    }

    fn create_vector_index(&mut self, index: &CreateVectorIndex) -> DbResult<()> {
        let schema = self.storage.schema(&index.table)?;
        let (_, def) = schema.column(&index.column)?;
        if !matches!(def.ty, ColumnType::Vector(_)) {
            return Err(DbError::new(
                ErrorCode::WrongValue,
                format!("column '{}' is not a VECTOR column", index.column),
            ));
        }
        // A bounded exact flat index never weakens the recall contract. Larger
        // tables use its streaming backing store rather than growing memory.
        self.prepare_scan_cache(&index.table)?;
        self.storage.declare_vector_index(&index.table, &index.name, &index.column)

    }

    // ----- DML ----------------------------------------------------------

    fn insert(&mut self, insert: &Insert) -> DbResult<u64> {
        // Natural column order can go directly through storage validation;
        // avoid cloning quoted vectors before parsing them into float32.
        if insert.columns.is_none() { return self.storage.insert(&insert.table,&insert.rows); }
        let schema = self.storage.schema(&insert.table)?.clone();
        let order = column_order(&schema, insert.columns.as_deref())?;

        let mut rows = Vec::with_capacity(insert.rows.len());
        for values in &insert.rows {
            if values.len() != order.len() {
                return Err(DbError::new(
                    ErrorCode::WrongValueCount,
                    format!(
                        "column count doesn't match value count: {} columns, {} values",
                        order.len(),
                        values.len()
                    ),
                ));
            }
            let mut row = vec![Value::Int(0); schema.columns.len()];
            for (value, &column) in values.iter().zip(&order) {
                row[column] = value.clone();
            }
            rows.push(row);
        }
        self.storage.insert(&insert.table, &rows)
    }

    fn update(&mut self, update: &Update) -> DbResult<u64> {
        let schema = self.storage.schema(&update.table)?.clone();
        if let Some(filter) = &update.filter { validate_expr(&schema, filter)?; }
        for (name, _) in &update.assignments {
            if schema.column(name)?.0 == schema.primary_key {
                return Err(DbError::new(ErrorCode::NotSupported, "updating the primary key is not supported"));
            }
        }
        let assigned: Vec<(usize, Value)> = update
            .assignments
            .iter()
            .map(|(name, value)| {
                let (column, _) = schema.column(name)?;
                Ok((column, schema.coerce(column, value)?))
            })
            .collect::<DbResult<Vec<_>>>()?;

        let pks = self.matching_pks(&update.table, update.filter.as_ref())?;
        let mut affected = 0u64;
        for pk in pks {
            let Some(mut row) = self.storage.get(&update.table, pk)? else {
                continue;
            };
            for (column, value) in &assigned {
                row[*column] = value.clone();
            }
            if self.storage.update(&update.table, pk, &row)? {
                affected += 1;
            }
        }
        Ok(affected)
    }

    fn delete(&mut self, delete: &Delete) -> DbResult<u64> {
        if let Some(filter) = &delete.filter { validate_expr(self.storage.schema(&delete.table)?, filter)?; }
        let pks = self.matching_pks(&delete.table, delete.filter.as_ref())?;
        let mut affected = 0u64;
        for pk in pks {
            if self.storage.delete(&delete.table, pk)? {
                affected += 1;
            }
        }
        Ok(affected)
    }

    /// Removes every row, atomically. MySQL's `TRUNCATE` commits implicitly,
    /// so the caller has already closed the explicit transaction.
    fn truncate(&mut self, table: &str) -> DbResult<()> {
        let schema = self.storage.schema(table)?.clone();
        let pks = {
            let mut pks = Vec::new();
            let mut failure = None;
            self.storage.for_each_row(table, |row| {
                match row[schema.primary_key] {
                    Value::Int(pk) => pks.push(pk),
                    _ => {
                        failure = Some(DbError::new(
                            ErrorCode::WrongValue,
                            "primary key is not an integer",
                        ));
                        return Ok(false);
                    }
                }
                Ok(true)
            })?;
            match failure {
                Some(err) => return Err(err),
                None => pks,
            }
        };
        self.storage.begin_statement()?;
        for pk in pks {
            if let Err(err) = self.storage.delete(table, pk) {
                self.storage.rollback_quietly();
                return Err(err);
            }
        }
        self.storage.commit()
    }

    fn show_tables(&mut self) -> ResultSet {
        let mut names: Vec<String> = self.storage.table_names().map(str::to_string).collect();
        names.sort();
        ResultSet {
            columns: vec!["Tables_in_test".to_string()],
            rows: names
                .into_iter()
                .map(|name| vec![Cell::Text(name)])
                .collect(),
        }
    }

    fn show_columns(&mut self, table: &str) -> DbResult<ResultSet> {
        let schema = self.storage.schema(table)?.clone();
        let rows = schema
            .columns
            .iter()
            .enumerate()
            .map(|(index, def)| {
                let ty = match def.ty {
                    ColumnType::BigInt => "bigint".to_string(),
                    ColumnType::Varchar(n) => format!("varchar({n})"),
                    ColumnType::Vector(n) => format!("vector({n})"),
                };
                let key = if index == schema.primary_key {
                    "PRI"
                } else {
                    ""
                };
                vec![
                    Cell::Text(def.name.clone()),
                    Cell::Text(ty),
                    Cell::Text("NO".to_string()),
                    Cell::Text(key.to_string()),
                    Cell::Text("NULL".to_string()),
                    Cell::Text(String::new()),
                ]
            })
            .collect();
        Ok(ResultSet {
            columns: ["Field", "Type", "Null", "Key", "Default", "Extra"]
                .iter()
                .map(|name| name.to_string())
                .collect(),
            rows,
        })
    }

    /// Primary keys of every row that satisfies `filter`.
    ///
    /// The scan streams row by row and only collects keys, so `UPDATE` and
    /// `DELETE` never hold the table in memory.
    fn matching_pks(&mut self, table: &str, filter: Option<&Expr>) -> DbResult<Vec<i64>> {
        let schema = self.storage.schema(table)?.clone();
        let mut pks = Vec::new();
        let mut failure = None;
        self.storage.for_each_row(table, |row| {
            if let Some(filter) = filter {
                match filter_matches(filter, row, &schema) {
                    Ok(true) => {}
                    Ok(false) => return Ok(true),
                    Err(err) => {
                        failure = Some(err);
                        return Ok(false);
                    }
                }
            }
            match row[schema.primary_key] {
                Value::Int(pk) => pks.push(pk),
                _ => {
                    failure = Some(DbError::new(
                        ErrorCode::WrongValue,
                        "primary key is not an integer",
                    ));
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        match failure {
            Some(err) => Err(err),
            None => Ok(pks),
        }
    }

    // ----- SELECT -------------------------------------------------------

    fn select(&mut self, select: &Select) -> DbResult<ResultSet> {
        let schema = self.storage.schema(&select.table)?.clone();
        if let Some(filter) = &select.filter { validate_expr(&schema, filter)?; }
        let outs = plan_outputs(&schema, &select.items)?;
        let columns: Vec<String> = outs.iter().map(|o| o.name.clone()).collect();
        let limit = select.limit.map(|n| n as usize);

        // `ORDER BY` is resolved once: an unknown column or a dimension
        // mismatch must fail even when the table is empty.
        let order = select
            .order_by
            .as_ref()
            .map(|order_by| {
                Ok::<_, DbError>((
                    resolve_order(&schema, &outs, &order_by.expr)?,
                    order_by.ascending,
                ))
            })
            .transpose()?;

        let mut rows: Vec<Vec<Cell>> = Vec::new();
        // Fast path for the shape the benchmark uses: a distance ordering with a
        // limit and a filter that only compares plain columns. It scans encoded
        // bytes, so no row is decoded unless it ends up in the result.
        let distance_order = order.as_ref().and_then(|(key, ascending)| match key {
            OrderKey::Distance { column, query } => Some((*column, query.clone(), *ascending)),
            OrderKey::Output(index) => match &outs[*index].kind {
                OutKind::Distance { column, query } => Some((*column, query.clone(), *ascending)),
                OutKind::Column(_) => None,
            },
            OrderKey::Column(_) => None,
        });
        if let (Some(keep), Some((column, query, ascending))) = (limit, distance_order) {
            if keep > 0 {
                if let Some(filter) = RawFilter::build(&schema, select.filter.as_ref())? {
                    return self.select_by_distance(
                        &select.table,
                        &schema,
                        &outs,
                        columns,
                        &filter,
                        column,
                        &query,
                        ascending,
                        keep,
                    );
                }
            }
        }
        match order {
            None => {
                let mut failure = None;
                self.storage.for_each_row(&select.table, |row| {
                    if failure.is_some() || limit.is_some_and(|n| rows.len() >= n) {
                        return Ok(false);
                    }
                    if let Some(filter) = &select.filter {
                        match filter_matches(filter, row, &schema) {
                            Ok(true) => {}
                            Ok(false) => return Ok(true),
                            Err(err) => {
                                failure = Some(err);
                                return Ok(false);
                            }
                        }
                    }
                    match project(&outs, row) {
                        Ok(cells) => rows.push(cells),
                        Err(err) => {
                            failure = Some(err);
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })?;
                if let Some(err) = failure {
                    return Err(err);
                }
            }
            Some((key, ascending)) => {
                // A bounded collector: with `LIMIT k` only the k best rows are
                // kept, so memory stays O(k) no matter how large the table is.
                let mut top = TopK::new(limit, ascending);
                let mut failure = None;
                if limit != Some(0) {
                    self.storage.for_each_row(&select.table, |row| {
                        if failure.is_some() {
                            return Ok(false);
                        }
                        if let Some(filter) = &select.filter {
                            match filter_matches(filter, row, &schema) {
                                Ok(true) => {}
                                Ok(false) => return Ok(true),
                                Err(err) => {
                                    failure = Some(err);
                                    return Ok(false);
                                }
                            }
                        }
                        let value = match eval_order(&key, &outs, row) {
                            Ok(value) => value,
                            Err(err) => {
                                failure = Some(err);
                                return Ok(false);
                            }
                        };
                        let pk = match row[schema.primary_key] {
                            Value::Int(pk) => pk,
                            _ => {
                                failure = Some(DbError::new(
                                    ErrorCode::WrongValue,
                                    "primary key is not an integer",
                                ));
                                return Ok(false);
                            }
                        };
                        match top.offer(value, pk, || project(&outs, row)) {
                            Ok(()) => {}
                            Err(err) => {
                                failure = Some(err);
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    })?;
                }
                if let Some(err) = failure {
                    return Err(err);
                }
                rows = top.into_rows();
            }
        }

        Ok(ResultSet { columns, rows })
    }

    fn prepare_scan_cache(&mut self, table: &str) -> DbResult<()> {
        let key = table.to_ascii_lowercase();
        if self.scan_cache.as_ref().is_some_and(|cache| cache.table == key) { return Ok(()); }
        let schema = self.storage.schema(table)?.clone();
        let count = self.storage.table(table)?.rows as usize;
        let metadata = count.checked_mul(std::mem::size_of::<CachedRow>());
        let mut bytes = Vec::new();
        let mut ranges = Vec::new();
        let vector_dim = schema
            .vector_column
            .map(|column| match schema.columns[column].ty {
                ColumnType::Vector(dim) => dim,
                _ => 0,
            })
            .unwrap_or(0);
        let mut fits = metadata.is_some_and(|m| m < SCAN_CACHE_BUDGET);
        if fits {
            ranges.reserve_exact(count);
            let metadata = ranges.capacity() * std::mem::size_of::<CachedRow>();
            self.storage.for_each_raw(table, |row| {
                let needed = bytes.len() + row.len();
                // Account for allocated capacities, not just initialized lengths.
                if needed.saturating_add(metadata) > SCAN_CACHE_BUDGET {
                    fits = false; return Ok(false);
                }
                if needed > bytes.capacity() {
                    let target = needed.div_ceil(65536).saturating_mul(65536)
                        .min(SCAN_CACHE_BUDGET - metadata);
                    bytes.reserve_exact(target - bytes.len());
                }
                let pk = crate::storage::record::prefix_pk(row).ok_or_else(|| DbError::new(ErrorCode::Unknown,"invalid cached primary key"))?;
                let vector_start = match schema.vector_column {
                    Some(column) => {
                        let raw = crate::storage::column_bytes(&schema,row,column).ok_or_else(|| DbError::new(ErrorCode::Unknown,"invalid cached vector"))?;
                        bytes.len() + (raw.as_ptr() as usize - row.as_ptr() as usize)
                    }
                    None => 0,
                };
                let start = bytes.len(); bytes.extend_from_slice(row);
                ranges.push(CachedRow {start:start as u32,end:bytes.len() as u32,vector_start:vector_start as u32,pk});
                Ok(true)
            })?;
        }
        if !fits { bytes = Vec::new(); ranges = Vec::new(); }
        let (quantized, quant_min, quant_step, quant_dim) = if fits {
            quantize_cached_vectors(&bytes, &ranges, vector_dim, count)
                .unwrap_or_else(|| (Vec::new(), 0.0, 0.0, 0))
        } else {
            (Vec::new(), 0.0, 0.0, 0)
        };
        self.scan_cache = Some(RawScanCache {
            table: key,
            bytes: fits.then_some(bytes),
            ranges,
            quantized,
            quant_dim,
            quant_min,
            quant_step,
            query_quantized: Vec::new(),
            candidate_flags: Vec::new(),
            quant_scores: Vec::new(),
            quant_histogram: Vec::new(),
            filters: Vec::new(),
            filter_bytes: 0,
            distances: Vec::new(),
            distance_scratch: Vec::new(),
        });
        Ok(())
    }

    /// `ORDER BY <distance> LIMIT n` evaluated straight on encoded rows.
    ///
    /// Only the columns the filter and the distance need are looked at, the row
    /// is decoded only when it survives the bounded collector, and the
    /// distances are computed on the stored `f32` bytes without allocating a
    /// vector per candidate.
    #[allow(clippy::too_many_arguments)]
    fn select_by_distance(
        &mut self,
        table: &str,
        schema: &TableSchema,
        outs: &[OutColumn],
        columns: Vec<String>,
        filter: &RawFilter,
        vector_column: usize,
        query: &[f32],
        ascending: bool,
        keep: usize,
    ) -> DbResult<ResultSet> {
        self.prepare_scan_cache(table)?;
        if self.scan_cache.as_ref().unwrap().bytes.is_some() {
            return select_cached_distance(self.scan_cache.as_mut().unwrap(),schema,outs,columns,filter,query,ascending,keep);
        }
        let mut top = TopK::new(Some(keep), ascending);
        let mut failure: Option<DbError> = None;
        let corrupt = || {
            DbError::new(
                ErrorCode::Unknown,
                format!("corrupt row in table '{}'", schema.name),
            )
        };
        let mut visit = |bytes: &[u8]| {
            if failure.is_some() {
                return Ok(false);
            }
            let Some(pk) = crate::storage::record::prefix_pk(bytes) else {
                failure = Some(corrupt());
                return Ok(false);
            };
            if !filter.matches(schema, bytes) {
                return Ok(true);
            }
            let Some(vector) = crate::storage::column_bytes(schema, bytes, vector_column) else {
                failure = Some(corrupt());
                return Ok(false);
            };
            let Some(squared) = distance::squared_stored(query, vector) else {
                failure = Some(DbError::new(
                    ErrorCode::WrongValue,
                    "vector dimension does not match the query vector",
                ));
                return Ok(false);
            };
            match top.offer(EvalValue::Real(squared), pk, || Ok(Vec::new())) {
                Ok(()) => Ok(true),
                Err(err) => {
                    failure = Some(err);
                    Ok(false)
                }
            }
        };
        let cache = self.scan_cache.as_ref().unwrap();
        if let Some(bytes) = &cache.bytes {
            for row in &cache.ranges { if !visit(&bytes[row.start as usize..row.end as usize])? { break; } }
        } else { self.storage.for_each_raw(table, visit)?; }
        if let Some(err) = failure {
            return Err(err);
        }
        let mut rows = Vec::new();
        for ranked in top.into_ranked() {
            if let Some(row) = self.storage.get(table, ranked.pk)? {
                rows.push(project(outs, &row)?);
            }
        }
        Ok(ResultSet { columns, rows })
    }
}

/// Compact ranking entry: no per-candidate allocation and no heap-file rereads.
#[derive(Clone, Copy)]
struct DistanceRank { distance:f64, pk:i64, index:usize, ascending:bool }
impl Ord for DistanceRank {
    fn cmp(&self,other:&Self)->Ordering {
        let d=self.distance.total_cmp(&other.distance);
        (if self.ascending {d} else {d.reverse()}).then_with(||self.pk.cmp(&other.pk))
    }
}
impl PartialOrd for DistanceRank {fn partial_cmp(&self,other:&Self)->Option<Ordering>{Some(self.cmp(other))}}
impl PartialEq for DistanceRank {fn eq(&self,other:&Self)->bool{self.cmp(other)==Ordering::Equal}}
impl Eq for DistanceRank {}

/// Packs the cached vectors into one byte per dimension with a table-wide
/// affine map, together with the parameters needed to reason about the error.
/// `None` means the table cannot be prefilted and must use the exact path.
fn quantize_cached_vectors(
    bytes: &[u8],
    ranges: &[CachedRow],
    vector_dim: usize,
    count: usize,
) -> Option<(Vec<u8>, f32, f32, usize)> {
    if vector_dim == 0 || count == 0 {
        return None;
    }
    let packed_len = count.checked_mul(vector_dim)?;
    if packed_len > VECTOR_CACHE_BUDGET {
        return None;
    }
    let width = vector_dim.checked_mul(4)?;
    let mut low = f32::INFINITY;
    let mut high = f32::NEG_INFINITY;
    for row in ranges {
        let start = row.vector_start as usize;
        let vector = bytes.get(start..start.checked_add(width)?)?;
        distance::stored_range(vector, &mut low, &mut high);
    }
    if !low.is_finite() || !high.is_finite() || high <= low {
        return None;
    }
    let step = (high - low) / 127.0;
    if !step.is_finite() || step <= 0.0 {
        return None;
    }
    let inverse_step = 1.0 / step;
    let mut packed = vec![0u8; packed_len];
    for (row_index, row) in ranges.iter().enumerate() {
        let start = row.vector_start as usize;
        let vector = bytes.get(start..start + width)?;
        let target = &mut packed[row_index * vector_dim..(row_index + 1) * vector_dim];
        distance::pack_stored_vector(vector, low, inverse_step, target);
    }
    Some((packed, low, step, vector_dim))
}

/// Uses the packed 7-bit vectors as a conservative lower-bound pass. Any row
/// that could still enter the exact TopK is recomputed from the original
/// `f32` bytes, so the extra pass does not change the result or its tie order.
fn select_quantized_topk(
    cache: &mut RawScanCache,
    query: &[f32],
    keep: usize,
    subset: Option<&[usize]>,
) -> DbResult<Option<Vec<DistanceRank>>> {
    let count = cache.ranges.len();
    let dimensions = query.len();
    if keep == 0
        || dimensions == 0
        || cache.quant_dim != dimensions
        || cache.quantized.len() != count.saturating_mul(dimensions)
        || !(cache.quant_step > 0.0)
    {
        return Ok(None);
    }
    let step = cache.quant_step;
    let minimum = cache.quant_min;
    let matched = subset.map_or(count, <[usize]>::len);
    if subset.is_some_and(|list| list.is_empty()) {
        return Ok(None);
    }
    // The packed copy is only a prefilter, so the map is allowed to clamp the
    // query where it falls outside the table range; the real per-dimension
    // reconstruction error is folded into the bound below.
    cache.query_quantized.clear();
    cache.query_quantized.reserve(dimensions);
    let mut query_error = 0.0f32;
    for &value in query {
        if !value.is_finite() {
            return Ok(None);
        }
        let code = ((value - minimum) / step).round().clamp(0.0, 127.0);
        query_error = query_error.max((value - (minimum + code * step)).abs());
        cache.query_quantized.push(code as u8);
    }
    // Stored values are inside the map range, so their own error is `step/2`.
    let error_bound =
        (dimensions as f32).sqrt() * (step * 0.5 + query_error) + 1e-4;
    // Both arrays are sized once per cache; a filtered query only touches the
    // rows its filter selected, so it must not clear the whole table per query.
    if cache.quant_scores.len() != count {
        cache.quant_scores.clear();
        cache.quant_scores.resize(count, 0);
    }
    match subset {
        Some(list) => {
            let RawScanCache {
                query_quantized,
                quantized,
                quant_scores,
                ..
            } = cache;
            for &index in list {
                if index >= count {
                    return Ok(None);
                }
                let start = index * dimensions;
                let row_codes = &quantized[start..start + dimensions];
                let Some(score) = distance::quantized_score_row(query_quantized, row_codes)
                else {
                    return Ok(None);
                };
                quant_scores[index] = score;
            }
        }
        None => {
            let scored = {
                let RawScanCache {
                    query_quantized,
                    quantized,
                    quant_scores,
                    ..
                } = cache;
                distance::quantized_scores(
                    query_quantized,
                    quantized,
                    dimensions,
                    count,
                    quant_scores,
                )
            };
            if !scored {
                return Ok(None);
            }
        }
    }

    // The cutoff only has to bound the true K-th distance from above, so a
    // bucketed order statistic of the packed scores is enough. The counting
    // pass below reuses the same histogram to prove the final cutoff.
    let probe = keep.saturating_mul(4).max(32);
    // A subset barely wider than the probe cannot pay for the extra pass.
    if probe >= matched || matched < probe.saturating_mul(2) {
        return Ok(None);
    }
    cache.quant_histogram.clear();
    cache.quant_histogram.resize(QUANT_BUCKETS, 0);
    let last_bucket = QUANT_BUCKETS - 1;
    match subset {
        Some(list) => {
            for &index in list {
                let bucket =
                    ((cache.quant_scores[index] >> QUANT_BUCKET_SHIFT) as usize).min(last_bucket);
                cache.quant_histogram[bucket] += 1;
            }
        }
        None => {
            for &score in &cache.quant_scores {
                let bucket = ((score >> QUANT_BUCKET_SHIFT) as usize).min(last_bucket);
                cache.quant_histogram[bucket] += 1;
            }
        }
    }
    let mut seen = 0usize;
    let mut threshold = u32::MAX;
    for (bucket, &tally) in cache.quant_histogram.iter().enumerate() {
        seen += tally as usize;
        if seen >= probe {
            // Upper edge of the bucket that holds the probe-th smallest score.
            threshold = (bucket as u32 + 1) << QUANT_BUCKET_SHIFT;
            break;
        }
    }
    if threshold == u32::MAX {
        return Ok(None);
    }

    if cache.candidate_flags.len() != count {
        cache.candidate_flags.clear();
        cache.candidate_flags.resize(count, 0);
    } else {
        match subset {
            Some(list) => {
                for &index in list {
                    cache.candidate_flags[index] = 0;
                }
            }
            None => cache.candidate_flags.fill(0),
        }
    }
    let mut candidates = 0usize;
    let bytes = cache.bytes.as_ref().unwrap();
    let mut top = BinaryHeap::with_capacity(keep.min(count));
    let mut added: Vec<usize> = Vec::with_capacity(probe.saturating_add(64).min(count));
    loop {
        added.clear();
        let mut min_excluded = u32::MAX;
        match subset {
            Some(list) => {
                for &index in list {
                    let score = cache.quant_scores[index];
                    if score <= threshold {
                        if cache.candidate_flags[index] == 0 {
                            cache.candidate_flags[index] = 1;
                            added.push(index);
                        }
                    } else if score < min_excluded {
                        min_excluded = score;
                    }
                }
            }
            None => {
                for (index, &score) in cache.quant_scores.iter().enumerate() {
                    if score <= threshold {
                        if cache.candidate_flags[index] == 0 {
                            cache.candidate_flags[index] = 1;
                            added.push(index);
                        }
                    } else if score < min_excluded {
                        min_excluded = score;
                    }
                }
            }
        }
        candidates += added.len();
        // A wide candidate set would not pay for the extra exact pass.
        if candidates > 4096 {
            return Ok(None);
        }
        for &index in &added {
            offer_exact(&cache.ranges, bytes, query, index, &mut top, keep)?;
        }
        if top.len() < keep {
            return Ok(None);
        }
        let worst = top.peek().unwrap().distance.max(0.0).sqrt();
        let limit = packed_limit(worst, error_bound, step);
        // Every row above the cutoff is at least `error_bound` farther than the
        // current worst exact distance, so it cannot enter the TopK.
        if min_excluded > limit || limit <= threshold {
            break;
        }
        threshold = limit;
    }
    Ok(Some(top.into_sorted_vec()))
}

/// Smallest packed score that still satisfies `sqrt(score) * step > worst +
/// error`, in the integer units produced by [`distance::quantized_scores`].
fn packed_limit(worst: f64, error: f32, step: f32) -> u32 {
    let scaled = ((worst + f64::from(error)) / f64::from(step)).max(0.0);
    let squared = scaled * scaled;
    if !squared.is_finite() || squared >= f64::from(u32::MAX) {
        return u32::MAX;
    }
    // The guard band absorbs the rounding of the float comparison itself.
    (squared as u32).saturating_add(8)
}

fn offer_exact(
    rows: &[CachedRow],
    bytes: &[u8],
    query: &[f32],
    index: usize,
    top: &mut BinaryHeap<DistanceRank>,
    keep: usize,
) -> DbResult<()> {
    let row = &rows[index];
    let start = row.vector_start as usize;
    let vector = bytes
        .get(start..start + query.len() * 4)
        .ok_or_else(|| DbError::new(ErrorCode::Unknown, "invalid cached vector bounds"))?;
    let distance = distance::squared_stored(query, vector)
        .ok_or_else(|| DbError::new(ErrorCode::WrongValue, "invalid cached vector dimension"))?;
    let ranked = DistanceRank {
        distance,
        pk: row.pk,
        index,
        ascending: true,
    };
    if top.len() < keep {
        top.push(ranked);
    } else if top
        .peek()
        .is_some_and(|worst| ranked.cmp(worst) == Ordering::Less)
    {
        *top.peek_mut().unwrap() = ranked;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn select_cached_distance(cache:&mut RawScanCache,schema:&TableSchema,outs:&[OutColumn],columns:Vec<String>,filter:&RawFilter,query:&[f32],ascending:bool,keep:usize)->DbResult<ResultSet> {
    // Reuse bounded exact predicate selections; every visibility-changing command
    // drops this entire cache, including transaction rollback and mutable access.
    let mut selected = cache.filters.iter().position(|entry|entry.filter==*filter);
    if selected.is_none() && !filter.terms.is_empty() {
        let literal_bytes:usize=filter.terms.iter().map(|term|match &term.literal{RawLiteral::Int(_)=>8,RawLiteral::Text(v)=>v.len()}).sum();
        let max_bytes=cache.ranges.len().saturating_mul(std::mem::size_of::<usize>()).saturating_add(literal_bytes).saturating_add(filter.terms.len()*std::mem::size_of::<RawTerm>());
        if max_bytes <= FILTER_CACHE_BUDGET {
            while cache.filters.len()>=32 || cache.filter_bytes.saturating_add(max_bytes)>FILTER_CACHE_BUDGET {
                if cache.filters.is_empty(){break;}
                cache.filter_bytes-=cache.filters.remove(0).bytes;
            }
            let bytes=cache.bytes.as_ref().unwrap();
            let mut indices=Vec::with_capacity(cache.ranges.len());
            for (i,row) in cache.ranges.iter().enumerate() {
                if filter.matches(schema,&bytes[row.start as usize..row.end as usize]) {indices.push(i);}
            }
            indices.shrink_to_fit();
            let allocated=indices.capacity()*std::mem::size_of::<usize>()+literal_bytes+filter.terms.len()*std::mem::size_of::<RawTerm>();
            cache.filters.push(CachedFilter {filter:filter.clone(),indices,bytes:allocated});
            cache.filter_bytes+=allocated;selected=Some(cache.filters.len()-1);
        }
    }
    // Large unfiltered ascending queries start from the packed 7-bit copy: it
    // reads a quarter of the bytes of the cached rows, and its proven error
    // bound keeps the `f64` recompute limited to rows that can still reach the
    // cutoff.
    if ascending && keep > 0 && filter.terms.is_empty() && cache.ranges.len() > 2048 {
        if let Some(ranked) = select_quantized_topk(cache, query, keep, None)? {
            let bytes = cache.bytes.as_ref().unwrap();
            let mut rows = Vec::with_capacity(ranked.len());
            for entry in ranked {
                let row = &cache.ranges[entry.index];
                let values = crate::storage::record::decode_row(
                    schema,
                    &bytes[row.start as usize..row.end as usize],
                )?;
                rows.push(project(outs, &values)?);
            }
            return Ok(ResultSet { columns, rows });
        }
    }
    // A cached filter turns the scan into a scattered subset; the packed copy
    // still applies there, and it reads a quarter of the bytes the f64 path
    // would touch for the same rows.
    if ascending && keep > 0 && cache.ranges.len() > 2048 {
        if let Some(selection) = selected {
            let entry = cache.filters.remove(selection);
            let outcome = select_quantized_topk(cache, query, keep, Some(&entry.indices));
            cache.filters.insert(selection, entry);
            if let Some(ranked) = outcome? {
                let bytes = cache.bytes.as_ref().unwrap();
                let mut rows = Vec::with_capacity(ranked.len());
                for entry in ranked {
                    let row = &cache.ranges[entry.index];
                    let values = crate::storage::record::decode_row(
                        schema,
                        &bytes[row.start as usize..row.end as usize],
                    )?;
                    rows.push(project(outs, &values)?);
                }
                return Ok(ResultSet { columns, rows });
            }
        }
    }
    let bytes=cache.bytes.as_ref().unwrap();

    // For a large cached table, rank with a SIMD f32 pass first and recompute
    // only the rows near the cutoff in f64. Small tables and tie-heavy cases
    // keep the direct exact path, so the output distances and primary-key
    // tie-break remain the reference f64 result.
    if ascending && keep > 0 && selected.is_none() && cache.ranges.len() > 2048 && query.len() >= 16 {
        let count = cache.ranges.len();
        let coarse_cap = keep.saturating_mul(8).max(64).min(count).min(4096);
        if coarse_cap < count {
            cache.distances.clear();
            cache.distances.resize(count, f32::INFINITY);
            let mut finite = true;
            {
                let scores = &mut cache.distances;
                let mut score = |index: usize| -> DbResult<()> {
                    let row = &cache.ranges[index];
                    let start = row.vector_start as usize;
                    let vector = bytes
                        .get(start..start + query.len() * 4)
                        .ok_or_else(|| DbError::new(ErrorCode::Unknown, "invalid cached vector bounds"))?;
                    let value = distance::squared_stored_f32(query, vector)
                        .ok_or_else(|| DbError::new(ErrorCode::WrongValue, "invalid cached vector dimension"))?;
                    if value.is_finite() {
                        scores[index] = value;
                    } else {
                        finite = false;
                    }
                    Ok(())
                };
                if let Some(selection) = selected {
                    for &index in &cache.filters[selection].indices {
                        score(index)?;
                    }
                } else {
                    for index in 0..count {
                        score(index)?;
                    }
                }
            }
            if finite {
                let boundary = {
                    cache.distance_scratch.clear();
                    cache.distance_scratch.extend_from_slice(&cache.distances);
                    let (_, nth, _) = cache
                        .distance_scratch
                        .select_nth_unstable_by(coarse_cap - 1, |a, b| a.total_cmp(b));
                    *nth
                };
                if boundary.is_finite() {
                    let limit = boundary + boundary.abs() * 1e-4 + 1e-6;
                    let mut candidates = Vec::with_capacity(coarse_cap + 16);
                    for (index, &value) in cache.distances.iter().enumerate() {
                        if value <= limit {
                            candidates.push(index);
                            if candidates.len() > 4096 {
                                break;
                            }
                        }
                    }
                    if !candidates.is_empty() && candidates.len() <= 4096 {
                        let mut top: BinaryHeap<DistanceRank> =
                            BinaryHeap::with_capacity(keep.min(candidates.len()));
                        for index in candidates {
                            let row = &cache.ranges[index];
                            let start = row.vector_start as usize;
                            let vector = bytes
                                .get(start..start + query.len() * 4)
                                .ok_or_else(|| DbError::new(ErrorCode::Unknown, "invalid cached vector bounds"))?;
                            let value = distance::squared_stored(query, vector)
                                .ok_or_else(|| DbError::new(ErrorCode::WrongValue, "invalid cached vector dimension"))?;
                            let ranked = DistanceRank { distance: value, pk: row.pk, index, ascending };
                            if top.len() < keep {
                                top.push(ranked);
                            } else if top.peek().is_some_and(|worst| ranked.cmp(worst) == Ordering::Less) {
                                *top.peek_mut().unwrap() = ranked;
                            }
                        }
                        let mut rows = Vec::with_capacity(top.len());
                        for ranked in top.into_sorted_vec() {
                            let row = &cache.ranges[ranked.index];
                            let values = crate::storage::record::decode_row(
                                schema,
                                &bytes[row.start as usize..row.end as usize],
                            )?;
                            rows.push(project(outs, &values)?);
                        }
                        return Ok(ResultSet { columns, rows });
                    }
                }
            }
        }
    }

    let mut top:BinaryHeap<DistanceRank>=BinaryHeap::with_capacity(keep.min(cache.ranges.len()).min(16384));
    let mut offer=|index:usize|->DbResult<()> {
        let row=&cache.ranges[index];
        if selected.is_none() && !filter.matches(schema,&bytes[row.start as usize..row.end as usize]) {return Ok(());}
        let start=row.vector_start as usize;
        let vector=bytes.get(start..start+query.len()*4).ok_or_else(||DbError::new(ErrorCode::Unknown,"invalid cached vector bounds"))?;
        let distance=distance::squared_stored(query,vector).ok_or_else(||DbError::new(ErrorCode::WrongValue,"invalid cached vector dimension"))?;
        let ranked=DistanceRank{distance,pk:row.pk,index,ascending};
        if top.len()<keep {top.push(ranked);}
        else if top.peek().is_some_and(|worst|ranked.cmp(worst)==Ordering::Less){*top.peek_mut().unwrap()=ranked;}
        Ok(())
    };
    if let Some(selection)=selected {
        for &index in &cache.filters[selection].indices {offer(index)?;}
    } else {
        for index in 0..cache.ranges.len(){offer(index)?;}
    }
    let mut rows=Vec::with_capacity(top.len());
    for ranked in top.into_sorted_vec() {
        let row=&cache.ranges[ranked.index];
        let values=crate::storage::record::decode_row(schema,&bytes[row.start as usize..row.end as usize])?;
        rows.push(project(outs,&values)?);
    }
    Ok(ResultSet {columns,rows})
}

/// A `WHERE` clause that only compares a plain column with a literal, so it can
/// be evaluated on encoded row bytes.
#[derive(Clone, PartialEq)]
struct RawFilter {
    terms: Vec<RawTerm>,
}

#[derive(Clone, PartialEq)]
struct RawTerm {
    column: usize,
    op: CmpOp,
    literal: RawLiteral,
}

#[derive(Clone, PartialEq)]
enum RawLiteral {
    Int(i64),
    Text(Vec<u8>),
}

impl RawFilter {
    /// `None` means the filter needs the general evaluator.
    fn build(schema: &TableSchema, filter: Option<&Expr>) -> DbResult<Option<Self>> {
        let mut terms = Vec::new();
        let supported = match filter {
            Some(filter) => collect_filter_terms(schema, filter, &mut terms)?,
            None => true,
        };
        Ok(if supported { Some(Self { terms }) } else { None })
    }

    fn matches(&self, schema: &TableSchema, bytes: &[u8]) -> bool {
        self.terms.iter().all(|term| {
            let Some(raw) = crate::storage::column_bytes(schema, bytes, term.column) else {
                return false;
            };
            let ord = match &term.literal {
                RawLiteral::Int(expected) => {
                    let Ok(raw) = <[u8; 8]>::try_from(raw) else {
                        return false;
                    };
                    i64::from_le_bytes(raw).cmp(expected)
                }
                RawLiteral::Text(expected) => raw.cmp(expected.as_slice()),
            };
            match term.op {
                CmpOp::Eq => ord == Ordering::Equal,
                CmpOp::Lt => ord == Ordering::Less,
                CmpOp::Le => ord != Ordering::Greater,
                CmpOp::Gt => ord == Ordering::Greater,
                CmpOp::Ge => ord != Ordering::Less,
            }
        })
    }
}

fn collect_filter_terms(
    schema: &TableSchema,
    expr: &Expr,
    out: &mut Vec<RawTerm>,
) -> DbResult<bool> {
    match expr {
        Expr::And(left, right) => {
            Ok(collect_filter_terms(schema, left, out)?
                && collect_filter_terms(schema, right, out)?)
        }
        Expr::Compare { left, op, right } => {
            let (Expr::Column(name), Expr::Lit(value)) = (&**left, &**right) else {
                return Ok(false);
            };
            let (index, def) = schema.column(name)?;
            let literal = match (&def.ty, value) {
                (ColumnType::BigInt, Value::Int(v)) => RawLiteral::Int(*v),
                (ColumnType::Varchar(_), Value::Text(v)) => {
                    RawLiteral::Text(v.as_bytes().to_vec())
                }
                _ => return Ok(false),
            };
            out.push(RawTerm {
                column: index,
                op: *op,
                literal,
            });
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// One scored row held by [`TopK`].
struct Ranked {
    key: EvalValue,
    pk: i64,
    ascending: bool,
    cells: Vec<Cell>,
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        // Ties are broken by primary key ascending in both directions, as the
        // task defines equal-to-equal results deterministically.
        let primary = total_cmp(&self.key, &other.key);
        let ordered = if self.ascending {
            primary
        } else {
            primary.reverse()
        };
        ordered.then_with(|| self.pk.cmp(&other.pk))
    }
}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Ranked {}

/// Keeps the `limit` rows that sort first, so a `SELECT ... ORDER BY ... LIMIT`
/// over a large table allocates O(limit) instead of O(rows).
///
/// Without a limit every row is kept: the task requires a fully ordered result
/// when `LIMIT` is absent, so the rows have to be materialised anyway.
struct TopK {
    limit: Option<usize>,
    ascending: bool,
    heap: BinaryHeap<Ranked>,
}

impl TopK {
    fn new(limit: Option<usize>, ascending: bool) -> Self {
        let capacity = limit.unwrap_or(0).min(16 * 1024);
        Self {
            limit,
            ascending,
            heap: BinaryHeap::with_capacity(capacity),
        }
    }

    /// Offers one row; the worst kept row is evicted once `limit` is reached.
    ///
    /// The output cells are built only for rows that end up being kept, so a
    /// scan that rejects millions of candidates allocates nothing per row.
    fn offer<F>(&mut self, key: EvalValue, pk: i64, cells: F) -> DbResult<()>
    where
        F: FnOnce() -> DbResult<Vec<Cell>>,
    {
        let mut ranked = Ranked {
            key,
            pk,
            ascending: self.ascending,
            cells: Vec::new(),
        };
        match self.limit {
            Some(limit) if self.heap.len() >= limit => {
                let better = self
                    .heap
                    .peek()
                    .is_some_and(|worst| ranked.cmp(worst) == Ordering::Less);
                if !better {
                    return Ok(());
                }
                self.heap.pop();
            }
            _ => {}
        }
        ranked.cells = cells()?;
        self.heap.push(ranked);
        Ok(())
    }

    fn into_rows(self) -> Vec<Vec<Cell>> {
        self.into_ranked()
            .into_iter()
            .map(|ranked| ranked.cells)
            .collect()
    }

    /// The kept rows in result order, before their output cells are attached.
    fn into_ranked(self) -> Vec<Ranked> {
        self.heap.into_sorted_vec()
    }
}

/// Resolves the `INSERT` column list into storage column order.
fn column_order(schema: &TableSchema, columns: Option<&[String]>) -> DbResult<Vec<usize>> {
    let Some(names) = columns else {
        return Ok((0..schema.columns.len()).collect());
    };
    let mut seen = vec![false; schema.columns.len()];
    let mut order = Vec::with_capacity(names.len());
    for name in names {
        let (index, _) = schema.column(name)?;
        if std::mem::replace(&mut seen[index], true) {
            return Err(DbError::new(
                ErrorCode::DuplicateColumn,
                format!("column '{name}' is specified twice"),
            ));
        }
        order.push(index);
    }
    if let Some(missing) = seen.iter().position(|used| !used) {
        return Err(DbError::new(
            ErrorCode::WrongValueCount,
            format!(
                "column '{}' has no value and columns have no defaults",
                schema.columns[missing].name
            ),
        ));
    }
    Ok(order)
}

// ----- projection -------------------------------------------------------

#[derive(Clone, Debug)]
enum OutKind {
    Column(usize),
    Distance { column: usize, query: Vec<f32> },
}

#[derive(Clone, Debug)]
struct OutColumn {
    name: String,
    kind: OutKind,
}

fn plan_outputs(schema: &TableSchema, items: &[SelectItem]) -> DbResult<Vec<OutColumn>> {
    let mut outs = Vec::new();
    for item in items {
        match item {
            SelectItem::Star => {
                for (index, def) in schema.columns.iter().enumerate() {
                    outs.push(OutColumn {
                        name: def.name.clone(),
                        kind: OutKind::Column(index),
                    });
                }
            }
            SelectItem::Column(name) => {
                let (index, def) = schema.column(name)?;
                outs.push(OutColumn {
                    name: def.name.clone(),
                    kind: OutKind::Column(index),
                });
            }
            SelectItem::AliasedColumn { column, alias } => {
                let (index, _) = schema.column(column)?;
                outs.push(OutColumn { name: alias.clone(), kind: OutKind::Column(index) });
            }
            SelectItem::L2Distance {
                column,
                query,
                alias,
            } => {
                let (index, def) = schema.column(column)?;
                let ColumnType::Vector(dim) = def.ty else {
                    return Err(DbError::new(
                        ErrorCode::WrongValue,
                        format!("column '{}' is not a VECTOR column", def.name),
                    ));
                };
                if dim != query.len() {
                    return Err(DbError::new(
                        ErrorCode::WrongValue,
                        format!(
                            "query vector has {} dimensions but column '{}' has {dim}",
                            query.len(),
                            def.name
                        ),
                    ));
                }
                outs.push(OutColumn {
                    name: alias
                        .clone()
                        .unwrap_or_else(|| format!("l2_distance({})", def.name)),
                    kind: OutKind::Distance {
                        column: index,
                        query: query.clone(),
                    },
                });
            }
        }
    }
    if outs.is_empty() {
        return Err(DbError::new(ErrorCode::Syntax, "SELECT list is empty"));
    }
    Ok(outs)
}

fn project(outs: &[OutColumn], row: &[Value]) -> DbResult<Vec<Cell>> {
    outs.iter()
        .map(|out| match &out.kind {
            OutKind::Column(index) => Ok(cell_from_value(&row[*index])),
            OutKind::Distance { column, query } => {
                let EvalValue::Vector(vector) = EvalValue::from(&row[*column]) else {
                    return Err(DbError::new(
                        ErrorCode::WrongValue,
                        "expected a VECTOR column",
                    ));
                };
                let d = distance::distance(&vector, query).ok_or_else(|| {
                    DbError::new(
                        ErrorCode::WrongValue,
                        "vector dimension does not match the query vector",
                    )
                })?;
                Ok(Cell::Real(d))
            }
        })
        .collect()
}

fn cell_from_value(value: &Value) -> Cell {
    match value {
        Value::Int(v) => Cell::Int(*v),
        Value::Text(v) => Cell::Text(v.clone()),
        Value::Vector(v) => Cell::Vector(v.clone()),
    }
}

/// `SELECT 1 [AS one], 'x'`: one row built from literals only. As in MySQL the
/// column name defaults to the literal's own text.
fn select_const(items: &[(Value, Option<String>)]) -> ResultSet {
    let mut columns = Vec::with_capacity(items.len());
    let mut row = Vec::with_capacity(items.len());
    for (value, alias) in items {
        let cell = cell_from_value(value);
        columns.push(alias.clone().unwrap_or_else(|| cell.render()));
        row.push(cell);
    }
    ResultSet {
        columns,
        rows: vec![row],
    }
}

// ----- expressions ------------------------------------------------------

/// Values as seen by the evaluator: stored cells plus computed distances.
#[derive(Clone, Debug, PartialEq)]
enum EvalValue {
    Int(i64),
    Real(f64),
    Text(String),
    Vector(Vec<f32>),
}

impl From<&Value> for EvalValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Int(v) => EvalValue::Int(*v),
            Value::Text(v) => EvalValue::Text(v.clone()),
            Value::Vector(v) => EvalValue::Vector(v.clone()),
        }
    }
}

fn validate_expr(schema: &TableSchema, expr: &Expr) -> DbResult<()> {
    match expr {
        Expr::Column(name) => { schema.column(name)?; }
        Expr::Lit(_) => {}
        Expr::Distance { column, query } => {
            let (_, def) = schema.column(column)?;
            if !matches!(def.ty, ColumnType::Vector(dim) if dim == query.len()) {
                return Err(DbError::new(ErrorCode::WrongValue, "invalid distance vector type or dimensions"));
            }
        }
        Expr::Compare { left, right, .. } | Expr::And(left, right) => { validate_expr(schema,left)?; validate_expr(schema,right)?; }
        Expr::In { left, .. } => { validate_expr(schema,left)?; }
    }
    Ok(())
}

fn filter_matches(expr: &Expr, row: &[Value], schema: &TableSchema) -> DbResult<bool> {
    match expr {
        Expr::Compare { left, op, right } => {
            let left = eval(left, row, schema)?;
            let right = eval(right, row, schema)?;
            let ord = total_cmp(&left, &right);
            Ok(match op {
                CmpOp::Eq => ord == Ordering::Equal,
                CmpOp::Lt => ord == Ordering::Less,
                CmpOp::Le => ord != Ordering::Greater,
                CmpOp::Gt => ord == Ordering::Greater,
                CmpOp::Ge => ord != Ordering::Less,
            })
        }
        Expr::And(left, right) => {
            Ok(filter_matches(left, row, schema)? && filter_matches(right, row, schema)?)
        }
        Expr::In { left, values } => {
            let left = eval(left, row, schema)?;
            Ok(values
                .iter()
                .any(|value| total_cmp(&left, &EvalValue::from(value)) == Ordering::Equal))
        }
        _ => Err(DbError::new(
            ErrorCode::NotSupported,
            "WHERE must combine comparisons with AND",
        )),
    }
}

fn eval(expr: &Expr, row: &[Value], schema: &TableSchema) -> DbResult<EvalValue> {
    match expr {
        Expr::Column(name) => {
            let (index, _) = schema.column(name)?;
            Ok(EvalValue::from(&row[index]))
        }
        Expr::Lit(value) => Ok(EvalValue::from(value)),
        Expr::Distance { column, query } => {
            let (index, def) = schema.column(column)?;
            if !matches!(def.ty, ColumnType::Vector(_)) {
                return Err(DbError::new(
                    ErrorCode::WrongValue,
                    format!("column '{}' is not a VECTOR column", def.name),
                ));
            }
            let EvalValue::Vector(vector) = EvalValue::from(&row[index]) else {
                return Err(DbError::new(
                    ErrorCode::WrongValue,
                    "expected a VECTOR column",
                ));
            };
            let d = distance::distance(&vector, query).ok_or_else(|| {
                DbError::new(
                    ErrorCode::WrongValue,
                    "vector dimension does not match the query vector",
                )
            })?;
            Ok(EvalValue::Real(d))
        }
        Expr::Compare { .. } | Expr::And(_, _) | Expr::In { .. } => {
            filter_matches(expr, row, schema).map(|b| EvalValue::Int(i64::from(b)))
        }
    }
}

/// Total order over evaluator values. Mixed numeric/text pairs compare
/// numerically when the text parses as a number and lexicographically
/// otherwise; vectors never appear in `WHERE`/`ORDER BY` keys but are ordered
/// lexicographically for completeness.
fn total_cmp(a: &EvalValue, b: &EvalValue) -> Ordering {
    match (a, b) {
        (EvalValue::Int(x), EvalValue::Int(y)) => x.cmp(y),
        (EvalValue::Text(x), EvalValue::Text(y)) => x.cmp(y),
        (EvalValue::Vector(x), EvalValue::Vector(y)) => x
            .iter()
            .zip(y)
            .map(|(a, b)| a.total_cmp(b))
            .find(|o| o.is_ne())
            .unwrap_or_else(|| x.len().cmp(&y.len())),
        _ => match (as_f64(a), as_f64(b)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => as_text(a).cmp(&as_text(b)),
        },
    }
}

fn as_f64(value: &EvalValue) -> Option<f64> {
    match value {
        EvalValue::Int(v) => Some(*v as f64),
        EvalValue::Real(v) => Some(*v),
        EvalValue::Text(v) => v.trim().parse().ok(),
        EvalValue::Vector(_) => None,
    }
}

fn as_text(value: &EvalValue) -> String {
    match value {
        EvalValue::Int(v) => v.to_string(),
        EvalValue::Real(v) => v.to_string(),
        EvalValue::Text(v) => v.clone(),
        EvalValue::Vector(v) => format!("{v:?}"),
    }
}

// ----- ORDER BY ---------------------------------------------------------

/// A resolved sort key: a projected output cell, a table column, or a distance.
#[derive(Clone, Debug)]
enum OrderKey {
    Output(usize),
    Column(usize),
    Distance { column: usize, query: Vec<f32> },
}

fn resolve_order(schema: &TableSchema, outs: &[OutColumn], expr: &Expr) -> DbResult<OrderKey> {
    match expr {
        Expr::Column(name) => {
            if let Some(index) = outs.iter().position(|o| o.name.eq_ignore_ascii_case(name)) {
                return Ok(OrderKey::Output(index));
            }
            let (index, _) = schema.column(name)?;
            Ok(OrderKey::Column(index))
        }
        Expr::Distance { column, query } => {
            let (index, def) = schema.column(column)?;
            let ColumnType::Vector(dim) = def.ty else {
                return Err(DbError::new(
                    ErrorCode::WrongValue,
                    format!("column '{}' is not a VECTOR column", def.name),
                ));
            };
            if dim != query.len() {
                return Err(DbError::new(
                    ErrorCode::WrongValue,
                    format!(
                        "query vector has {} dimensions but column '{}' has {dim}",
                        query.len(),
                        def.name
                    ),
                ));
            }
            Ok(OrderKey::Distance {
                column: index,
                query: query.clone(),
            })
        }
        other => Err(DbError::new(
            ErrorCode::NotSupported,
            format!("unsupported ORDER BY expression: {other:?}"),
        )),
    }
}

fn eval_order(key: &OrderKey, outs: &[OutColumn], row: &[Value]) -> DbResult<EvalValue> {
    match key {
        OrderKey::Output(index) => {
            // Recomputing the projection here keeps sorting independent of the
            // rendered cells, which lose precision for vectors.
            let out = &outs[*index];
            match &out.kind {
                OutKind::Column(column) => Ok(EvalValue::from(&row[*column])),
                OutKind::Distance { column, query } => {
                    let EvalValue::Vector(vector) = EvalValue::from(&row[*column]) else {
                        return Err(DbError::new(
                            ErrorCode::WrongValue,
                            "expected a VECTOR column",
                        ));
                    };
                    let d = distance::distance(&vector, query).ok_or_else(|| {
                        DbError::new(
                            ErrorCode::WrongValue,
                            "vector dimension does not match the query vector",
                        )
                    })?;
                    Ok(EvalValue::Real(d))
                }
            }
        }
        OrderKey::Column(index) => Ok(EvalValue::from(&row[*index])),
        OrderKey::Distance { column, query } => {
            let EvalValue::Vector(vector) = EvalValue::from(&row[*column]) else {
                return Err(DbError::new(
                    ErrorCode::WrongValue,
                    "expected a VECTOR column",
                ));
            };
            let d = distance::distance(&vector, query).ok_or_else(|| {
                DbError::new(
                    ErrorCode::WrongValue,
                    "vector dimension does not match the query vector",
                )
            })?;
            Ok(EvalValue::Real(d))
        }
    }
}

#[cfg(test)]
mod cache_budget_tests {
 use super::*;
 #[test]
 fn oversized_rows_stream_and_allocated_cache_capacities_stay_bounded(){
  let dir=std::env::temp_dir().join(format!("mvdb-cache-budget-{}",std::process::id()));
  let _=std::fs::remove_dir_all(&dir);let mut s=Session::open(&dir).unwrap();
  assert!(matches!(s.execute("CREATE TABLE t(id BIGINT PRIMARY KEY,g BIGINT,txt VARCHAR(4000),v VECTOR(2))"),Response::Ok{..}));
  for batch in 0..30 {
   let values=(batch*100..(batch+1)*100).map(|i|format!("({i},{},'{}','[{},0]')",i%40,"x".repeat(3000),i%7)).collect::<Vec<_>>().join(",");
   assert!(matches!(s.execute(&format!("INSERT INTO t VALUES {values}")),Response::Ok{..}));
  }
  for group in [0,1,39,44] {
   let response=s.execute(&format!("SELECT id FROM t WHERE g={group} ORDER BY L2_DISTANCE(v,'[0,0]') LIMIT 3"));
   let Response::Rows(rs)=response else {panic!("{response:?}")};
   let mut expected:Vec<i64>=(0..3000).filter(|i|i%40==group).collect();expected.sort_by_key(|i|(i%7,*i));expected.truncate(3);
   assert_eq!(rs.rows,expected.into_iter().map(|i|vec![Cell::Int(i)]).collect::<Vec<_>>());
  }
  let cache=s.scan_cache.as_ref().unwrap();assert!(cache.bytes.is_none());assert_eq!(cache.ranges.capacity(),0);
  s.execute("DELETE FROM t WHERE id>=1000");
  for group in 0..40 {s.execute(&format!("SELECT id FROM t WHERE g={group} ORDER BY L2_DISTANCE(v,'[0,0]') LIMIT 3"));}
  let cache=s.scan_cache.as_ref().unwrap();
  assert!(cache.bytes.as_ref().unwrap().capacity()+cache.ranges.capacity()*std::mem::size_of::<CachedRow>()<=SCAN_CACHE_BUDGET);
  assert!(cache.filter_bytes<=FILTER_CACHE_BUDGET);assert!(cache.filters.len()<=32);
  drop(s);std::fs::remove_dir_all(dir).unwrap();
 }
}

#[cfg(test)]
mod packed_prefilter_tests {
    use super::*;

    /// Coarse grid so many rows share distances; the duplicate values also
    /// exercise the primary-key tie order.
    fn row_vector(index: usize) -> Vec<f32> {
        (0..8)
            .map(|dimension| ((index * 7 + dimension * 13) % 17) as f32 * 0.5)
            .collect()
    }

    fn brute_force(rows: &[Vec<f32>], query: &[f32], keep: usize) -> Vec<(f64, i64)> {
        let mut best: Vec<(f64, i64)> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                (
                    distance::squared(row, query).unwrap(),
                    i64::try_from(index).unwrap() + 1,
                )
            })
            .collect();
        best.sort_by(|left, right| left.0.total_cmp(&right.0).then(left.1.cmp(&right.1)));
        best.truncate(keep);
        best
    }

    #[test]
    fn packed_prefilter_keeps_exact_order_for_ties_and_out_of_range_queries() {
        let dir = std::env::temp_dir().join(format!("mvdb-packed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::open(&dir).unwrap();
        assert!(matches!(
            session.execute("CREATE TABLE t(id BIGINT PRIMARY KEY, g BIGINT, v VECTOR(8))"),
            Response::Ok { .. }
        ));
        let rows: Vec<Vec<f32>> = (0..3000).map(row_vector).collect();
        for batch in 0..30 {
            let values = (batch * 100..(batch + 1) * 100)
                .map(|index| {
                    let vector = rows[index]
                        .iter()
                        .map(|value| value.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("({},{},'[{vector}]')", index + 1, index % 5)
                })
                .collect::<Vec<_>>()
                .join(",");
            assert!(matches!(
                session.execute(&format!("INSERT INTO t VALUES {values}")),
                Response::Ok { .. }
            ));
        }
        session.prepare_scan_cache("t").unwrap();
        assert!(session.scan_cache.as_ref().unwrap().quant_dim == 8);

        let mut answered = 0;
        for probe in 0..40 {
            let query: Vec<f32> = (0..8)
                .map(|dimension| {
                    if probe % 5 == 4 {
                        // Far outside the stored range, so the query side of the
                        // affine map clamps and contributes a larger error.
                        40.0 + dimension as f32
                    } else {
                        ((probe * 31 + dimension * 11) % 23) as f32 * 0.5
                    }
                })
                .collect();
            let expected = brute_force(&rows, &query, 10);
            let cache = session.scan_cache.as_mut().unwrap();
            if let Some(ranked) = select_quantized_topk(cache, &query, 10, None).unwrap() {
                let got: Vec<(f64, i64)> =
                    ranked.iter().map(|entry| (entry.distance, entry.pk)).collect();
                assert_eq!(got, expected, "probe {probe}");
                answered += 1;
            }
        }
        assert!(answered >= 20, "packed prefilter answered only {answered}/40");
        drop(session);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

