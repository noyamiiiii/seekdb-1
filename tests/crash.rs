//! Crash test: kill the server with SIGKILL in the middle of a load, restart
//! it, and check that every acknowledged statement survived while no statement
//! survived partially.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mvdb-crash-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn(dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_minivectordb"))
            .arg("--data")
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("cannot start the server");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Self {
            child,
            stdin,
            stdout,
        };
        assert_eq!(server.line(), "READY");
        server
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("server closed stdout");
        line.trim_end().to_string()
    }

    /// Sends one statement and consumes its response; returns true on success.
    fn run(&mut self, sql: &str) -> bool {
        writeln!(self.stdin, "{sql};").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let line = self.line();
            if line.starts_with("Query OK") {
                return true;
            }
            if line.starts_with("ERROR") {
                return false;
            }
            assert!(!line.is_empty(), "unexpected end of output for {sql}");
        }
    }

    /// Sends a statement without reading its response.
    fn send(&mut self, sql: &str) {
        writeln!(self.stdin, "{sql};").unwrap();
        self.stdin.flush().unwrap();
    }

    fn kill(mut self) {
        // SIGKILL: no shutdown hook, no buffered flush, exactly like a crash.
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn shutdown(mut self) {
        let _ = writeln!(self.stdin, "exit;");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }

    /// Runs `SELECT id FROM ...` and collects the ids the server reports.
    fn select_ids(&mut self, sql: &str) -> Vec<i64> {
        writeln!(self.stdin, "{sql};").unwrap();
        self.stdin.flush().unwrap();
        let mut ids = Vec::new();
        let mut header_seen = false;
        loop {
            let line = self.line();
            if line.contains("rows in set") {
                return ids;
            }
            if line.starts_with('+') {
                continue;
            }
            if line.starts_with('|') {
                if !header_seen {
                    header_seen = true;
                    continue;
                }
                let cell = line.trim_matches('|').trim();
                if let Ok(id) = cell.parse::<i64>() {
                    ids.push(id);
                }
                continue;
            }
            assert!(!line.is_empty(), "unexpected end of output");
        }
    }
}

fn create_table(server: &mut Server) {
    assert!(server.run(
        "CREATE TABLE t (id BIGINT PRIMARY KEY, name VARCHAR(16), emb VECTOR(2))"
    ));
}

#[test]
fn committed_rows_survive_sigkill_and_no_statement_is_partial() {
    let dir = TempDir::new();
    let mut acked = 0u32;

    {
        let mut server = Server::spawn(&dir.0);
        create_table(&mut server);
        // 40 statements of 5 rows each, acknowledged one by one.
        for batch in 0..40u32 {
            let values = (0..5)
                .map(|i| {
                    let pk = batch * 5 + i + 1;
                    format!("({pk}, 'r{pk}', '[{pk},0]')")
                })
                .collect::<Vec<_>>()
                .join(", ");
            assert!(server.run(&format!("INSERT INTO t VALUES {values}")));
            acked += 5;
        }
        // Now flood a few more statements without waiting for acknowledgements
        // and kill the process mid-flight.
        for batch in 40..60u32 {
            let values = (0..5)
                .map(|i| {
                    let pk = batch * 5 + i + 1;
                    format!("({pk}, 'r{pk}', '[{pk},0]')")
                })
                .collect::<Vec<_>>()
                .join(", ");
            server.send(&format!("INSERT INTO t VALUES {values}"));
        }
        server.kill();
    }

    let mut server = Server::spawn(&dir.0);
    let ids = server.select_ids("SELECT id FROM t");
    server.shutdown();

    assert!(
        ids.len() >= acked as usize,
        "{acked} rows were acknowledged but only {} survived",
        ids.len()
    );
    assert_eq!(
        ids.len() % 5,
        0,
        "a five-row statement was applied partially"
    );
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "duplicate primary keys after recovery");
    assert_eq!(sorted.first(), Some(&1));
}

#[test]
fn repeated_crashes_do_not_corrupt_the_database() {
    let dir = TempDir::new();
    let mut acked: Vec<i64> = Vec::new();
    let mut in_flight: Vec<i64> = Vec::new();
    for round in 0..5i64 {
        let mut server = Server::spawn(&dir.0);
        if round == 0 {
            create_table(&mut server);
        }
        for step in 1..=6i64 {
            let pk = round * 100 + step;
            assert!(server.run(&format!("INSERT INTO t VALUES ({pk}, 'r', '[{pk},0]')")));
            acked.push(pk);
        }
        // A statement that is still in flight when the process dies.
        let pk = 9000 + round;
        server.send(&format!("INSERT INTO t VALUES ({pk}, 'z', '[{pk},0]')"));
        in_flight.push(pk);
        server.kill();
    }

    let mut server = Server::spawn(&dir.0);
    let ids = server.select_ids("SELECT id FROM t");
    server.shutdown();

    let found: std::collections::HashSet<i64> = ids.iter().copied().collect();
    assert_eq!(found.len(), ids.len(), "duplicate primary keys after recovery");
    for pk in &acked {
        assert!(found.contains(pk), "acknowledged row {pk} was lost");
    }
    // In-flight rows may or may not have made it, but nothing else may appear.
    let unexplained = found.len() - acked.len();
    assert!(
        unexplained <= in_flight.len(),
        "found {} rows that were never acknowledged",
        unexplained
    );
}