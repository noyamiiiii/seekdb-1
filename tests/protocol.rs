//! End-to-end protocol tests: they spawn the real binary and talk to it over
//! pipes, exactly like the evaluation driver does.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("minivectordb-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp data dir");
    dir
}

/// Starts the server with `input` on stdin and returns `(stdout, exit code)`.
fn run(name: &str, input: &str) -> (String, i32) {
    let dir = data_dir(name);
    let mut child: Child = Command::new(env!("CARGO_BIN_EXE_minivectordb"))
        .arg("--data")
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn server");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write statements");
    let out = child.wait_with_output().expect("wait for server");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let code = out.status.code().unwrap_or(-1);
    (stdout, code)
}

#[test]
fn prints_ready_before_anything_else() {
    let (out, code) = run("ready", "exit;\n");
    assert_eq!(code, 0);
    assert_eq!(out, "READY\nBYE\n");
}

#[test]
fn one_block_per_statement() {
    let (out, code) = run("blocks", "SELECT * FROM t;\nSELECT * FROM u;\n");
    assert_eq!(code, 0);
    let blocks: Vec<&str> = out.lines().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(blocks.len(), 2, "stdout was:\n{out}");
}

#[test]
fn exit_stops_reading_the_stream() {
    let (out, _) = run("exit", "SELECT * FROM t;\nexit;\nSELECT * FROM u;\n");
    assert!(out.ends_with("BYE\n"), "stdout was:\n{out}");
    assert_eq!(out.matches("ERROR").count(), 1);
}

#[test]
fn multi_line_and_commented_statement_is_one_block() {
    let (out, _) = run(
        "comments",
        "-- leading comment\nSELECT 1 /* inline ; */\n  FROM t\n  WHERE id = 1;\n",
    );
    assert_eq!(out.matches("ERROR").count(), 1, "stdout was:\n{out}");
}

#[test]
fn eof_without_exit_ends_with_success() {
    let (out, code) = run("eof", "SELECT * FROM t");
    assert_eq!(code, 0);
    assert!(out.starts_with("READY\n"));
    assert_eq!(out.matches("ERROR").count(), 1);
}

#[test]
fn a_constant_select_is_one_row() {
    let (out, code) = run("const", "SELECT 1 AS one;\n");
    assert_eq!(code, 0);
    assert!(out.contains("1 row in set"), "stdout was:\n{out}");
    assert!(out.contains("| one |"), "stdout was:\n{out}");
}

#[test]
fn unknown_options_are_ignored() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_minivectordb"))
        .args(["--nonsense", "--mode=ann", "--data"])
        .arg(data_dir("unknown-options"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn server");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"exit;\n")
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(stdout.ends_with("BYE\n"), "stdout was:\n{stdout}");
}
