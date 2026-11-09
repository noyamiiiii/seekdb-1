//! Statement-level tests: the SQL semantics the task enumerates (filtered
//! TopK, deterministic tie-breaking, transaction visibility, error codes).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use minivectordb::error::ErrorCode;
use minivectordb::executor::Session;
use minivectordb::response::{Cell, Response};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mvdb-exec-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn session(dir: &TempDir) -> Session {
    Session::open(&dir.0).unwrap()
}

/// Runs statements that must all succeed, returning the last response.
fn run_ok(session: &mut Session, sql: &str) -> Response {
    match session.execute(sql) {
        Response::Err(err) => panic!("{sql}: {err}"),
        other => other,
    }
}

fn rows(response: Response) -> (Vec<String>, Vec<Vec<Cell>>) {
    match response {
        Response::Rows(rs) => (rs.columns, rs.rows),
        other => panic!("expected rows, got {other:?}"),
    }
}

fn ids(response: Response) -> Vec<i64> {
    rows(response)
        .1
        .into_iter()
        .map(|row| match row[0] {
            Cell::Int(v) => v,
            ref other => panic!("expected an int, got {other:?}"),
        })
        .collect()
}

fn error_code(response: Response) -> ErrorCode {
    match response {
        Response::Err(err) => err.code,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn seed(session: &mut Session) {
    run_ok(
        session,
        "CREATE TABLE items (id BIGINT PRIMARY KEY, name VARCHAR(16), emb VECTOR(2))",
    );
    run_ok(
        session,
        "INSERT INTO items VALUES (1, 'a', '[0,0]'), (2, 'b', [0,1]), (3, 'c', [3,4]), (4, 'd', [3,4])",
    );
}

#[test]
fn insert_select_roundtrip_with_both_vector_forms() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    let (columns, rows) = rows(run_ok(&mut s, "SELECT * FROM items WHERE id = 3"));
    assert_eq!(columns, vec!["id", "name", "emb"]);
    assert_eq!(
        rows,
        vec![vec![
            Cell::Int(3),
            Cell::Text("c".into()),
            Cell::Vector(vec![3.0, 4.0])
        ]]
    );
}

#[test]
fn distance_is_reported_in_six_decimals() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    let (columns, rows) = rows(run_ok(
        &mut s,
        "SELECT L2_DISTANCE(emb, '[0,0]') AS dist FROM items WHERE id = 3",
    ));
    assert_eq!(columns, vec!["dist"]);
    assert_eq!(rows, vec![vec![Cell::Real(5.0)]]);
    assert_eq!(rows[0][0].render(), "5.000000");
}

#[test]
fn topk_filters_before_limiting_and_breaks_ties_by_primary_key() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    // Rows 3 and 4 tie at distance 5; the primary key decides.
    let got = ids(run_ok(
        &mut s,
        "SELECT id FROM items WHERE name >= 'a' ORDER BY L2_DISTANCE(emb, '[0,0]') LIMIT 2",
    ));
    // `name >= 'a'` keeps every row; the two nearest are 1 and 2.
    assert_eq!(got, vec![1, 2]);

    let got = ids(run_ok(
        &mut s,
        "SELECT id FROM items ORDER BY L2_DISTANCE(emb, '[0,0]') DESC LIMIT 3",
    ));
    assert_eq!(got, vec![3, 4, 2], "distance desc, ties by id asc");
}

#[test]
fn topk_respects_the_filter_and_returns_fewer_rows_when_short() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    let got = ids(run_ok(
        &mut s,
        "SELECT id FROM items WHERE id >= 3 ORDER BY L2_DISTANCE(emb, '[0,0]') LIMIT 10",
    ));
    assert_eq!(got, vec![3, 4]);

    let got = ids(run_ok(
        &mut s,
        "SELECT id FROM items WHERE id > 100 ORDER BY id LIMIT 5",
    ));
    assert!(got.is_empty());
}

#[test]
fn limit_zero_and_plain_order_by() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    assert!(ids(run_ok(&mut s, "SELECT id FROM items LIMIT 0")).is_empty());
    assert_eq!(
        ids(run_ok(&mut s, "SELECT id FROM items ORDER BY id DESC")),
        vec![4, 3, 2, 1]
    );
    // Ties in a plain ORDER BY also break by primary key ascending.
    assert_eq!(
        ids(run_ok(&mut s, "SELECT id FROM items ORDER BY emb LIMIT 2")).len(),
        2
    );
}

#[test]
fn update_and_delete_report_affected_rows() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    let Response::Ok { affected } = run_ok(&mut s, "UPDATE items SET name = 'z' WHERE id >= 3")
    else {
        panic!("expected ok")
    };
    assert_eq!(affected, 2);
    let (_, rows) = rows(run_ok(&mut s, "SELECT name FROM items WHERE id = 4"));
    assert_eq!(rows, vec![vec![Cell::Text("z".into())]]);

    let Response::Ok { affected } = run_ok(&mut s, "DELETE FROM items WHERE name = 'z'") else {
        panic!("expected ok")
    };
    assert_eq!(affected, 2);
    assert_eq!(
        ids(run_ok(&mut s, "SELECT id FROM items ORDER BY id")),
        vec![1, 2]
    );
}

#[test]
fn explicit_transaction_sees_its_own_writes_and_rolls_back() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    run_ok(&mut s, "BEGIN");
    run_ok(&mut s, "DELETE FROM items WHERE id = 1");
    run_ok(&mut s, "INSERT INTO items VALUES (9, 'x', '[1,1]')");
    // Read-your-writes inside the transaction.
    assert_eq!(
        ids(run_ok(&mut s, "SELECT id FROM items ORDER BY id")),
        vec![2, 3, 4, 9]
    );
    run_ok(&mut s, "ROLLBACK");
    assert_eq!(
        ids(run_ok(&mut s, "SELECT id FROM items ORDER BY id")),
        vec![1, 2, 3, 4]
    );

    run_ok(&mut s, "BEGIN");
    run_ok(&mut s, "INSERT INTO items VALUES (9, 'x', '[1,1]')");
    run_ok(&mut s, "COMMIT");
    drop(s);

    // Reopen: the committed row survives, the rolled-back one never existed.
    let mut s = session(&dir);
    assert_eq!(
        ids(run_ok(&mut s, "SELECT id FROM items ORDER BY id")),
        vec![1, 2, 3, 4, 9]
    );
}

#[test]
fn failing_statement_inside_a_transaction_keeps_earlier_statements() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    run_ok(&mut s, "BEGIN");
    run_ok(&mut s, "INSERT INTO items VALUES (5, 'e', '[0,0]')");
    assert_eq!(
        error_code(s.execute("INSERT INTO items VALUES (5, 'dup', '[0,0]')")),
        ErrorCode::DuplicateEntry
    );
    // Earlier statement in the transaction is still there and committable.
    assert_eq!(
        ids(run_ok(&mut s, "SELECT id FROM items WHERE id = 5")),
        vec![5]
    );
    run_ok(&mut s, "COMMIT");
    drop(s);
    let mut s = session(&dir);
    assert_eq!(
        ids(run_ok(&mut s, "SELECT id FROM items ORDER BY id")),
        vec![1, 2, 3, 4, 5]
    );
}

#[test]
fn error_codes_match_the_contract() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    assert_eq!(
        error_code(s.execute("SELECT * FROM nope")),
        ErrorCode::NoSuchTable
    );
    assert_eq!(
        error_code(s.execute("SELECT nope FROM items")),
        ErrorCode::UnknownColumn
    );
    assert_eq!(
        error_code(s.execute("INSERT INTO items VALUES (1, 'dup', '[0,0]')")),
        ErrorCode::DuplicateEntry
    );
    assert_eq!(
        error_code(s.execute("INSERT INTO items VALUES (5, 'x', '[0]')")),
        ErrorCode::WrongValue
    );
    assert_eq!(
        error_code(s.execute("INSERT INTO items VALUES (5, 'x', '[nan,0]')")),
        ErrorCode::WrongValue
    );
    assert_eq!(
        error_code(s.execute("INSERT INTO items VALUES (5, 'x', '[inf,0]')")),
        ErrorCode::WrongValue
    );
}

#[test]
fn transaction_control_errors() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);

    assert_eq!(
        error_code(s.execute("COMMIT")),
        ErrorCode::NoActiveTransaction
    );
    assert_eq!(
        error_code(s.execute("ROLLBACK")),
        ErrorCode::NoActiveTransaction
    );
    run_ok(&mut s, "BEGIN");
    assert_eq!(error_code(s.execute("BEGIN")), ErrorCode::NestedTransaction);
    run_ok(&mut s, "ROLLBACK");
    // The transaction is gone: a second ROLLBACK has nothing to roll back.
    assert_eq!(
        error_code(s.execute("ROLLBACK")),
        ErrorCode::NoActiveTransaction
    );
}

#[test]
fn primary_key_cannot_be_updated() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    seed(&mut s);
    assert_eq!(
        error_code(s.execute("UPDATE items SET id = 10 WHERE id = 1")),
        ErrorCode::NotSupported
    );
}

#[test]
fn varchar_length_is_counted_in_characters() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    run_ok(
        &mut s,
        "CREATE TABLE t (id BIGINT PRIMARY KEY, s VARCHAR(3))",
    );
    run_ok(&mut s, "INSERT INTO t VALUES (1, '中文三')");
    assert_eq!(
        error_code(s.execute("INSERT INTO t VALUES (2, '中文四字')")),
        ErrorCode::DataTooLong
    );
}

#[test]
fn ddl_commits_the_open_transaction_first() {
    let dir = TempDir::new();
    let mut s = session(&dir);
    run_ok(&mut s, "CREATE TABLE t (id BIGINT PRIMARY KEY)");
    run_ok(&mut s, "BEGIN");
    run_ok(&mut s, "INSERT INTO t VALUES (1)");
    // MySQL commits the open transaction before running DDL, so the row above
    // survives, and the transaction is no longer open afterwards.
    run_ok(&mut s, "CREATE TABLE u (id BIGINT PRIMARY KEY)");
    assert_eq!(
        error_code(s.execute("ROLLBACK")),
        ErrorCode::NoActiveTransaction
    );
    let (_, data) = rows(run_ok(&mut s, "SELECT id FROM t"));
    assert_eq!(data, vec![vec![Cell::Int(1)]]);
}

#[test]
fn topk_streams_a_large_table_and_matches_the_brute_force_order() {
    const ROWS: i64 = 20_000;
    const K: i64 = 10;

    let dir = TempDir::new();
    let mut s = session(&dir);
    run_ok(
        &mut s,
        "CREATE TABLE big (id BIGINT PRIMARY KEY, name VARCHAR(16), emb VECTOR(2))",
    );

    // Spread the rows over enough pages that the scan has to stream through
    // several of them, and give a few rows identical vectors to exercise the
    // primary-key tie-break.
    let mut inserted = 0;
    while inserted < ROWS {
        let batch: Vec<String> = (0..500)
            .map(|i| {
                let pk = inserted + i + 1;
                let x = pk % 97;
                let y = if pk % 50 == 0 { 0 } else { pk % 13 };
                format!("({pk}, 'r{pk}', [{x},{y}])")
            })
            .collect();
        run_ok(
            &mut s,
            &format!("INSERT INTO big VALUES {}", batch.join(", ")),
        );
        inserted += 500;
    }

    let query = "[3,4]";
    let got = ids(run_ok(
        &mut s,
        &format!("SELECT id FROM big ORDER BY L2_DISTANCE(emb, '{query}') LIMIT {K}"),
    ));

    let mut expected: Vec<(f64, i64)> = (1..=ROWS)
        .map(|pk| {
            let x = pk % 97;
            let y = if pk % 50 == 0 { 0 } else { pk % 13 };
            let dist = ((x as f64 - 3.0).powi(2) + (y as f64 - 4.0).powi(2)).sqrt();
            (dist, pk)
        })
        .collect();
    expected.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let expected: Vec<i64> = expected.into_iter().take(K as usize).map(|(_, pk)| pk).collect();

    assert_eq!(got, expected);
}
