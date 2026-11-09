//! Statement AST produced by [`crate::parser`] and consumed by the executor.
//!
//! The subset mirrors the competition statement: single client, one explicit
//! transaction at a time, no NULLs, no JOINs, no aggregates, no subqueries.

/// A literal value as written in a statement (or a stored cell).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Text(String),
    Vector(Vec<f32>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnType {
    BigInt,
    /// Character length: Unicode scalar values, not bytes.
    Varchar(usize),
    /// Fixed dimension, `float32` elements.
    Vector(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    CreateTable(CreateTable),
    CreateVectorIndex(CreateVectorIndex),
    DropTable(DropTable),
    Insert(Insert),
    Select(Select),
    Update(Update),
    Delete(Delete),
    Set(SetVar),
    Begin,
    Commit,
    Rollback,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateTable {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub primary_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateVectorIndex {
    /// Index name; informational, kept for error messages.
    pub name: String,
    pub table: String,
    pub column: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DropTable {
    pub table: String,
    /// `DROP TABLE IF EXISTS`: a missing table is not an error.
    pub if_exists: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Insert {
    pub table: String,
    /// Explicit column list, or `None` for positional `VALUES` over all columns.
    pub columns: Option<Vec<String>>,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Select {
    pub items: Vec<SelectItem>,
    pub table: String,
    pub filter: Option<Expr>,
    pub order_by: Option<OrderBy>,
    /// `None` means "no LIMIT"; `Some(0)` returns an empty result set.
    pub limit: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SelectItem {
    /// `*`: every column in table order.
    Star,
    Column(String),
    /// `L2_DISTANCE(vector_column, '[1,2,3]')`, optionally aliased.
    L2Distance {
        column: String,
        query: Vec<f32>,
        alias: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderBy {
    pub expr: Expr,
    /// `ORDER BY x` is ascending; `ORDER BY x DESC` sets this to false.
    pub ascending: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub table: String,
    pub assignments: Vec<(String, Value)>,
    pub filter: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Delete {
    pub table: String,
    pub filter: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SetVar {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Column(String),
    Lit(Value),
    /// `L2_DISTANCE(col, '[...]')` used in an expression position.
    Distance {
        column: String,
        query: Vec<f32>,
    },
    Compare {
        left: Box<Expr>,
        op: CmpOp,
        right: Box<Expr>,
    },
    And(Box<Expr>, Box<Expr>),
}

impl Expr {
    /// Plan-time check: the evaluator only supports comparisons against
    /// literals, possibly combined with `AND`.
    pub fn is_supported_filter(&self) -> bool {
        match self {
            Expr::Compare { left, right, .. } => {
                matches!(&**left, Expr::Column(_) | Expr::Lit(_))
                    && matches!(&**right, Expr::Column(_) | Expr::Lit(_))
                    && !matches!(&**left, Expr::Distance { .. })
                    && !matches!(&**right, Expr::Distance { .. })
            }
            Expr::And(l, r) => l.is_supported_filter() && r.is_supported_filter(),
            _ => false,
        }
    }
}
