//! Response blocks written to stdout.
//!
//! One statement produces exactly one block, and every block ends with a line
//! feed so the driver can read it line by line:
//!
//! ```text
//! +----+-------+
//! | id | name  |
//! +----+-------+
//! |  1 | alice |
//! +----+-------+
//! 1 row in set
//! ```
//!
//! Numeric cells are right aligned, everything else is left aligned, and every
//! cell is padded with one space on each side, matching `mysql`'s default
//! table style. Column width is measured in Unicode scalar values.

use std::io::{self, Write};

use crate::error::DbError;

#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Int(i64),
    Real(f64),
    Text(String),
    Vector(Vec<f32>),
}

impl Cell {
    /// Textual form used in result tables.
    pub fn render(&self) -> String {
        match self {
            Cell::Int(v) => v.to_string(),
            // Distances: a fixed number of decimals keeps the comparison with
            // the reference values stable within the documented tolerance.
            Cell::Real(v) => format!("{v:.6}"),
            Cell::Text(v) => v.clone(),
            // Shortest representation that round-trips back to the same f32.
            Cell::Vector(v) => {
                let mut out = String::with_capacity(2 + v.len() * 12);
                out.push('[');
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&x.to_string());
                }
                out.push(']');
                out
            }
        }
    }

    fn is_numeric(&self) -> bool {
        matches!(self, Cell::Int(_) | Cell::Real(_))
    }
}

#[derive(Clone, Debug, Default)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Clone, Debug)]
pub enum Response {
    /// DDL, DML and transaction control.
    Ok {
        affected: u64,
    },
    Rows(ResultSet),
    Err(DbError),
    Bye,
}

impl Response {
    pub fn ok(affected: u64) -> Self {
        Response::Ok { affected }
    }

    pub fn write(&self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Response::Ok { affected } => writeln!(
                out,
                "Query OK, {} {} affected",
                affected,
                if *affected == 1 { "row" } else { "rows" }
            ),
            Response::Rows(rs) => write_result_set(out, rs),
            Response::Err(err) => writeln!(out, "{err}"),
            Response::Bye => writeln!(out, "BYE"),
        }
    }
}

fn write_result_set(out: &mut impl Write, rs: &ResultSet) -> io::Result<()> {
    let ncols = rs.columns.len();
    let mut widths: Vec<usize> = rs.columns.iter().map(|c| c.chars().count()).collect();

    // Cells are rendered once: each one is needed for the width pass and again
    // for the output pass, and result sets can be large.
    let mut rendered: Vec<(Vec<String>, Vec<bool>)> = Vec::with_capacity(rs.rows.len());
    for row in &rs.rows {
        let mut texts = Vec::with_capacity(ncols);
        let mut numeric = Vec::with_capacity(ncols);
        for (i, width) in widths.iter_mut().enumerate() {
            match row.get(i) {
                Some(cell) => {
                    let text = cell.render();
                    *width = (*width).max(text.chars().count());
                    texts.push(text);
                    numeric.push(cell.is_numeric());
                }
                None => {
                    texts.push(String::new());
                    numeric.push(false);
                }
            }
        }
        rendered.push((texts, numeric));
    }

    let mut border = String::with_capacity(4 * ncols + 1);
    for w in &widths {
        border.push('+');
        for _ in 0..w + 2 {
            border.push('-');
        }
    }
    border.push('+');
    border.push('\n');

    out.write_all(border.as_bytes())?;
    write_row(out, &rs.columns, &widths, &vec![false; ncols])?;
    out.write_all(border.as_bytes())?;
    for (texts, numeric) in &rendered {
        write_row(out, texts, &widths, numeric)?;
    }
    out.write_all(border.as_bytes())?;
    writeln!(
        out,
        "{} {} in set",
        rs.rows.len(),
        if rs.rows.len() == 1 { "row" } else { "rows" }
    )
}

fn write_row(
    out: &mut impl Write,
    cells: &[String],
    widths: &[usize],
    numeric: &[bool],
) -> io::Result<()> {
    out.write_all(b"|")?;
    for (i, cell) in cells.iter().enumerate() {
        let width = widths[i];
        let len = cell.chars().count();
        let pad = width.saturating_sub(len);
        out.write_all(b" ")?;
        if numeric.get(i).copied().unwrap_or(false) {
            for _ in 0..pad {
                out.write_all(b" ")?;
            }
            out.write_all(cell.as_bytes())?;
        } else {
            out.write_all(cell.as_bytes())?;
            for _ in 0..pad {
                out.write_all(b" ")?;
            }
        }
        out.write_all(b" |")?;
    }
    out.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(response: &Response) -> String {
        let mut buf = Vec::new();
        response.write(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn renders_table_with_alignment() {
        let rs = ResultSet {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec![Cell::Int(1), Cell::Text("alice".into())],
                vec![Cell::Int(20), Cell::Text("bo".into())],
            ],
        };
        assert_eq!(
            render(&Response::Rows(rs)),
            "+----+-------+\n\
             | id | name  |\n\
             +----+-------+\n\
             |  1 | alice |\n\
             | 20 | bo    |\n\
             +----+-------+\n\
             2 rows in set\n"
        );
    }

    #[test]
    fn renders_empty_result_set() {
        let rs = ResultSet {
            columns: vec!["id".into()],
            rows: vec![],
        };
        assert_eq!(
            render(&Response::Rows(rs)),
            "+----+\n| id |\n+----+\n+----+\n0 rows in set\n"
        );
    }

    #[test]
    fn renders_single_row_count() {
        let rs = ResultSet {
            columns: vec!["v".into()],
            rows: vec![vec![Cell::Real(1.5)]],
        };
        assert_eq!(
            render(&Response::Rows(rs)),
            "+----------+\n| v        |\n+----------+\n| 1.500000 |\n+----------+\n1 row in set\n"
        );
    }

    #[test]
    fn renders_vector_and_affected_rows() {
        assert_eq!(render(&Response::ok(0)), "Query OK, 0 rows affected\n");
        assert_eq!(render(&Response::ok(1)), "Query OK, 1 row affected\n");
        assert_eq!(
            Cell::Vector(vec![1.0, 1.5, -2.25]).render(),
            "[1,1.5,-2.25]"
        );
    }

    #[test]
    fn renders_errors() {
        let err = DbError::no_such_table("t");
        assert_eq!(
            render(&Response::Err(err)),
            "ERROR 1146: Table 't' doesn't exist\n"
        );
    }
}
