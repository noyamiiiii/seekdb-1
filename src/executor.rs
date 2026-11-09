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
struct RawScanCache {
    table: String,
    bytes: Option<Vec<u8>>,
    ranges: Vec<(usize, usize)>,
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
        const BUDGET: usize = 8 * 1024 * 1024;
        let key = table.to_ascii_lowercase();
        if self.scan_cache.as_ref().is_some_and(|cache| cache.table == key) { return Ok(()); }
        let mut bytes = Vec::new();
        let mut ranges = Vec::new();
        let mut fits = true;
        self.storage.for_each_raw(table, |row| {
            if bytes.len().saturating_add(row.len()).saturating_add((ranges.len()+1)*16) > BUDGET {
                fits = false;return Ok(false);
            }
            let start = bytes.len();bytes.extend_from_slice(row);ranges.push((start,bytes.len()));
            Ok(true)
        })?;
        self.scan_cache = Some(if fits { RawScanCache { table:key, bytes:Some(bytes), ranges } }
            else { RawScanCache { table:key, bytes:None, ranges:Vec::new() } });
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
            for &(start,end) in &cache.ranges { if !visit(&bytes[start..end])? { break; } }
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

/// A `WHERE` clause that only compares a plain column with a literal, so it can
/// be evaluated on encoded row bytes.
struct RawFilter {
    terms: Vec<RawTerm>,
}

struct RawTerm {
    column: usize,
    op: CmpOp,
    literal: RawLiteral,
}

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
