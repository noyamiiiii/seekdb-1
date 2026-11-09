//! stdio protocol front end.
//!
//! The evaluation driver talks to the server over stdin/stdout only:
//!
//! * the server prints a single ready line (`READY`) after the data directory
//!   has been recovered and the vector indexes have been rebuilt;
//! * input is a stream of SQL statements separated by `;` (a semicolon inside a
//!   string literal or a comment never terminates a statement);
//! * every statement produces exactly one response block on stdout;
//! * diagnostics go to stderr.
//!
//! Comments are stripped while scanning: `-- ` to end of line, `#` to end of
//! line, and `/* ... */` block comments (no nesting, as in MySQL).

use std::io::{self, BufRead};

/// Aggregated, statement-at-a-time reader over stdin.
pub struct StatementReader<R: BufRead> {
    inner: R,
    /// Pieces of the statement being accumulated (comments already dropped).
    buf: String,
    /// Current physical line, kept until fully consumed: a line may hold the
    /// tail of one statement plus the head of the next one.
    line: String,
    /// Scan position inside `line`.
    pos: usize,
    /// True while inside a single-quoted literal.
    in_str: bool,
    /// True while inside a `/* ... */` block comment.
    in_block: bool,
}

impl<R: BufRead> StatementReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: String::new(),
            line: String::new(),
            pos: 0,
            in_str: false,
            in_block: false,
        }
    }

    /// Next statement text (whitespace trimmed, trailing `;` removed), or
    /// `None` at end of input.
    pub fn next_statement(&mut self) -> io::Result<Option<String>> {
        loop {
            if self.pos < self.line.len() {
                if let Some(stmt) = self.scan_line() {
                    return Ok(Some(stmt));
                }
                continue;
            }
            self.line.clear();
            self.pos = 0;
            if self.inner.read_line(&mut self.line)? == 0 {
                let mut pending = std::mem::take(&mut self.buf);
                if self.in_block {
                    // Preserve a lexical error instead of executing the SQL prefix.
                    pending.push_str(" /*");
                }
                let trimmed = pending.trim().trim_start_matches('\u{feff}');
                if trimmed.is_empty() {
                    return Ok(None);
                }
                self.in_str = false;
                self.in_block = false;
                return Ok(Some(trimmed.to_string()));
            }
        }
    }

    fn scan_line(&mut self) -> Option<String> {
        let bytes = self.line.as_bytes();
        let mut i = self.pos;
        let mut piece_start = i;
        while i < bytes.len() {
            let b = bytes[i];
            if self.in_block {
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    self.in_block = false;
                    i += 2;
                    piece_start = i;
                } else {
                    i += 1;
                }
                continue;
            }
            if self.in_str {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    self.in_str = false;
                }
                i += 1;
                continue;
            }
            match b {
                b'\'' => {
                    self.in_str = true;
                    i += 1;
                }
                b'-' if bytes.get(i + 1) == Some(&b'-') => {
                    self.buf.push_str(&self.line[piece_start..i]);
                    // A removed comment is whitespace, never token concatenation.
                    self.buf.push('\n');
                    self.pos = bytes.len();
                    return self.take_line_command();
                }
                b'#' => {
                    self.buf.push_str(&self.line[piece_start..i]);
                    // A removed comment is whitespace, never token concatenation.
                    self.buf.push('\n');
                    self.pos = bytes.len();
                    return self.take_line_command();
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    self.buf.push_str(&self.line[piece_start..i]);
                    self.buf.push(' ');
                    self.in_block = true;
                    i += 2;
                    piece_start = i;
                }
                b';' => {
                    self.buf.push_str(&self.line[piece_start..i]);
                    let stmt = std::mem::take(&mut self.buf);
                    self.pos = i + 1;
                    let trimmed = stmt.trim().trim_start_matches('\u{feff}');
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                    // Keep an empty-statement run iterative to avoid stack growth.
                    i += 1;
                    piece_start = i;
                    continue;
                }
                _ => i += 1,
            }
        }
        self.buf.push_str(&self.line[piece_start..]);
        self.pos = bytes.len();
        self.take_line_command()
    }

    fn take_line_command(&mut self) -> Option<String> {
        if self.in_str || self.in_block {
            return None;
        }
        let command = self.buf.trim().trim_start_matches('\u{feff}').trim();
        if command.eq_ignore_ascii_case("exit")
            || command.eq_ignore_ascii_case("quit")
            || command == "\\q"
        {
            let command = command.to_string();
            self.buf.clear();
            Some(command)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn split(input: &str) -> Vec<String> {
        let mut r = StatementReader::new(Cursor::new(input.as_bytes().to_vec()));
        let mut out = Vec::new();
        while let Some(s) = r.next_statement().unwrap() {
            out.push(s);
        }
        out
    }

    #[test]
    fn splits_simple_statements() {
        assert_eq!(
            split("SELECT 1; SELECT 2;\n"),
            vec!["SELECT 1".to_string(), "SELECT 2".to_string()]
        );
    }

    #[test]
    fn keeps_semicolons_inside_literals() {
        assert_eq!(
            split("INSERT INTO t VALUES (1, 'a;b');"),
            vec!["INSERT INTO t VALUES (1, 'a;b')".to_string()]
        );
    }

    #[test]
    fn handles_line_and_block_comments() {
        assert_eq!(
            split("-- leading\nSELECT 1; /* mid ; */ SELECT 2; # tail\n"),
            vec!["SELECT 1".to_string(), "SELECT 2".to_string()]
        );
    }

    #[test]
    fn spans_multiple_lines() {
        assert_eq!(
            split("SELECT *\n  FROM t\n  WHERE id = 1;\n"),
            vec!["SELECT *\n  FROM t\n  WHERE id = 1".to_string()]
        );
    }

    #[test]
    fn eof_flushes_unterminated_statement() {
        assert_eq!(split("SELECT 1"), vec!["SELECT 1".to_string()]);
    }

    #[test]
    fn skips_empty_statements() {
        assert_eq!(split(";;\nSELECT 1;;\n"), vec!["SELECT 1".to_string()]);
    }

    #[test]
    fn strips_a_leading_byte_order_mark() {
        assert_eq!(split("\u{feff}SELECT 1;\n"), vec!["SELECT 1".to_string()]);
    }

    #[test]
    fn keeps_vector_literal_intact() {
        assert_eq!(
            split("CREATE TABLE t (id BIGINT PRIMARY KEY, v VECTOR(3));"),
            vec!["CREATE TABLE t (id BIGINT PRIMARY KEY, v VECTOR(3))".to_string()]
        );
    }
}

#[cfg(test)]
mod regression_tests {
    use super::StatementReader;
    use std::io::Cursor;

    #[test]
    fn removed_comments_preserve_token_boundaries() {
        let mut r = StatementReader::new(Cursor::new("CREATE/* a */TABLE t(id BIGINT PRIMARY KEY); SELECT id-- tail\nFROM t;"));
        assert_eq!(r.next_statement().unwrap().unwrap(), "CREATE TABLE t(id BIGINT PRIMARY KEY)");
        assert_eq!(r.next_statement().unwrap().unwrap(), "SELECT id\nFROM t");
    }

    #[test]
    fn long_empty_statement_runs_do_not_recurse() {
        let input = format!("{}SELECT 1;", ";".repeat(100_000));
        let mut r = StatementReader::new(Cursor::new(input));
        assert_eq!(r.next_statement().unwrap().as_deref(), Some("SELECT 1"));
        assert!(r.next_statement().unwrap().is_none());
    }
}
