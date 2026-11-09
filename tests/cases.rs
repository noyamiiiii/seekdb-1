//! Runs the `.sql` case files under `tests/cases` against the real server and
//! compares the whole transcript with the block after `-- expect` in the file.
//!
//! The format is deliberately close to mysqltest, which the grading harness
//! uses: one statement per line (a trailing `;` closes it), and the expected
//! output is the text the server writes back, line for line.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mvdb-cases-{}-{id}", std::process::id()));
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
        let read = self.stdout.read_line(&mut line).unwrap();
        assert!(read > 0, "the server closed its output");
        line.trim_end_matches(['\r', '\n']).to_string()
    }

    /// Sends one statement and collects its whole response.
    fn run(&mut self, statement: &str) -> Vec<String> {
        writeln!(self.stdin, "{statement}").unwrap();
        self.stdin.flush().unwrap();
        let mut lines = Vec::new();
        loop {
            let line = self.line();
            let done = line.starts_with("ERROR ")
                || line.starts_with("Query OK")
                || line.ends_with("in set");
            lines.push(line);
            if done {
                return lines;
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "exit;");
        let _ = self.child.wait();
    }
}

struct Case {
    name: String,
    statements: Vec<String>,
    expected: Vec<String>,
}

fn parse_case(path: &Path) -> Case {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    // Editors (and PowerShell) like to prepend a BOM; it is not part of the
    // first line's content.
    let text = text.trim_start_matches('\u{feff}');
    let mut name = String::new();
    let mut statements = Vec::new();
    let mut expected = Vec::new();
    let mut in_expected = false;
    let mut pending = String::new();
    for line in text.lines() {
        let trimmed = line.trim_end();
        if !in_expected && trimmed.starts_with("-- name:") {
            name = trimmed["-- name:".len()..].trim().to_string();
            continue;
        }
        if trimmed == "-- expect" {
            in_expected = true;
            continue;
        }
        if in_expected {
            if !trimmed.is_empty() {
                expected.push(trimmed.to_string());
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }
        pending.push_str(trimmed);
        if trimmed.ends_with(';') {
            statements.push(std::mem::take(&mut pending));
        } else {
            pending.push(' ');
        }
    }
    assert!(pending.is_empty(), "{}: unterminated statement", path.display());
    assert!(!statements.is_empty(), "{}: no statements", path.display());
    assert!(!expected.is_empty(), "{}: no expected output", path.display());
    Case {
        name,
        statements,
        expected,
    }
}

#[test]
fn case_files_match_the_server_transcript() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "{}: no case files", dir.display());

    for path in paths {
        let case = parse_case(&path);
        let tmp = TempDir::new();
        let mut server = Server::spawn(&tmp.0);
        let mut actual = Vec::new();
        for statement in &case.statements {
            actual.extend(server.run(statement));
        }
        assert_eq!(
            actual,
            case.expected,
            "case '{}' in {}",
            case.name,
            path.display()
        );
    }
}