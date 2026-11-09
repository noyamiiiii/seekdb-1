//! Read-execute-write loop over stdin/stdout.
//!
//! [`serve`] owns everything that is protocol mechanics: the readiness line, the
//! statement framing, flushing after each block and the `BYE` handshake. What a
//! statement *means* is delegated to a [`Handler`], which keeps the loop free of
//! engine knowledge and lets tests drive it in-process.

use std::io::{self, BufRead, Write};

use crate::proto::StatementReader;
use crate::response::Response;

/// Printed once the data directory is recovered and indexes are ready.
pub const READY_LINE: &str = "READY\n";

/// Executes one SQL statement.
///
/// Implementations must not write to stdout; they return a [`Response`] and the
/// loop writes it.
pub trait Handler {
    /// Executes `statement` (already trimmed, without its terminating `;`).
    fn execute(&mut self, statement: &str) -> Response;

    /// Flushes pending state before the process exits. Called exactly once, on
    /// `exit;`/`quit;` and at end of input.
    fn shutdown(&mut self) {}
}

/// Serves statements from `input` to `handler`, writing blocks to `output`.
///
/// Returns when the input ends or after the `BYE` handshake of an `exit;`
/// statement.
pub fn serve<H, R, W>(handler: &mut H, input: R, output: &mut W) -> io::Result<()>
where
    H: Handler,
    R: BufRead,
    W: Write,
{
    output.write_all(READY_LINE.as_bytes())?;
    output.flush()?;

    let mut reader = StatementReader::new(input);
    while let Some(statement) = reader.next_statement()? {
        if is_exit(&statement) {
            Response::Bye.write(output)?;
            output.flush()?;
            handler.shutdown();
            return Ok(());
        }
        // A failing statement never ends the session: errors are blocks like
        // any other and the driver keeps going.
        handler.execute(&statement).write(output)?;
        output.flush()?;
    }

    handler.shutdown();
    Ok(())
}

/// True for the client-side exit commands (they are not SQL).
pub fn is_exit(statement: &str) -> bool {
    let statement = statement.trim();
    if statement.eq_ignore_ascii_case("\\q") {
        return true;
    }
    // `exit 3` / `quit` are accepted too: only the leading keyword matters.
    let head = statement
        .split(|c: char| c.is_whitespace() || c == ';')
        .next()
        .unwrap_or("");
    head.eq_ignore_ascii_case("exit") || head.eq_ignore_ascii_case("quit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{DbError, ErrorCode};
    use std::io::Cursor;

    struct Echo;

    impl Handler for Echo {
        fn execute(&mut self, statement: &str) -> Response {
            Response::Rows(crate::response::ResultSet {
                columns: vec!["stmt".into()],
                rows: vec![vec![crate::response::Cell::Text(statement.into())]],
            })
        }
    }

    struct Failing;

    impl Handler for Failing {
        fn execute(&mut self, _statement: &str) -> Response {
            Response::Err(DbError::new(ErrorCode::NotSupported, "nope"))
        }
    }

    fn run<H: Handler>(handler: &mut H, input: &str) -> String {
        let mut out = Vec::new();
        serve(handler, Cursor::new(input.as_bytes().to_vec()), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn starts_with_ready_and_echoes_statements() {
        let out = run(&mut Echo, "SELECT 1;\n");
        assert!(out.starts_with("READY\n"));
        assert!(out.contains("| SELECT 1 |"));
        assert!(out.ends_with("1 row in set\n"));
    }

    #[test]
    fn exit_statement_writes_bye_and_stops() {
        let out = run(&mut Echo, "SELECT 1;\nexit;\nSELECT 2;\n");
        assert!(out.ends_with("BYE\n"));
        assert!(!out.contains("SELECT 2"));
    }

    #[test]
    fn errors_do_not_end_the_session() {
        let out = run(&mut Failing, "SELECT 1;\nSELECT 2;\n");
        assert_eq!(out.matches("ERROR 1235: nope").count(), 2);
    }

    #[test]
    fn eof_without_exit_shuts_down_cleanly() {
        let out = run(&mut Echo, "SELECT 1");
        assert!(out.contains("1 row in set"));
    }

    #[test]
    fn recognises_exit_spellings() {
        for s in ["exit", "EXIT;", "quit", "\\q", "exit 1"] {
            assert!(is_exit(s), "{s} should exit");
        }
        assert!(!is_exit("SELECT 'exit'"));
    }
}
