//! Local reproduction of the evaluation benchmark.
//!
//! Shape comes from the task statement: 10,000 rows with 128-dimension
//! vectors, loaded in batches of 100, then 1,000 TopK (K = 10) queries of
//! which half carry an equality filter on a plain column.
//!
//! Run with `cargo run --release --example bench`. Wall clock, process I/O
//! (`/proc/self/io`) and peak resident memory (`VmHWM`) are local proxies.
//! Official scoring uses a frozen driver and cgroup accounting including cache;
//! these measurements cannot predict the final rank or score.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use minivectordb::executor::Session;
use minivectordb::response::{Cell, Response};

const ROWS: usize = 10_000;
const DIM: usize = 128;
const BATCH: usize = 100;
const QUERIES: usize = 1_000;
const K: usize = 10;
const CATEGORIES: i64 = 20;

/// Deterministic little generator so runs are comparable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn vector(&mut self) -> String {
        let mut text = String::with_capacity(DIM * 9 + 2);
        text.push('[');
        for i in 0..DIM {
            if i > 0 {
                text.push(',');
            }
            let _ = write!(text, "{:.4}", self.unit());
        }
        text.push(']');
        text
    }
}

fn main() {
    let dir = std::env::temp_dir().join(format!("mvdb-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut session = Session::open(&dir).unwrap();

    ok(&mut session, "CREATE TABLE docs (id BIGINT PRIMARY KEY, category BIGINT, emb VECTOR(128))");

    let queries: Vec<(Option<i64>, String)> = (0..QUERIES)
        .map(|i| {
            let category = if i % 2 == 0 {
                Some((rng.next() % CATEGORIES as u64) as i64)
            } else {
                None
            };
            (category, rng.vector())
        })
        .collect();

    // ---- write phase ---------------------------------------------------
    let mut payloads: Vec<String> = Vec::with_capacity(ROWS / BATCH);
    for batch in 0..ROWS / BATCH {
        let mut sql = String::from("INSERT INTO docs VALUES ");
        for i in 0..BATCH {
            let pk = batch * BATCH + i + 1;
            let category = (pk as i64) % CATEGORIES;
            if i > 0 {
                sql.push_str(", ");
            }
            let _ = write!(sql, "({pk}, {category}, '{}')", rng.vector());
        }
        payloads.push(sql);
    }

    let start_io = process_io();
    let write_start = Instant::now();
    for sql in &payloads {
        ok(&mut session, sql);
    }
    let write_time = write_start.elapsed();
    let write_io = process_io().since(start_io);

    // ---- where the query time goes -------------------------------------
    let t = Instant::now();
    let mut decoded_values = 0usize;
    session
        .storage()
        .for_each_row("docs", |row| {
            decoded_values += row.len();
            Ok(true)
        })
        .unwrap();
    let decode_scan = t.elapsed();

    let t = Instant::now();
    let mut raw_rows = 0usize;
    {
        let table = session.storage().table("docs").unwrap();
        table
            .heap
            .for_each_live(|_, _, _| {
                raw_rows += 1;
                Ok(())
            })
            .unwrap();
    }
    let raw_scan = t.elapsed();

    // ---- query phase ---------------------------------------------------
    let query_io_start = process_io();
    let query_start = Instant::now();
    let mut filtered = std::time::Duration::ZERO;
    let mut unfiltered = std::time::Duration::ZERO;
    let mut seen = 0usize;
    for (category, vector) in &queries {
        let one_start = Instant::now();
        let sql = match category {
            Some(category) => format!(
                "SELECT id, L2_DISTANCE(emb, '{vector}') AS d FROM docs WHERE category = {category} ORDER BY d LIMIT {K}"
            ),
            None => format!(
                "SELECT id, L2_DISTANCE(emb, '{vector}') AS d FROM docs ORDER BY d LIMIT {K}"
            ),
        };
        let Response::Rows(rows) = session.execute(&sql) else {
            panic!("query failed: {sql}");
        };
        check_topk(&rows.rows, *category);
        seen += rows.rows.len();
        if category.is_some() {
            filtered += one_start.elapsed();
        } else {
            unfiltered += one_start.elapsed();
        }
    }
    let query_time = query_start.elapsed();
    let query_io = process_io().since(query_io_start);

    // How much of the write phase is the durable commit itself?
    let commit_start = Instant::now();
    for _ in 0..200 {
        ok(&mut session, "BEGIN");
        ok(&mut session, "COMMIT");
    }
    let commit_time = commit_start.elapsed();

    println!("rows            : {ROWS} x {DIM} dims");
    println!("empty commit    : {:>8.3} ms/commit", ms(commit_time) / 200.0);
    println!("write           : {:>8.1} ms  ({:>7.2} ms/batch)", ms(write_time), ms(write_time) / (ROWS / BATCH) as f64);
    println!("write io        : read {:>6} KiB  write {:>6} KiB", write_io.read / 1024, write_io.write / 1024);
    println!("query           : {:>8.1} ms  ({:>7.3} ms/query)", ms(query_time), ms(query_time) / QUERIES as f64);
    println!("query filtered  : {:>8.1} ms  ({:>7.3} ms/query)", ms(filtered), ms(filtered) / (QUERIES / 2) as f64);
    println!("query unfiltered: {:>8.1} ms  ({:>7.3} ms/query)", ms(unfiltered), ms(unfiltered) / (QUERIES / 2) as f64);
    println!("query io        : read {:>6} KiB  write {:>6} KiB", query_io.read / 1024, query_io.write / 1024);
    println!("rows returned   : {seen}");
    println!("scan + decode   : {:>8.1} ms ({decoded_values} values)", ms(decode_scan));
    println!("scan raw bytes  : {:>8.1} ms ({raw_rows} rows)", ms(raw_scan));
    println!("peak memory     : {:>8} MiB (VmHWM)", peak_memory_mib());

    let _ = std::fs::remove_dir_all(PathBuf::from(&dir));
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn ok(session: &mut Session, sql: &str) -> Response {
    match session.execute(sql) {
        Response::Err(err) => panic!("{sql}: {err}"),
        other => other,
    }
}

fn check_topk(rows: &[Vec<Cell>], category: Option<i64>) {
    assert!(rows.len() <= K, "more than K rows");
    let mut pks = std::collections::HashSet::new();
    let mut last = f64::NEG_INFINITY;
    for row in rows {
        let Cell::Int(pk) = row[0] else {
            panic!("bad id cell");
        };
        assert!(pks.insert(pk), "duplicate id {pk}");
        let Cell::Real(d) = row[1] else {
            panic!("bad distance cell");
        };
        assert!(d >= last, "distance is not ascending");
        last = d;
        if let Some(category) = category {
            assert_eq!(
                pk % CATEGORIES,
                category,
                "row {pk} does not satisfy category = {category}"
            );
        }
    }
}

#[derive(Clone, Copy)]
struct Io {
    read: u64,
    write: u64,
}

impl Io {
    fn since(self, before: Io) -> Io {
        Io {
            read: self.read.saturating_sub(before.read),
            write: self.write.saturating_sub(before.write),
        }
    }
}

fn process_io() -> Io {
    let text = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let mut io = Io { read: 0, write: 0 };
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("read_bytes: ") {
            io.read = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("write_bytes: ") {
            io.write = value.trim().parse().unwrap_or(0);
        }
    }
    io
}

fn peak_memory_mib() -> u64 {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("VmHWM:") {
            let kib: u64 = value.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0);
            return kib / 1024;
        }
    }
    0
}
