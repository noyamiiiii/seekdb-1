//! Row encoding: turns a `Vec<Value>` into the compact bytes stored in a heap
//! page, and back. The schema decides the layout, so no per-value type tags are
//! needed:
//!
//! ```text
//! row     -> 8 bytes little-endian primary key, then the values in column order
//! BIGINT  -> 8 bytes, little-endian i64
//! VARCHAR -> 4 bytes LE length, then UTF-8 bytes
//! VECTOR  -> dim * 4 bytes, little-endian f32
//! ```
//!
//! Repeating the primary key in front of every row lets the index rebuild after
//! a restart read just eight bytes per record instead of decoding whole rows.

use crate::ast::{ColumnType, Value};
use crate::error::{DbError, DbResult, ErrorCode};
use crate::schema::TableSchema;

pub fn encode_row(schema: &TableSchema, row: &[Value]) -> DbResult<Vec<u8>> {
    if row.len() != schema.columns.len() {
        return Err(DbError::new(
            ErrorCode::WrongValueCount,
            format!(
                "row has {} values but table '{}' has {} columns",
                row.len(),
                schema.name,
                schema.columns.len()
            ),
        ));
    }
    let mut out = Vec::with_capacity(8 + row.len() * 16);
    out.extend_from_slice(&schema.primary_key_value(row).to_le_bytes());
    for (value, def) in row.iter().zip(&schema.columns) {
        match (value, def.ty) {
            (Value::Int(v), ColumnType::BigInt) => out.extend_from_slice(&v.to_le_bytes()),
            (Value::Text(s), ColumnType::Varchar(_)) => {
                let len = u32::try_from(s.len()).map_err(|_| {
                    DbError::new(ErrorCode::DataTooLong, "text value too long to encode")
                })?;
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            (Value::Vector(v), ColumnType::Vector(dim)) => {
                if v.len() != dim {
                    return Err(DbError::new(
                        ErrorCode::WrongValue,
                        format!(
                            "column '{}' expects {dim} elements, got {}",
                            def.name,
                            v.len()
                        ),
                    ));
                }
                for x in v {
                    out.extend_from_slice(&x.to_le_bytes());
                }
            }
            _ => {
                return Err(DbError::new(
                    ErrorCode::WrongValue,
                    format!("value does not match the type of column '{}'", def.name),
                ))
            }
        }
    }
    Ok(out)
}

pub fn decode_row(schema: &TableSchema, bytes: &[u8]) -> DbResult<Vec<Value>> {
    let corrupt = || {
        DbError::new(
            ErrorCode::Unknown,
            format!("corrupt row in table '{}'", schema.name),
        )
    };
    let mut out = Vec::with_capacity(schema.columns.len());
    let mut pos = 8usize;
    let prefix = prefix_pk(bytes).ok_or_else(corrupt)?;
    for def in &schema.columns {
        match def.ty {
            ColumnType::BigInt => {
                let raw = bytes.get(pos..pos + 8).ok_or_else(corrupt)?;
                out.push(Value::Int(i64::from_le_bytes(raw.try_into().unwrap())));
                pos += 8;
            }
            ColumnType::Varchar(_) => {
                let raw = bytes.get(pos..pos + 4).ok_or_else(corrupt)?;
                let len = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
                pos += 4;
                let text = bytes.get(pos..pos + len).ok_or_else(corrupt)?;
                let text = std::str::from_utf8(text).map_err(|_| corrupt())?;
                out.push(Value::Text(text.to_string()));
                pos += len;
            }
            ColumnType::Vector(dim) => {
                let len = dim * 4;
                let raw = bytes.get(pos..pos + len).ok_or_else(corrupt)?;
                let mut values = Vec::with_capacity(dim);
                for chunk in raw.chunks_exact(4) {
                    values.push(f32::from_le_bytes(chunk.try_into().unwrap()));
                }
                out.push(Value::Vector(values));
                pos += len;
            }
        }
    }
    if pos != bytes.len() {
        return Err(corrupt());
    }
    if schema.primary_key_value(&out) != prefix {
        return Err(corrupt());
    }
    Ok(out)
}

/// The primary key repeated at the start of every encoded row.
pub fn prefix_pk(bytes: &[u8]) -> Option<i64> {
    let raw = bytes.get(..8)?;
    Some(i64::from_le_bytes(raw.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn schema() -> TableSchema {
        let crate::ast::Statement::CreateTable(ct) =
            parse("CREATE TABLE docs (id BIGINT PRIMARY KEY, title VARCHAR(8), emb VECTOR(3))")
                .unwrap()
        else {
            panic!()
        };
        TableSchema::from_create(&ct).unwrap()
    }

    #[test]
    fn roundtrips_every_column_type() {
        let schema = schema();
        let row = vec![
            Value::Int(-42),
            Value::Text("数据库".into()),
            Value::Vector(vec![0.5, -1.25, 3.0]),
        ];
        let bytes = encode_row(&schema, &row).unwrap();
        assert_eq!(bytes.len(), 8 + 8 + 4 + 9 + 12);
        assert_eq!(prefix_pk(&bytes), Some(-42));
        assert_eq!(decode_row(&schema, &bytes).unwrap(), row);
    }

    #[test]
    fn detects_truncated_rows() {
        let schema = schema();
        let row = vec![
            Value::Int(1),
            Value::Text("x".into()),
            Value::Vector(vec![1.0, 2.0, 3.0]),
        ];
        let bytes = encode_row(&schema, &row).unwrap();
        assert!(decode_row(&schema, &bytes[..bytes.len() - 1]).is_err());
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(decode_row(&schema, &extra).is_err());
    }

    #[test]
    fn rejects_mismatched_value_types() {
        let schema = schema();
        let row = vec![
            Value::Int(1),
            Value::Vector(vec![1.0, 2.0, 3.0]),
            Value::Vector(vec![1.0, 2.0, 3.0]),
        ];
        assert_eq!(
            encode_row(&schema, &row).unwrap_err().code,
            ErrorCode::WrongValue
        );
    }
}
