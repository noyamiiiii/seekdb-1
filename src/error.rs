//! Error codes and the `ERROR <code>: <message>` response form.
//!
//! The code numbers follow MySQL's public error numbering where an equivalent
//! condition exists, so that the protocol feels like a MySQL server to the test
//! driver. The three transaction codes at the end are this project's own
//! choices; they are documented in `docs/protocol.md`.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    /// Generic failure without a more specific code.
    Unknown = 1105,
    /// `CREATE TABLE` for a name that already exists.
    TableExists = 1050,
    /// Column name that is not part of the referenced table.
    UnknownColumn = 1054,
    /// Column name that matches more than one source.
    AmbiguousColumn = 1052,
    /// Duplicate column name in `CREATE TABLE`.
    DuplicateColumn = 1060,
    /// Duplicate value in the primary key.
    DuplicateEntry = 1062,
    /// Lexical or grammatical error in a statement.
    Syntax = 1064,
    /// Row value count does not match the target column count.
    WrongValueCount = 1136,
    /// Reference to a table that does not exist.
    NoSuchTable = 1146,
    /// Statement is valid SQL but not part of the supported subset.
    NotSupported = 1235,
    /// Numeric value outside the range of the target type.
    OutOfRange = 1264,
    /// String value longer than the declared character length.
    DataTooLong = 1406,
    /// Value cannot be converted to the declared column type.
    WrongValue = 1366,
    /// `BEGIN` while a transaction is already open.
    NestedTransaction = 1567,
    /// `COMMIT`/`ROLLBACK` while no transaction is open.
    NoActiveTransaction = 1568,
}

impl ErrorCode {
    pub fn as_u16(self) -> u16 {
        self as u16
    }
}

/// A database error with its protocol code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbError {
    pub code: ErrorCode,
    pub message: String,
}

impl DbError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let message = message.into();
        // Response blocks are line oriented: never smuggle a newline in.
        let message = message.replace(['\n', '\r'], " ");
        Self { code, message }
    }

    pub fn syntax(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Syntax, message)
    }

    pub fn unsupported(feature: impl fmt::Display) -> Self {
        Self::new(
            ErrorCode::NotSupported,
            format!("{feature} is not supported in this build"),
        )
    }

    pub fn no_such_table(name: &str) -> Self {
        Self::new(
            ErrorCode::NoSuchTable,
            format!("Table '{name}' doesn't exist"),
        )
    }

    pub fn unknown_column(name: &str) -> Self {
        Self::new(ErrorCode::UnknownColumn, format!("Unknown column '{name}'"))
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ERROR {}: {}", self.code.as_u16(), self.message)
    }
}

impl std::error::Error for DbError {}

pub type DbResult<T> = Result<T, DbError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_code_and_message() {
        let e = DbError::syntax("You have an error in your SQL syntax");
        assert_eq!(
            e.to_string(),
            "ERROR 1064: You have an error in your SQL syntax"
        );
    }

    #[test]
    fn message_newlines_are_flattened() {
        let e = DbError::new(ErrorCode::Unknown, "a\nb");
        assert_eq!(e.message, "a b");
    }
}
