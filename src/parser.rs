//! Recursive-descent parser for the supported SQL subset.
//!
//! Anything outside the subset (valid SQL we do not implement, such as `JOIN`
//! or aggregates) is reported as [`ErrorCode::NotSupported`]; malformed input is
//! [`ErrorCode::Syntax`].

use crate::ast::{
    CmpOp, ColumnDef, ColumnType, CreateTable, CreateVectorIndex, Delete, DropTable, Expr, Insert,
    OrderBy, Select, SelectItem, SetVar, Statement, Update, Value,
};
use crate::error::{DbError, DbResult, ErrorCode};
use crate::lexer::{tokenize, Kind, Token};

/// Parses one statement (already stripped of its terminating `;`).
pub fn parse(statement: &str) -> DbResult<Statement> {
    let tokens = tokenize(statement)?;
    let mut parser = Parser { tokens, pos: 0 };
    let stmt = parser.parse_statement()?;
    parser.expect_end()?;
    Ok(stmt)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        // `tokenize` always appends an `End` token.
        &self.tokens[self.pos]
    }

    fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if tok.kind != Kind::End {
            self.pos += 1;
        }
        tok
    }

    fn at_word(&self, keyword: &str) -> bool {
        self.peek().is_word(keyword)
    }

    fn eat_word(&mut self, keyword: &str) -> bool {
        if self.at_word(keyword) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_word(&mut self, keyword: &str) -> DbResult<()> {
        if self.eat_word(keyword) {
            Ok(())
        } else {
            Err(self.error_here(format!("expected {keyword}")))
        }
    }

    fn expect_kind(&mut self, kind: Kind, what: &str) -> DbResult<()> {
        if self.peek().kind == kind {
            self.bump();
            Ok(())
        } else {
            Err(self.error_here(format!("expected {what}")))
        }
    }

    fn expect_end(&mut self) -> DbResult<()> {
        if self.peek().kind == Kind::End {
            Ok(())
        } else {
            Err(self.error_here("unexpected token after statement"))
        }
    }

    fn error_here(&self, message: impl Into<String>) -> DbError {
        let pos = self.peek().pos;
        DbError::syntax(format!("{} (at character {})", message.into(), pos + 1))
    }

    fn unsupported_here(&self, what: &str) -> DbError {
        DbError::new(
            ErrorCode::NotSupported,
            format!("{what} (at character {})", self.peek().pos + 1),
        )
    }

    /// Identifier: a bare word that is not one of the reserved keywords used by
    /// the grammar.
    fn identifier(&mut self) -> DbResult<String> {
        let tok = self.peek().clone();
        if tok.kind != Kind::Word || is_reserved(&tok.text) {
            return Err(self.error_here("expected an identifier"));
        }
        self.bump();
        Ok(tok.text)
    }

    fn parse_statement(&mut self) -> DbResult<Statement> {
        let tok = self.peek().clone();
        if tok.kind != Kind::Word {
            return Err(self.error_here("expected a statement"));
        }
        let stmt = if tok.text.eq_ignore_ascii_case("create") {
            self.bump();
            self.parse_create()?
        } else if tok.text.eq_ignore_ascii_case("insert") {
            self.bump();
            self.parse_insert()?
        } else if tok.text.eq_ignore_ascii_case("select") {
            self.bump();
            self.parse_select()?
        } else if tok.text.eq_ignore_ascii_case("update") {
            self.bump();
            self.parse_update()?
        } else if tok.text.eq_ignore_ascii_case("delete") {
            self.bump();
            self.parse_delete()?
        } else if tok.text.eq_ignore_ascii_case("drop") {
            self.bump();
            self.parse_drop()?
        } else if tok.text.eq_ignore_ascii_case("truncate") {
            self.bump();
            self.eat_word("table");
            Statement::Truncate(self.identifier()?)
        } else if tok.text.eq_ignore_ascii_case("show") {
            self.bump();
            self.parse_show()?
        } else if tok.text.eq_ignore_ascii_case("describe") || tok.text.eq_ignore_ascii_case("desc") {
            self.bump();
            self.eat_word("table");
            Statement::ShowColumns {
                table: self.identifier()?,
            }
        } else if tok.text.eq_ignore_ascii_case("use") {
            self.bump();
            self.identifier()?;
            Statement::Noop
        } else if tok.text.eq_ignore_ascii_case("start") {
            self.bump();
            self.expect_word("transaction")?;
            Statement::Begin
        } else if tok.text.eq_ignore_ascii_case("set") {
            self.bump();
            self.parse_set()?
        } else if tok.text.eq_ignore_ascii_case("begin") {
            self.bump();
            self.eat_word("work");
            Statement::Begin
        } else if tok.text.eq_ignore_ascii_case("commit") {
            self.bump();
            Statement::Commit
        } else if tok.text.eq_ignore_ascii_case("rollback") {
            self.bump();
            Statement::Rollback
        } else {
            return Err(self.unsupported_here(&format!(
                "statement '{}' is not supported",
                tok.text.to_uppercase()
            )));
        };
        Ok(stmt)
    }

    fn parse_create(&mut self) -> DbResult<Statement> {
        if self.eat_word("table") {
            self.parse_create_table()
        } else if self.eat_word("vector") {
            self.expect_word("index")?;
            self.parse_create_vector_index()
        } else if self.eat_word("database") || self.eat_word("schema") {
            self.eat_if_not_exists();
            self.identifier()?;
            Ok(Statement::Noop)
        } else if self.at_word("index") {
            Err(self.unsupported_here("plain CREATE INDEX is not supported"))
        } else {
            Err(self.error_here("expected TABLE or VECTOR INDEX after CREATE"))
        }
    }

    fn parse_drop(&mut self) -> DbResult<Statement> {
        if self.eat_word("index") {
            let name = self.identifier()?;
            self.expect_word("on")?;
            let table = self.identifier()?;
            return Ok(Statement::DropIndex { table, name });
        }
        if self.eat_word("database") || self.eat_word("schema") {
            self.eat_if_exists();
            self.identifier()?;
            return Ok(Statement::Noop);
        }
        self.parse_drop_table()
    }

    /// Consumes `IF NOT EXISTS` when present (accepted, then ignored).
    fn eat_if_not_exists(&mut self) -> bool {
        if self.at_word("if")
            && self.peek_at(1).is_some_and(|t| t.is_word("not"))
            && self.peek_at(2).is_some_and(|t| t.is_word("exists"))
        {
            self.bump();
            self.bump();
            self.bump();
            return true;
        }
        false
    }

    /// Consumes `IF EXISTS` when present (accepted, then ignored).
    fn eat_if_exists(&mut self) -> bool {
        if self.at_word("if") && self.peek_at(1).is_some_and(|t| t.is_word("exists")) {
            self.bump();
            self.bump();
            return true;
        }
        false
    }

    fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.pos + offset)
    }

    fn parse_show(&mut self) -> DbResult<Statement> {
        if self.eat_word("tables") {
            return Ok(Statement::ShowTables);
        }
        if self.eat_word("columns") {
            self.expect_word("from")?;
            return Ok(Statement::ShowColumns {
                table: self.identifier()?,
            });
        }
        Err(self.unsupported_here("only SHOW TABLES and SHOW COLUMNS are supported"))
    }

    fn parse_drop_table(&mut self) -> DbResult<Statement> {
        self.expect_word("table")?;
        let if_exists = if self.eat_word("if") {
            self.expect_word("exists")?;
            true
        } else {
            false
        };
        let table = self.identifier()?;
        Ok(Statement::DropTable(DropTable { table, if_exists }))
    }

    fn parse_create_table(&mut self) -> DbResult<Statement> {
        let name = self.identifier()?;
        self.expect_kind(Kind::LParen, "'('")?;
        let mut columns: Vec<ColumnDef> = Vec::new();
        let mut primary_key: Option<String> = None;
        loop {
            if self.at_word("primary") {
                self.bump();
                self.expect_word("key")?;
                self.expect_kind(Kind::LParen, "'('")?;
                let col = self.identifier()?;
                self.expect_kind(Kind::RParen, "')'")?;
                if primary_key.is_some() { return Err(self.error_here("multiple primary keys are not supported")); }
                primary_key = Some(col);
            } else {
                let col_name = self.identifier()?;
                let ty = self.parse_column_type()?;
                if self.eat_word("primary") {
                    self.expect_word("key")?;
                    if primary_key.is_some() { return Err(self.error_here("multiple primary keys are not supported")); }
                    primary_key = Some(col_name.clone());
                }
                columns.push(ColumnDef { name: col_name, ty });
            }
            if self.peek().kind == Kind::Comma {
                self.bump();
                continue;
            }
            break;
        }
        self.expect_kind(Kind::RParen, "')'")?;
        if columns.is_empty() {
            return Err(self.error_here("a table needs at least one column"));
        }
        Ok(Statement::CreateTable(CreateTable {
            name,
            columns,
            primary_key,
        }))
    }

    fn parse_column_type(&mut self) -> DbResult<ColumnType> {
        if self.eat_word("bigint") {
            return Ok(ColumnType::BigInt);
        }
        if self.eat_word("varchar") {
            self.expect_kind(Kind::LParen, "'('")?;
            let n = self.parse_positive_usize("VARCHAR length")?;
            self.expect_kind(Kind::RParen, "')'")?;
            return Ok(ColumnType::Varchar(n));
        }
        if self.eat_word("vector") {
            self.expect_kind(Kind::LParen, "'('")?;
            let n = self.parse_positive_usize("VECTOR dimension")?;
            self.expect_kind(Kind::RParen, "')'")?;
            return Ok(ColumnType::Vector(n));
        }
        Err(self.error_here("expected BIGINT, VARCHAR(n) or VECTOR(n)"))
    }

    fn parse_create_vector_index(&mut self) -> DbResult<Statement> {
        let name = self.identifier()?;
        self.expect_word("on")?;
        let table = self.identifier()?;
        self.expect_kind(Kind::LParen, "'('")?;
        let column = self.identifier()?;
        self.expect_kind(Kind::RParen, "')'")?;
        Ok(Statement::CreateVectorIndex(CreateVectorIndex {
            name,
            table,
            column,
        }))
    }

    fn parse_insert(&mut self) -> DbResult<Statement> {
        self.expect_word("into")?;
        let table = self.identifier()?;
        let columns = if self.peek().kind == Kind::LParen {
            self.bump();
            let mut cols = Vec::new();
            loop {
                cols.push(self.identifier()?);
                if self.peek().kind == Kind::Comma {
                    self.bump();
                    continue;
                }
                break;
            }
            self.expect_kind(Kind::RParen, "')'")?;
            Some(cols)
        } else {
            None
        };
        self.expect_word("values")?;
        let mut rows = Vec::new();
        loop {
            self.expect_kind(Kind::LParen, "'('")?;
            let mut row = Vec::new();
            loop {
                row.push(self.parse_value()?);
                if self.peek().kind == Kind::Comma {
                    self.bump();
                    continue;
                }
                break;
            }
            self.expect_kind(Kind::RParen, "')'")?;
            rows.push(row);
            if self.peek().kind == Kind::Comma {
                self.bump();
                continue;
            }
            break;
        }
        Ok(Statement::Insert(Insert {
            table,
            columns,
            rows,
        }))
    }

    fn parse_select(&mut self) -> DbResult<Statement> {
        let mut items: Vec<SelectItem> = Vec::new();
        let mut consts: Vec<(Value, Option<String>)> = Vec::new();
        loop {
            if matches!(self.peek().kind, Kind::Number | Kind::Str | Kind::LBracket) {
                let value = self.parse_value()?;
                let alias = self.parse_optional_alias()?;
                consts.push((value, alias));
            } else {
                items.push(self.parse_select_item()?);
            }
            if self.peek().kind == Kind::Comma {
                self.bump();
                continue;
            }
            break;
        }
        if !self.eat_word("from") {
            if !consts.is_empty() && items.is_empty() {
                return Ok(Statement::SelectConst { items: consts });
            }
            return Err(self.error_here("expected FROM"));
        }
        if !consts.is_empty() {
            return Err(self.unsupported_here("literal and column select items cannot be mixed"));
        }
        let table = self.identifier()?;
        let filter = if self.eat_word("where") {
            Some(self.parse_filter()?)
        } else {
            None
        };
        let order_by = if self.eat_word("order") {
            self.expect_word("by")?;
            let expr = self.parse_order_expr()?;
            let ascending = if self.eat_word("desc") {
                false
            } else {
                self.eat_word("asc");
                true
            };
            Some(OrderBy { expr, ascending })
        } else {
            None
        };
        let limit = if self.eat_word("limit") {
            Some(self.parse_limit()?)
        } else {
            None
        };
        Ok(Statement::Select(Select {
            items,
            table,
            filter,
            order_by,
            limit,
        }))
    }

    fn parse_optional_alias(&mut self) -> DbResult<Option<String>> {
        if self.eat_word("as") {
            return Ok(Some(self.identifier()?));
        }
        // `SELECT 1 one` is valid MySQL too, but `FROM` must not be swallowed.
        if self.peek().kind == Kind::Word && !self.at_word("from") {
            return Ok(Some(self.identifier()?));
        }
        Ok(None)
    }

    fn parse_select_item(&mut self) -> DbResult<SelectItem> {
        if self.peek().kind == Kind::Star {
            self.bump();
            return Ok(SelectItem::Star);
        }
        if self.at_word("l2_distance") {
            let (column, query) = self.parse_l2_call()?;
            let alias = self.parse_optional_alias()?;
            return Ok(SelectItem::L2Distance {
                column,
                query,
                alias,
            });
        }
        let name = self.identifier()?;
        // `count(*)`, `sum(x)`, ... are valid SQL we do not implement.
        if self.peek().kind == Kind::LParen {
            return Err(DbError::new(
                ErrorCode::NotSupported,
                format!("function '{name}' is not supported"),
            ));
        }
        match self.parse_optional_alias()? {
            Some(alias) => Ok(SelectItem::AliasedColumn { column: name, alias }),
            None => Ok(SelectItem::Column(name)),
        }
    }

    /// Parses `L2_DISTANCE ( column , '[1,2,3]' )` or `L2_DISTANCE ( column , [1,2,3] )`.
    fn parse_l2_call(&mut self) -> DbResult<(String, Vec<f32>)> {
        self.expect_word("l2_distance")?;
        self.expect_kind(Kind::LParen, "'('")?;
        let column = self.identifier()?;
        self.expect_kind(Kind::Comma, "','")?;
        let query = if self.peek().kind == Kind::Str {
            // The query vector is normally passed as a string constant.
            let text = self.bump().text;
            parse_vector_string(&text)?
        } else {
            self.parse_vector_literal()?
        };
        self.expect_kind(Kind::RParen, "')'")?;
        Ok((column, query))
    }

    fn parse_order_expr(&mut self) -> DbResult<Expr> {
        if self.at_word("l2_distance") {
            let (column, query) = self.parse_l2_call()?;
            return Ok(Expr::Distance { column, query });
        }
        Ok(Expr::Column(self.identifier()?))
    }

    fn parse_filter(&mut self) -> DbResult<Expr> {
        self.parse_conjunction(0)
    }

    fn parse_conjunction(&mut self, depth: usize) -> DbResult<Expr> {
        if depth > 128 {
            return Err(self.unsupported_here("predicate nesting is too deep"));
        }
        let mut expr = self.parse_predicate(depth)?;
        while self.eat_word("and") {
            let rhs = self.parse_predicate(depth)?;
            expr = Expr::And(Box::new(expr), Box::new(rhs));
        }
        if self.at_word("or") {
            return Err(self.unsupported_here("OR is not supported"));
        }
        Ok(expr)
    }

    fn parse_predicate(&mut self, depth: usize) -> DbResult<Expr> {
        if self.peek().kind == Kind::LParen {
            self.bump();
            let expr = self.parse_conjunction(depth + 1)?;
            self.expect_kind(Kind::RParen, "')'")?;
            Ok(expr)
        } else {
            self.parse_comparison()
        }
    }

    fn parse_comparison(&mut self) -> DbResult<Expr> {
        let left = self.parse_operand()?;
        if self.eat_word("between") {
            let low = self.parse_operand()?;
            self.expect_word("and")?;
            let high = self.parse_operand()?;
            let lower = Expr::Compare {
                left: Box::new(left.clone()),
                op: CmpOp::Ge,
                right: Box::new(low),
            };
            let upper = Expr::Compare {
                left: Box::new(left),
                op: CmpOp::Le,
                right: Box::new(high),
            };
            return Ok(Expr::And(Box::new(lower), Box::new(upper)));
        }
        if self.eat_word("in") {
            self.expect_kind(Kind::LParen, "'('")?;
            let mut values = Vec::new();
            loop {
                values.push(self.parse_value()?);
                if self.peek().kind == Kind::Comma {
                    self.bump();
                    continue;
                }
                break;
            }
            self.expect_kind(Kind::RParen, "')'")?;
            return Ok(Expr::In {
                left: Box::new(left),
                values,
            });
        }
        let op = match self.peek().kind {
            Kind::Eq => CmpOp::Eq,
            Kind::Lt => CmpOp::Lt,
            Kind::Le => CmpOp::Le,
            Kind::Gt => CmpOp::Gt,
            Kind::Ge => CmpOp::Ge,
            _ => return Err(self.error_here("expected a comparison operator")),
        };
        self.bump();
        let right = self.parse_operand()?;
        Ok(Expr::Compare {
            left: Box::new(left),
            op,
            right: Box::new(right),
        })
    }

    fn parse_operand(&mut self) -> DbResult<Expr> {
        if self.peek().kind == Kind::Word {
            return Ok(Expr::Column(self.identifier()?));
        }
        Ok(Expr::Lit(self.parse_value()?))
    }

    fn parse_value(&mut self) -> DbResult<Value> {
        match self.peek().kind {
            Kind::Number => {
                let tok = self.bump();
                let int = tok.text.parse::<i64>().map_err(|_| {
                    DbError::new(
                        ErrorCode::OutOfRange,
                        format!("integer '{}' is out of range", tok.text),
                    )
                })?;
                Ok(Value::Int(int))
            }
            Kind::Str => {
                let tok = self.bump();
                Ok(Value::Text(tok.text))
            }
            Kind::LBracket => Ok(Value::Vector(self.parse_vector_literal()?)),
            _ => Err(self.error_here("expected a literal value")),
        }
    }

    fn parse_vector_literal(&mut self) -> DbResult<Vec<f32>> {
        // Malformed vectors are value errors, not syntax errors: the literal
        // shape is valid SQL, its content is not.
        self.expect_kind(Kind::LBracket, "'['")?;
        let mut values = Vec::new();
        loop {
            let tok = self.peek().clone();
            if tok.kind != Kind::Number {
                return Err(DbError::new(
                    ErrorCode::WrongValue,
                    format!(
                        "vector element at character {} is not a number",
                        tok.pos + 1
                    ),
                ));
            }
            self.bump();
            let parsed = tok.text.parse::<f32>().map_err(|_| {
                DbError::new(
                    ErrorCode::WrongValue,
                    format!("'{}' is not a valid float32", tok.text),
                )
            })?;
            if !parsed.is_finite() {
                return Err(DbError::new(
                    ErrorCode::WrongValue,
                    format!("'{}' is not a finite float32", tok.text),
                ));
            }
            values.push(parsed);
            if self.peek().kind == Kind::Comma {
                self.bump();
                continue;
            }
            break;
        }
        self.expect_kind(Kind::RBracket, "']'")?;
        if values.is_empty() {
            return Err(DbError::new(
                ErrorCode::WrongValue,
                "a vector needs at least one element",
            ));
        }
        Ok(values)
    }

    fn parse_update(&mut self) -> DbResult<Statement> {
        let table = self.identifier()?;
        self.expect_word("set")?;
        let mut assignments = Vec::new();
        loop {
            let column = self.identifier()?;
            self.expect_kind(Kind::Eq, "'='")?;
            let value = self.parse_value()?;
            assignments.push((column, value));
            if self.peek().kind == Kind::Comma {
                self.bump();
                continue;
            }
            break;
        }
        let filter = if self.eat_word("where") {
            Some(self.parse_filter()?)
        } else {
            None
        };
        Ok(Statement::Update(Update {
            table,
            assignments,
            filter,
        }))
    }

    fn parse_delete(&mut self) -> DbResult<Statement> {
        self.expect_word("from")?;
        let table = self.identifier()?;
        let filter = if self.eat_word("where") {
            Some(self.parse_filter()?)
        } else {
            None
        };
        Ok(Statement::Delete(Delete { table, filter }))
    }

    fn parse_set(&mut self) -> DbResult<Statement> {
        let name = self.identifier()?;
        if self.peek().kind == Kind::Eq {
            self.bump();
        }
        // `SET NAMES utf8mb4` has no '=', and the server ignores the value of
        // every option anyway, so both spellings are accepted here.
        let value = match self.peek().kind {
            Kind::Str | Kind::Word | Kind::Number => self.bump().text,
            _ => return Err(self.error_here("expected a value after SET")),
        };
        Ok(Statement::Set(SetVar { name, value }))
    }

    fn parse_limit(&mut self) -> DbResult<u64> {
        let tok = self.peek().clone();
        if tok.kind != Kind::Number {
            return Err(self.error_here("expected an integer after LIMIT"));
        }
        self.bump();
        if tok.text.starts_with('-') {
            return Err(DbError::new(
                ErrorCode::OutOfRange,
                "LIMIT must not be negative",
            ));
        }
        tok.text
            .parse::<u64>()
            .map_err(|_| DbError::new(ErrorCode::OutOfRange, "LIMIT is out of range"))
    }

    fn parse_positive_usize(&mut self, what: &str) -> DbResult<usize> {
        let tok = self.peek().clone();
        if tok.kind != Kind::Number {
            return Err(self.error_here(format!("expected a number for {what}")));
        }
        self.bump();
        let n = tok.text.parse::<usize>().map_err(|_| {
            DbError::new(
                ErrorCode::OutOfRange,
                format!("{what} '{}' is out of range", tok.text),
            )
        })?;
        if n == 0 {
            return Err(DbError::new(
                ErrorCode::OutOfRange,
                format!("{what} must be greater than zero"),
            ));
        }
        Ok(n)
    }
}

/// Parses the contents of a vector written as a string constant (`'[1,2,3]'`).
///
/// Also used by [`crate::schema`]: a vector column accepts the bracketed literal
/// both bare (`[1,2,3]`) and quoted (`'[1,2,3]'`), and the quoted form reaches
/// the storage layer as a text value that has to be recognised.
pub(crate) fn parse_vector_string(text: &str) -> DbResult<Vec<f32>> {
    let tokens = tokenize(text).map_err(|_| {
        DbError::new(
            ErrorCode::WrongValue,
            format!("'{text}' is not a valid vector literal"),
        )
    })?;
    let mut parser = Parser { tokens, pos: 0 };
    let values = parser.parse_vector_literal().map_err(|_| {
        DbError::new(
            ErrorCode::WrongValue,
            format!("'{text}' is not a valid vector literal"),
        )
    })?;
    parser.expect_end().map_err(|_| {
        DbError::new(
            ErrorCode::WrongValue,
            format!("'{text}' is not a valid vector literal"),
        )
    })?;
    Ok(values)
}

fn is_reserved(word: &str) -> bool {
    const RESERVED: &[&str] = &[
        "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create",
        "table", "vector", "index", "primary", "key", "and", "or", "order", "by", "limit", "as",
        "begin", "commit", "rollback", "on", "desc", "asc",
    ];
    RESERVED.iter().any(|r| word.eq_ignore_ascii_case(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(sql: &str) -> Statement {
        parse(sql).unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    #[test]
    fn parses_create_table() {
        let stmt =
            parse_ok("CREATE TABLE docs (id BIGINT PRIMARY KEY, title VARCHAR(64), emb VECTOR(3))");
        let Statement::CreateTable(ct) = stmt else {
            panic!("wrong statement")
        };
        assert_eq!(ct.name, "docs");
        assert_eq!(ct.primary_key.as_deref(), Some("id"));
        assert_eq!(ct.columns.len(), 3);
        assert_eq!(ct.columns[1].ty, ColumnType::Varchar(64));
        assert_eq!(ct.columns[2].ty, ColumnType::Vector(3));
    }

    #[test]
    fn parses_table_level_primary_key() {
        let stmt = parse_ok("CREATE TABLE t (id BIGINT, PRIMARY KEY (id))");
        let Statement::CreateTable(ct) = stmt else {
            panic!()
        };
        assert_eq!(ct.primary_key.as_deref(), Some("id"));
    }

    #[test]
    fn parses_create_vector_index() {
        let stmt = parse_ok("CREATE VECTOR INDEX idx ON docs (emb)");
        let Statement::CreateVectorIndex(ci) = stmt else {
            panic!()
        };
        assert_eq!(ci.name, "idx");
        assert_eq!(ci.table, "docs");
        assert_eq!(ci.column, "emb");
    }

    #[test]
    fn parses_multi_row_insert() {
        let stmt = parse_ok("INSERT INTO docs VALUES (1, 'a', [1,2,3]), (2, 'b', [4,5,6])");
        let Statement::Insert(ins) = stmt else {
            panic!()
        };
        assert_eq!(ins.rows.len(), 2);
        assert_eq!(ins.rows[1][0], Value::Int(2));
        assert_eq!(ins.rows[0][2], Value::Vector(vec![1.0, 2.0, 3.0]));
    }

    #[test]
    fn parses_insert_with_column_list() {
        let stmt = parse_ok("INSERT INTO docs (id, emb) VALUES (1, [1,2,3])");
        let Statement::Insert(ins) = stmt else {
            panic!()
        };
        assert_eq!(ins.columns.as_deref().map(|c| c.len()), Some(2));
        assert_eq!(ins.rows[0].len(), 2);
    }

    #[test]
    fn parses_topk_select() {
        let stmt = parse_ok(
            "SELECT id, L2_DISTANCE(emb, '[1,2,3]') AS dist FROM docs \
             WHERE id >= 10 AND title = 'x' ORDER BY dist LIMIT 5",
        );
        let Statement::Select(sel) = stmt else {
            panic!()
        };
        assert_eq!(sel.items.len(), 2);
        assert!(matches!(sel.items[1], SelectItem::L2Distance { .. }));
        assert!(sel.filter.is_some());
        assert_eq!(sel.limit, Some(5));
        assert!(matches!(sel.order_by.unwrap().expr, Expr::Column(_)));
    }

    #[test]
    fn parses_order_by_distance_desc() {
        let stmt = parse_ok("SELECT id FROM docs ORDER BY L2_DISTANCE(emb, [1,2,3]) DESC LIMIT 10");
        let Statement::Select(sel) = stmt else {
            panic!()
        };
        let order = sel.order_by.unwrap();
        assert!(!order.ascending);
        assert!(matches!(order.expr, Expr::Distance { .. }));
    }

    #[test]
    fn parses_string_form_distance_vector() {
        let stmt = parse_ok("SELECT L2_DISTANCE(emb, '[1, 2.5, -3e-1]') FROM t");
        let Statement::Select(sel) = stmt else {
            panic!()
        };
        let SelectItem::L2Distance { query, .. } = &sel.items[0] else {
            panic!("wrong item")
        };
        assert_eq!(query, &vec![1.0, 2.5, -0.3]);
    }

    #[test]
    fn rejects_malformed_vector_string() {
        let err = parse("SELECT L2_DISTANCE(emb, '[1,2') FROM t").unwrap_err();
        assert_eq!(err.code, ErrorCode::WrongValue);
    }

    #[test]
    fn parses_update_and_delete() {
        let Statement::Update(u) = parse_ok("UPDATE docs SET title = 'z' WHERE id = 1") else {
            panic!()
        };
        assert_eq!(u.assignments.len(), 1);
        let Statement::Delete(d) = parse_ok("DELETE FROM docs WHERE id > 5") else {
            panic!()
        };
        assert!(d.filter.is_some());
    }

    #[test]
    fn parses_transactions_and_set() {
        assert_eq!(parse_ok("BEGIN"), Statement::Begin);
        assert_eq!(parse_ok("COMMIT"), Statement::Commit);
        assert_eq!(parse_ok("ROLLBACK"), Statement::Rollback);
        let Statement::Set(s) = parse_ok("SET vector_mode = 'ann'") else {
            panic!()
        };
        assert_eq!(s.name, "vector_mode");
        assert_eq!(s.value, "ann");
    }

    #[test]
    fn rejects_unsupported_statements() {
        let err = parse("ALTER TABLE t ADD COLUMN x BIGINT").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotSupported);
        let err = parse("SELECT count(*) FROM t").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotSupported);
    }

    #[test]
    fn rejects_or_and_negative_limit() {
        assert_eq!(
            parse("SELECT id FROM t WHERE id = 1 OR id = 2")
                .unwrap_err()
                .code,
            ErrorCode::NotSupported
        );
        assert_eq!(
            parse("SELECT id FROM t LIMIT -1").unwrap_err().code,
            ErrorCode::OutOfRange
        );
    }

    #[test]
    fn rejects_non_finite_vector_elements() {
        let err = parse("INSERT INTO t VALUES (1, [1e40])").unwrap_err();
        assert_eq!(err.code, ErrorCode::WrongValue);
    }

    #[test]
    fn reports_syntax_errors_with_position() {
        let err = parse("SELECT FROM t").unwrap_err();
        assert_eq!(err.code, ErrorCode::Syntax);
        assert!(err.message.contains("character"));
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert_eq!(
            parse("SELECT id FROM t garbage").unwrap_err().code,
            ErrorCode::Syntax
        );
    }
}
