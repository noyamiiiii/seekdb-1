//! The catalog: the list of tables and their schemas, stored as one small
//! binary file that is rewritten atomically on DDL.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use crate::ast::{ColumnDef, ColumnType};
use crate::schema::TableSchema;

const MAGIC: &[u8; 8] = b"MVCAT01\n";

#[derive(Default)]
pub struct Catalog {
    pub tables: BTreeMap<String, TableSchema>,
}

impl Catalog {
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let corrupt = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a MiniVectorDB catalog", path.display()),
            )
        };
        if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
            return Err(corrupt());
        }
        let mut cursor = Cursor::new(&bytes[MAGIC.len()..]);
        let count = cursor.u32().ok_or_else(corrupt)?;
        let mut tables = BTreeMap::new();
        for _ in 0..count {
            let name = cursor.string().ok_or_else(corrupt)?;
            let columns = parse_columns(&mut cursor).ok_or_else(corrupt)?;
            let primary_key = cursor.u32().ok_or_else(corrupt)? as usize;
            let vector_column = match cursor.u32().ok_or_else(corrupt)? {
                u32::MAX => None,
                idx => Some(idx as usize),
            };
            let schema = TableSchema {
                name: name.clone(),
                columns,
                primary_key,
                vector_column,
            };
            tables.insert(name, schema);
        }
        Ok(Self { tables })
    }

    /// Writes the catalog through a temporary file so a crash never leaves a
    /// half-written catalog behind.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.tables.len() as u32).to_le_bytes());
        for schema in self.tables.values() {
            put_string(&mut out, &schema.name);
            out.extend_from_slice(&(schema.columns.len() as u32).to_le_bytes());
            for column in &schema.columns {
                put_string(&mut out, &column.name);
                match column.ty {
                    ColumnType::BigInt => {
                        out.push(0);
                        out.extend_from_slice(&0u32.to_le_bytes());
                    }
                    ColumnType::Varchar(len) => {
                        out.push(1);
                        out.extend_from_slice(&(len as u32).to_le_bytes());
                    }
                    ColumnType::Vector(dim) => {
                        out.push(2);
                        out.extend_from_slice(&(dim as u32).to_le_bytes());
                    }
                }
            }
            out.extend_from_slice(&(schema.primary_key as u32).to_le_bytes());
            match schema.vector_column {
                Some(idx) => out.extend_from_slice(&(idx as u32).to_le_bytes()),
                None => out.extend_from_slice(&u32::MAX.to_le_bytes()),
            }
        }

        let tmp = path.with_extension("tmp");
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&out)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, path)
    }
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn parse_columns(cursor: &mut Cursor<'_>) -> Option<Vec<ColumnDef>> {
    let count = cursor.u32()?;
    let mut columns = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = cursor.string()?;
        let tag = cursor.take(1)?[0];
        let param = cursor.u32()? as usize;
        let ty = match tag {
            0 => ColumnType::BigInt,
            1 => ColumnType::Varchar(param),
            2 => ColumnType::Vector(param),
            _ => return None,
        };
        columns.push(ColumnDef { name, ty });
    }
    Some(columns)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}
