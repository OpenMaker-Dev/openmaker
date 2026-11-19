//! The statement a parse tree describes (execute-sql.md 3.2), with the
//! positions FileMaker reports errors at (3.3). Built once per statement,
//! so variants are not boxed for size.
#![allow(clippy::large_enum_variant)]

pub use super::lex::Pos;

/// A name as written: its value (quotes removed, `""` as `"`) and the
/// position of its token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub pos: Pos,
}

#[derive(Clone, Debug)]
pub enum Statement {
    Query(Query),
    /// `TABLE name`: accepted by the grammar, rejected by FileMaker's
    /// visitor (9.3).
    Table {
        pos: Pos,
    },
    Insert {
        table: Name,
        columns: Vec<Name>,
        source: InsertSource,
    },
    Update {
        table: Name,
        assignments: Vec<(Name, Expr)>,
        filter: Option<Predicate>,
        cursor: Option<Name>,
    },
    Delete {
        table: Name,
        filter: Option<Predicate>,
        cursor: Option<Name>,
    },
    /// `CREATE TABLE`, `DROP TABLE`, `ALTER TABLE`, `CREATE INDEX`, `DROP
    /// INDEX`, `TRUNCATE TABLE`: the table named and the column, for the
    /// index forms and `ALTER ... DROP/ALTER COLUMN`.
    Ddl {
        kind: DdlKind,
        table: Name,
        column: Option<Name>,
    },
    DeclareCursor {
        name: Name,
        query: Query,
    },
    /// `OPEN`, `CLOSE`, `DEALLOCATE`, `FETCH` on a cursor.
    Cursor {
        name: Name,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DdlKind {
    CreateTable,
    DropTable,
    AlterTable,
    CreateIndex,
    DropIndex,
    Truncate,
}

#[derive(Clone, Debug)]
pub enum InsertSource {
    /// `VALUES` rows: the position of each row's `(` and its values.
    Values(Vec<(Pos, Vec<Expr>)>),
    Query(QueryExpr),
    Default,
}

/// `query_statement`.
#[derive(Clone, Debug)]
pub struct Query {
    pub body: QueryExpr,
    pub order_by: Vec<OrderSpec>,
    pub offset: Option<Offset>,
    pub fetch: Option<Fetch>,
}

#[derive(Clone, Debug)]
pub struct Offset {
    /// The `OFFSET` keyword.
    pub pos: Pos,
    pub count: Expr,
}

#[derive(Clone, Debug)]
pub struct Fetch {
    /// The `FETCH` keyword.
    pub pos: Pos,
    /// `FETCH NEXT` (rejected) rather than `FETCH FIRST`.
    pub next: bool,
    pub count: Option<Expr>,
    pub percent: bool,
    pub with_ties: bool,
}

#[derive(Clone, Debug)]
pub struct OrderSpec {
    pub expr: Expr,
    pub descending: bool,
}

/// A query and the queries `UNION`ed to it, left to right.
#[derive(Clone, Debug)]
pub struct QueryExpr {
    pub first: QueryTerm,
    pub unions: Vec<Union>,
}

#[derive(Clone, Debug)]
pub struct Union {
    pub all: bool,
    pub term: QueryTerm,
}

#[derive(Clone, Debug)]
pub enum QueryTerm {
    Table { pos: Pos },
    Spec(Box<QuerySpec>),
    Paren(Box<QueryExpr>),
}

impl QueryTerm {
    /// The position of the term's first `SELECT` (or `TABLE`).
    pub fn pos(&self) -> Pos {
        match self {
            QueryTerm::Table { pos } => *pos,
            QueryTerm::Spec(spec) => spec.pos,
            QueryTerm::Paren(inner) => inner.first.pos(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct QuerySpec {
    /// The `SELECT` keyword.
    pub pos: Pos,
    pub distinct: bool,
    pub select: SelectList,
    pub from: Vec<TableRef>,
    pub filter: Option<Predicate>,
    pub group_by: Option<GroupBy>,
    pub having: Option<Predicate>,
}

#[derive(Clone, Debug)]
pub struct GroupBy {
    /// The `GROUP` keyword.
    pub pos: Pos,
    pub columns: Vec<ColumnRef>,
}

#[derive(Clone, Debug)]
pub enum SelectList {
    Star(Pos),
    Items(Vec<SelectItem>),
}

#[derive(Clone, Debug)]
pub enum SelectItem {
    /// `t.*`.
    TableStar(Name),
    /// `GETAS(expr, 'TYPE' | DEFAULT)`.
    GetAs {
        expr: Expr,
        stream: Option<Name>,
        alias: Option<Name>,
    },
    Expr {
        expr: Expr,
        alias: Option<Name>,
    },
}

#[derive(Clone, Debug)]
pub enum TableRef {
    Table {
        name: Name,
        alias: Option<Name>,
    },
    Join {
        left: Box<TableRef>,
        kind: JoinKind,
        right: Box<TableRef>,
        on: Option<Predicate>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Cross,
}

#[derive(Clone, Debug)]
pub struct ColumnRef {
    pub table: Option<Name>,
    /// `None` for `ROWID`.
    pub column: Option<Name>,
    pub repetition: Option<Box<Expr>>,
    /// The first token (the qualifier, or the column).
    pub pos: Pos,
    /// The column's own token.
    pub column_pos: Pos,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    /// The first token of the expression.
    pub pos: Pos,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Subtract,
    Concat,
    Multiply,
    Divide,
    Power,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatePart {
    Day,
    Hour,
    Minute,
    Month,
    Second,
    Year,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    Avg,
    Count,
    Every,
    Max,
    Min,
    Some,
    Sum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimSide {
    Both,
    Leading,
    Trailing,
}

/// A `CAST` target type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastType {
    Int,
    Decimal,
    Boolean,
    Date,
    Time,
    Timestamp,
    Varchar,
    Binary,
}

#[derive(Clone, Debug)]
pub enum Literal {
    /// UNSIGNED_CONST or FLOAT_CONST as written.
    Number(String),
    Text(String),
    /// `DATE '...'`, `TIME '...'`, `TIMESTAMP '...'`: the string and the
    /// position of its token.
    Date(String, Pos),
    Time(String, Pos),
    Timestamp(String, Pos),
    /// `B'...'` and `X'...'` as written.
    Bit(String),
    Hex(String),
    Null,
    True,
    False,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Column(ColumnRef),
    Literal(Literal),
    /// `?`.
    Param,
    /// `:name`.
    Named(Name),
    Default,
    Negate(Box<Expr>),
    Plus(Box<Expr>),
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Interval {
        expr: Box<Expr>,
        subtract: bool,
        amount: Box<Expr>,
        unit: DatePart,
    },
    Case {
        operand: Option<Box<Expr>>,
        whens: Vec<(Arg, Expr)>,
        /// The `ELSE` keyword and its value (`None` for `ELSE NULL`).
        otherwise: Option<(Pos, Option<Box<Expr>>)>,
    },
    Cast {
        expr: Box<Expr>,
        ty: CastType,
    },
    Aggregate {
        func: Aggregate,
        distinct: bool,
        /// `None` for `COUNT(*)`.
        arg: Option<Box<Expr>>,
    },
    /// A scalar function: its name as written (upper case for keywords)
    /// and arguments; `parens` when written with parentheses.
    Function {
        name: Name,
        args: Vec<Arg>,
        parens: bool,
    },
    Extract {
        part: DatePart,
        expr: Box<Expr>,
    },
    Trim {
        side: Option<TrimSide>,
        chars: Option<Box<Expr>>,
        expr: Box<Expr>,
    },
    Subquery(Box<QueryExpr>),
    Nullif(Box<Expr>, Box<Expr>),
    Coalesce(Vec<Arg>),
    Paren(Box<Expr>),
}

/// A `common_expression`: a predicate or a value.
#[derive(Clone, Debug)]
pub enum Arg {
    Value(Expr),
    Predicate(Predicate),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareOp {
    Ne,
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quantifier {
    All,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsWhat {
    False,
    Null,
    True,
    Unknown,
}

#[derive(Clone, Debug)]
pub enum ListOrQuery {
    List(Vec<Arg>),
    Query(QueryExpr),
}

#[derive(Clone, Debug)]
pub enum Predicate {
    Or(Vec<Predicate>),
    And(Vec<Predicate>),
    Not(Box<Predicate>),
    Compare {
        left: Expr,
        op: CompareOp,
        right: Expr,
        op_pos: Pos,
    },
    Quantified {
        left: Expr,
        op: CompareOp,
        quantifier: Quantifier,
        list: ListOrQuery,
        op_pos: Pos,
    },
    In {
        left: Expr,
        negated: bool,
        list: ListOrQuery,
        /// The `IN` keyword.
        pos: Pos,
    },
    Is {
        expr: Expr,
        negated: bool,
        what: IsWhat,
        pos: Pos,
    },
    Between {
        expr: Expr,
        negated: bool,
        low: Expr,
        high: Expr,
        pos: Pos,
    },
    Like {
        expr: Expr,
        negated: bool,
        pattern: Expr,
        pos: Pos,
    },
    PartOf {
        part: Expr,
        negated: bool,
        whole: Expr,
        pos: Pos,
    },
    /// `value_expression ± INTERVAL value_expression unit` in predicate
    /// position.
    Interval {
        expr: Expr,
        pos: Pos,
    },
    Exists {
        unique: bool,
        query: QueryExpr,
        pos: Pos,
    },
}
