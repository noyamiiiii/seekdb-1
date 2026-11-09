//! Table schemas and value validation.
//!
//! Validation happens once per written row, before anything reaches the storage
//! layer, so a rejected statement never leaves partial state behind.

use crate::ast::{ColumnDef, ColumnType, CreateTable, Value};
use crate::error::{DbError, DbResult, ErrorCode};

/// A table definition as stored in the catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    /// Column index of the single `BIGINT` primary key.
    pub primary_key: usize,
    /// Column index of the single vector column, if any.
    pub vector_column: Option<usize>,
}

impl TableSchema {
    /// Builds and validates a schema from `CREATE TABLE`.
    ///
    /// The subset allows exactly one `BIGINT` primary key and at most one vector
    /// column per table.
    pub fn from_create(create: &CreateTable) -> DbResult<Self> {
        for (i, column) in create.columns.iter().enumerate() {
            if create.columns[..i]
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(&column.name))
            {
                return Err(DbError::new(
                    ErrorCode::DuplicateColumn,
                    format!("duplicate column name '{}'", column.name),
                ));
            }
        }

        let primary_name = create.primary_key.as_deref().ok_or_else(|| {
            DbError::new(
                ErrorCode::NotSupported,
                "every table needs a BIGINT PRIMARY KEY column",
            )
        })?;
        let primary_key = find_column(&create.columns, primary_name)?;
        if create.columns[primary_key].ty != ColumnType::BigInt {
            return Err(DbError::new(
                ErrorCode::NotSupported,
                "the primary key must be BIGINT",
            ));
        }

        let vector_columns: Vec<usize> = create
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c.ty, ColumnType::Vector(_)))
            .map(|(i, _)| i)
            .collect();
        if vector_columns.len() > 1 {
            return Err(DbError::new(
                ErrorCode::NotSupported,
                "a table may have at most one VECTOR column",
            ));
        }

        Ok(Self {
            name: create.name.clone(),
            columns: create.columns.clone(),
            primary_key,
            vector_column: vector_columns.first().copied(),
        })
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn column(&self, name: &str) -> DbResult<(usize, &ColumnDef)> {
        let idx = self
            .column_index(name)
            .ok_or_else(|| DbError::unknown_column(name))?;
        Ok((idx, &self.columns[idx]))
    }

    /// Validates and normalises `value` for `column`, returning the value that
    /// will be stored. Numbers are coerced to text for `VARCHAR`, as MySQL does.
    pub fn coerce(&self, column: usize, value: &Value) -> DbResult<Value> {
        let def = &self.columns[column];
        match (def.ty, value) {
            (ColumnType::BigInt, Value::Int(v)) => Ok(Value::Int(*v)),
            (ColumnType::Varchar(max), Value::Text(s)) => {
                check_varchar_length(&def.name, max, s)?;
                Ok(Value::Text(s.clone()))
            }
            (ColumnType::Varchar(max), Value::Int(v)) => {
                let s = v.to_string();
                check_varchar_length(&def.name, max, &s)?;
                Ok(Value::Text(s))
            }
            (ColumnType::Vector(dim), Value::Vector(v)) => {
                check_vector(&def.name, dim, v)?;
                Ok(Value::Vector(v.clone()))
            }
            // `INSERT ... VALUES (1, '[1,2,3]')`: the bracketed form is also
            // accepted as a string constant, which the lexer hands over as text.
            (ColumnType::Vector(dim), Value::Text(s)) => {
                let parsed = crate::parser::parse_vector_string(s).map_err(|_| {
                    DbError::new(
                        ErrorCode::WrongValue,
                        format!(
                            "'{}' is not a valid vector literal for column '{}'",
                            s, def.name
                        ),
                    )
                })?;
                check_vector(&def.name, dim, &parsed)?;
                Ok(Value::Vector(parsed))
            }
            _ => Err(DbError::new(
                ErrorCode::WrongValue,
                format!(
                    "cannot store {} in column '{}'",
                    value_type_name(value),
                    def.name
                ),
            )),
        }
    }

    /// Validates a complete row in column order.
    pub fn coerce_row(&self, values: &[Value]) -> DbResult<Vec<Value>> {
        if values.len() != self.columns.len() {
            return Err(DbError::new(
                ErrorCode::WrongValueCount,
                format!(
                    "row has {} values but table '{}' has {} columns",
                    values.len(),
                    self.name,
                    self.columns.len()
                ),
            ));
        }
        values
            .iter()
            .enumerate()
            .map(|(i, v)| self.coerce(i, v))
            .collect()
    }

    pub fn primary_key_value(&self, row: &[Value]) -> i64 {
        match row.get(self.primary_key) {
            Some(Value::Int(v)) => *v,
            // `coerce_row` guarantees the type; zero is unreachable in practice.
            _ => 0,
        }
    }
}

fn find_column(columns: &[ColumnDef], name: &str) -> DbResult<usize> {
    columns
        .iter()
        .position(|c| c.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| DbError::unknown_column(name))
}

fn check_varchar_length(name: &str, max: usize, value: &str) -> DbResult<()> {
    let len = value.chars().count();
    if len > max {
        return Err(DbError::new(
            ErrorCode::DataTooLong,
            format!("data too long for column '{name}': {len} characters, limit {max}"),
        ));
    }
    Ok(())
}

fn check_vector(name: &str, dim: usize, value: &[f32]) -> DbResult<()> {
    if value.len() != dim {
        return Err(DbError::new(
            ErrorCode::WrongValue,
            format!(
                "column '{name}' expects a vector of {dim} elements, got {}",
                value.len()
            ),
        ));
    }
    if let Some(bad) = value.iter().find(|v| !v.is_finite()) {
        return Err(DbError::new(
            ErrorCode::WrongValue,
            format!("column '{name}' got a non-finite vector element ({bad})"),
        ));
    }
    Ok(())
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Int(_) => "BIGINT",
        Value::Text(_) => "VARCHAR",
        Value::Vector(_) => "VECTOR",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn schema(sql: &str) -> TableSchema {
        let crate::ast::Statement::CreateTable(ct) = parse(sql).unwrap() else {
            panic!("not a create table")
        };
        TableSchema::from_create(&ct).unwrap()
    }

    fn docs() -> TableSchema {
        schema("CREATE TABLE docs (id BIGINT PRIMARY KEY, title VARCHAR(4), emb VECTOR(3))")
    }

    #[test]
    fn finds_primary_key_and_vector_column() {
        let s = docs();
        assert_eq!(s.primary_key, 0);
        assert_eq!(s.vector_column, Some(2));
        assert_eq!(s.column_index("TITLE"), Some(1));
    }

    #[test]
    fn rejects_missing_or_wrong_primary_key() {
        let crate::ast::Statement::CreateTable(ct) =
            parse("CREATE TABLE t (id BIGINT, name VARCHAR(3))").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            TableSchema::from_create(&ct).unwrap_err().code,
            ErrorCode::NotSupported
        );

        let crate::ast::Statement::CreateTable(ct) =
            parse("CREATE TABLE t (id VARCHAR(3) PRIMARY KEY)").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            TableSchema::from_create(&ct).unwrap_err().code,
            ErrorCode::NotSupported
        );
    }

    #[test]
    fn rejects_duplicate_and_multiple_vector_columns() {
        let crate::ast::Statement::CreateTable(ct) =
            parse("CREATE TABLE t (id BIGINT PRIMARY KEY, a VECTOR(2), b VECTOR(2))").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            TableSchema::from_create(&ct).unwrap_err().code,
            ErrorCode::NotSupported
        );

        let crate::ast::Statement::CreateTable(ct) =
            parse("CREATE TABLE t (id BIGINT PRIMARY KEY, id VARCHAR(2))").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            TableSchema::from_create(&ct).unwrap_err().code,
            ErrorCode::DuplicateColumn
        );
    }

    #[test]
    fn varchar_length_counts_unicode_characters() {
        let s = docs();
        // Four CJK characters fit in VARCHAR(4) even though they are 12 bytes.
        assert_eq!(
            s.coerce(1, &Value::Text("数据库的".into())).unwrap(),
            Value::Text("数据库的".into())
        );
        let err = s.coerce(1, &Value::Text("数据库的好".into())).unwrap_err();
        assert_eq!(err.code, ErrorCode::DataTooLong);
    }

    #[test]
    fn vector_dimension_and_finite_values_are_checked() {
        let s = docs();
        let err = s.coerce(2, &Value::Vector(vec![1.0, 2.0])).unwrap_err();
        assert_eq!(err.code, ErrorCode::WrongValue);
        let err = s
            .coerce(2, &Value::Vector(vec![1.0, f32::NAN, 2.0]))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::WrongValue);
    }

    #[test]
    fn types_are_checked_per_column() {
        let s = docs();
        assert_eq!(
            s.coerce(0, &Value::Text("1".into())).unwrap_err().code,
            ErrorCode::WrongValue
        );
        assert_eq!(
            s.coerce(2, &Value::Int(1)).unwrap_err().code,
            ErrorCode::WrongValue
        );
        // Integers coerce to text in VARCHAR columns, as in MySQL.
        assert_eq!(
            s.coerce(1, &Value::Int(42)).unwrap(),
            Value::Text("42".into())
        );
    }

    #[test]
    fn row_length_must_match_schema() {
        let s = docs();
        let err = s.coerce_row(&[Value::Int(1)]).unwrap_err();
        assert_eq!(err.code, ErrorCode::WrongValueCount);
    }
}
