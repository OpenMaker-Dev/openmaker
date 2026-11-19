//! Building a statement (execute-sql.md 4, 5, 7, 11): names resolved
//! against the source's occurrences and fields, types assigned, and the
//! checks FileMaker's visitor makes, each with its FQL code and position.
//! The result is a plan [`crate::exec`] runs.

use std::rc::Rc;

use om_calc::{Number, Value};

use crate::error::SqlError;
use crate::func::{self, Func};
use crate::parse::ast::*;
use crate::source::{
    self, FieldClass, FieldDef, SqlSource, SystemColumn, SystemTable, TableDef, ValueListValues,
};
use crate::stack;
use crate::value::{Ty, Val};

/// Where a table instance's rows come from.
#[derive(Clone, Debug)]
pub enum InstSrc {
    User {
        table: TableDef,
        fields: Vec<FieldDef>,
    },
    /// System table and value list rows, read when the statement is built.
    Rows {
        columns: Vec<(String, Ty)>,
        rows: Rc<Vec<Vec<Value>>>,
    },
}

/// A table in a `FROM`.
#[derive(Clone, Debug)]
pub struct Inst {
    /// The name columns qualify it by: its alias, or the table's name.
    pub name: String,
    pub src: InstSrc,
}

/// A field repetition.
#[derive(Clone, Debug)]
pub enum Rep {
    Const(u32),
    Expr(Box<E>),
}

/// How a `?` reads (5.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PMode {
    /// Its calculation value (select list, arithmetic).
    Raw,
    /// Converted for a comparison with a value of this type.
    Compare(Ty),
    /// Converted as the calculation converts (function arguments).
    Convert(Ty),
}

/// A bound expression.
#[derive(Clone, Debug)]
pub enum E {
    Const(Val),
    Field {
        depth: usize,
        inst: usize,
        field: usize,
        rep: Rep,
    },
    Col {
        depth: usize,
        inst: usize,
        col: usize,
    },
    RowId {
        depth: usize,
        inst: usize,
    },
    RowModId {
        depth: usize,
        inst: usize,
    },
    Param {
        index: usize,
        mode: PMode,
    },
    Neg(Box<E>),
    Arith {
        op: BinOp,
        l: Box<E>,
        r: Box<E>,
        ty: Ty,
    },
    Concat(Box<E>, Box<E>),
    Interval {
        e: Box<E>,
        subtract: bool,
        amount: Box<E>,
        unit: DatePart,
        ty: Ty,
    },
    Case {
        whens: Vec<(C, E)>,
        otherwise: Option<Box<E>>,
    },
    Cast {
        e: Box<E>,
        to: CastType,
    },
    Agg(usize),
    Func {
        f: Func,
        args: Vec<E>,
    },
    Trim {
        e: Box<E>,
        chars: Option<Box<E>>,
        leading: bool,
        trailing: bool,
    },
    Scalar(Box<QPlan>),
    Nullif(Box<E>, Box<E>),
    Coalesce(Vec<E>),
    /// An expression several comparisons read, evaluated by each: a
    /// simple CASE's operand, compared by every WHEN.
    Shared(Rc<E>),
}

/// A bound predicate.
#[derive(Clone, Debug)]
pub enum C {
    And(Vec<C>),
    Or(Vec<C>),
    Not(Box<C>),
    Cmp {
        l: E,
        op: CompareOp,
        r: E,
    },
    In {
        l: E,
        list: InList,
        negated: bool,
    },
    Quant {
        l: E,
        op: CompareOp,
        all: bool,
        list: InList,
    },
    IsNull {
        e: E,
        negated: bool,
    },
    IsBool {
        e: E,
        negated: bool,
        what: IsWhat,
    },
    Between {
        e: E,
        low: E,
        high: E,
        negated: bool,
    },
    Like {
        e: E,
        pattern: E,
        negated: bool,
    },
    PartOf {
        part: E,
        whole: E,
        negated: bool,
    },
    Exists {
        q: Box<QPlan>,
        unique: bool,
    },
    False,
}

#[derive(Clone, Debug)]
pub enum InList {
    Values(Vec<E>),
    Query(Box<QPlan>),
}

#[derive(Clone, Debug)]
pub struct AggPlan {
    pub func: Aggregate,
    pub distinct: bool,
    pub arg: Option<E>,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum From {
    Inst(usize),
    Join {
        left: Box<From>,
        kind: JoinKind,
        right: Box<From>,
        on: Option<C>,
    },
}

#[derive(Clone, Debug)]
pub struct OutCol {
    pub expr: E,
    pub ty: Ty,
    /// The alias or column name ORDER BY can name.
    pub name: Option<String>,
    pub container: bool,
    pub aggregate: bool,
}

/// A planned `query_spec`.
#[derive(Clone, Debug)]
pub struct Spec {
    pub insts: Vec<Inst>,
    pub from: Vec<From>,
    pub filter: Option<C>,
    pub group_by: Vec<E>,
    pub aggs: Vec<AggPlan>,
    pub grouped: bool,
    pub having: Option<C>,
    pub cols: Vec<OutCol>,
    pub distinct: bool,
    /// Expressions ORDER BY sorts by that are not output columns.
    pub order_exprs: Vec<E>,
}

#[derive(Clone, Debug)]
pub enum Term {
    Spec(Box<Spec>),
    Query(Box<QPlan>),
}

#[derive(Clone, Copy, Debug)]
pub enum OrderKey {
    Output(usize),
    Hidden(usize),
}

#[derive(Clone, Debug)]
pub struct QPlan {
    pub first: Term,
    pub unions: Vec<(bool, Term)>,
    pub order: Vec<(OrderKey, bool)>,
    pub offset: usize,
    pub fetch: Option<FetchPlan>,
    pub types: Vec<Ty>,
    pub containers: Vec<bool>,
    pub names: Vec<Option<String>>,
}

#[derive(Clone, Copy, Debug)]
pub struct FetchPlan {
    pub count: u64,
    pub percent: bool,
    pub with_ties: bool,
}

/// What a statement does: a query to run (every other statement is checked
/// and refused with error 3 while it is built).
pub enum Plan {
    Query(QPlan),
}

struct Scope {
    insts: Vec<Inst>,
}

/// Whether aggregates may appear where an expression is bound.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggMode {
    /// A select item, HAVING operand or ORDER BY item: the expression may be
    /// an aggregate.
    Top,
    /// Inside an expression.
    Nested,
    /// In `WHERE` (FQL0049).
    Where,
    /// Not in a query with aggregates (ON, GROUP BY, ...): FQL0005.
    None,
}

pub struct Binder<'s> {
    source: &'s mut dyn SqlSource,
    tables: Vec<TableDef>,
    scopes: Vec<Scope>,
    /// The aggregates of the query specs being built, innermost last.
    aggs: Vec<Vec<AggPlan>>,
    /// Column references outside aggregates, per open query spec.
    plain_columns: Vec<Vec<(usize, usize, Option<u32>)>>,
    /// Inside an aggregate's argument (its columns are not plain columns).
    aggregate_depth: usize,
    next_param: usize,
    named: bool,
    dynamic: bool,
}

type R<T> = Result<T, SqlError>;

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

impl<'s> Binder<'s> {
    pub fn new(source: &'s mut dyn SqlSource) -> Binder<'s> {
        let tables = source.tables();
        Binder {
            source,
            tables,
            scopes: Vec::new(),
            aggs: Vec::new(),
            plain_columns: Vec::new(),
            aggregate_depth: 0,
            next_param: 0,
            named: false,
            dynamic: false,
        }
    }

    pub fn statement(&mut self, statement: &Statement) -> R<Plan> {
        match statement {
            Statement::Query(q) => Ok(Plan::Query(self.query(q)?)),
            Statement::Table { pos } => Err(SqlError::fql(1, *pos, &[])),
            Statement::Insert {
                table,
                columns,
                source,
            } => {
                let (_, fields) = self.writable_table(table)?;
                for c in columns {
                    if !fields.iter().any(|f| same_name(&f.name, &c.text)) {
                        return Err(SqlError::fql(7, c.pos, &[&c.text]));
                    }
                }
                let width = if columns.is_empty() {
                    fields.len()
                } else {
                    columns.len()
                };
                if let InsertSource::Values(rows) = source {
                    for (pos, values) in rows {
                        if values.len() != width {
                            return Err(SqlError::fql(14, *pos, &[]));
                        }
                    }
                }
                Err(SqlError::Engine(3))
            }
            Statement::Update {
                table,
                assignments,
                filter,
                cursor,
            } => {
                let (def, fields) = self.writable_table(table)?;
                if let Some(c) = cursor {
                    return Err(SqlError::fql(37, c.pos, &[&c.text]));
                }
                for (c, _) in assignments {
                    if !fields.iter().any(|f| same_name(&f.name, &c.text)) {
                        return Err(SqlError::fql(7, c.pos, &[&c.text]));
                    }
                }
                self.check_filter(def, fields, table, filter)?;
                Err(SqlError::Engine(3))
            }
            Statement::Delete {
                table,
                filter,
                cursor,
            } => {
                let (def, fields) = self.writable_table(table)?;
                if let Some(c) = cursor {
                    return Err(SqlError::fql(37, c.pos, &[&c.text]));
                }
                self.check_filter(def, fields, table, filter)?;
                Err(SqlError::Engine(3))
            }
            Statement::Ddl {
                kind,
                table,
                column,
            } => {
                if let Some(t) = SystemTable::by_name(&table.text) {
                    return Err(SqlError::fql(48, table.pos, &[t.name()]));
                }
                if *kind != DdlKind::CreateTable {
                    let (_, fields) = self.writable_table(table)?;
                    if let Some(c) = column {
                        if !fields.iter().any(|f| same_name(&f.name, &c.text)) {
                            return Err(SqlError::fql(7, c.pos, &[&c.text]));
                        }
                    }
                }
                Err(SqlError::Engine(3))
            }
            Statement::DeclareCursor { query, .. } => {
                self.query(query)?;
                Err(SqlError::Engine(3))
            }
            Statement::Cursor { name } => Err(SqlError::fql(37, name.pos, &[&name.text])),
        }
    }

    fn writable_table(&mut self, name: &Name) -> R<(TableDef, Vec<FieldDef>)> {
        if let Some(t) = SystemTable::by_name(&name.text) {
            return Err(SqlError::fql(48, name.pos, &[t.name()]));
        }
        let def = self
            .tables
            .iter()
            .find(|t| same_name(&t.name, &name.text))
            .cloned()
            .ok_or_else(|| SqlError::fql(2, name.pos, &[&name.text]))?;
        let fields = self.source.fields(&def);
        Ok((def, fields))
    }

    fn check_filter(
        &mut self,
        def: TableDef,
        fields: Vec<FieldDef>,
        name: &Name,
        filter: &Option<Predicate>,
    ) -> R<()> {
        let Some(p) = filter else {
            return Ok(());
        };
        self.scopes.push(Scope {
            insts: vec![Inst {
                name: name.text.clone(),
                src: InstSrc::User { table: def, fields },
            }],
        });
        self.aggs.push(Vec::new());
        self.plain_columns.push(Vec::new());
        let r = self.predicate(p, AggMode::Where);
        self.scopes.pop();
        self.aggs.pop();
        self.plain_columns.pop();
        r.map(|_| ())
    }

    fn query(&mut self, q: &Query) -> R<QPlan> {
        let mut plan = match (&q.body.first, q.body.unions.is_empty()) {
            (QueryTerm::Spec(spec), true) => {
                let (spec, order) = self.spec(spec, &q.order_by)?;
                let mut plan = single(Term::Spec(Box::new(spec)));
                plan.order = order;
                plan
            }
            _ => {
                let mut plan = self.query_expr(&q.body)?;
                for spec in &q.order_by {
                    if let Some(key) = output_order_key(&plan.names, &spec.expr)? {
                        plan.order.push((key, spec.descending));
                    }
                }
                plan
            }
        };
        if let Some(off) = &q.offset {
            plan.offset =
                count(&off.count).ok_or_else(|| SqlError::fql(50, off.pos, &[]))? as usize;
        }
        if let Some(f) = &q.fetch {
            if f.next {
                return Err(SqlError::fql(1, f.pos, &[]));
            }
            let n = match &f.count {
                None => 1,
                Some(c) => count(c)
                    .filter(|n| *n > 0)
                    .ok_or_else(|| SqlError::fql(52, f.pos, &[]))?,
            };
            if f.with_ties && q.order_by.is_empty() {
                return Err(SqlError::fql(51, f.pos, &[]));
            }
            plan.fetch = Some(FetchPlan {
                count: n,
                percent: f.percent,
                with_ties: f.with_ties,
            });
        }
        Ok(plan)
    }

    /// A query expression without ORDER BY (a union, a subquery).
    fn query_expr(&mut self, q: &QueryExpr) -> R<QPlan> {
        stack::guard(|| self.query_expr_inner(q))
    }

    fn query_expr_inner(&mut self, q: &QueryExpr) -> R<QPlan> {
        let first = self.term(&q.first)?;
        let mut plan = single(first);
        for u in &q.unions {
            let term = self.term(&u.term)?;
            let (types, containers, _) = term_columns(&term);
            let pos = u.term.pos();
            if types.len() != plan.types.len() {
                return Err(SqlError::fql(26, pos, &[]));
            }
            for (i, b) in types.iter().enumerate() {
                match union_type(plan.types[i], *b) {
                    Some(t) => plan.types[i] = t,
                    None => return Err(SqlError::fql(27, pos, &[])),
                }
                if !u.all && (plan.containers[i] || containers[i]) {
                    return Err(SqlError::fql(46, pos, &[]));
                }
            }
            plan.unions.push((u.all, term));
        }
        Ok(plan)
    }

    fn term(&mut self, t: &QueryTerm) -> R<Term> {
        match t {
            QueryTerm::Table { pos } => Err(SqlError::fql(1, *pos, &[])),
            QueryTerm::Spec(spec) => Ok(Term::Spec(Box::new(self.spec(spec, &[])?.0))),
            QueryTerm::Paren(inner) => Ok(Term::Query(Box::new(self.query_expr(inner)?))),
        }
    }
}

/// A plan of one term.
fn single(first: Term) -> QPlan {
    let (types, containers, names) = term_columns(&first);
    QPlan {
        first,
        unions: Vec::new(),
        order: Vec::new(),
        offset: 0,
        fetch: None,
        types,
        containers,
        names,
    }
}

/// ORDER BY naming an output column by ordinal or name (after a union).
fn output_order_key(names: &[Option<String>], e: &Expr) -> R<Option<OrderKey>> {
    match &e.kind {
        ExprKind::Literal(l) => {
            let n = ordinal(l, names.len()).ok_or_else(|| ordinal_error(l, e.pos))?;
            Ok(Some(OrderKey::Output(n)))
        }
        ExprKind::Column(ColumnRef {
            table: None,
            column: Some(name),
            repetition: None,
            ..
        }) => names
            .iter()
            .position(|n| n.as_deref().is_some_and(|n| same_name(n, &name.text)))
            .map(|i| Some(OrderKey::Output(i)))
            .ok_or_else(|| SqlError::fql(7, e.pos, &[&name.text])),
        _ => Ok(None),
    }
}

/// The column an ORDER BY literal names (from 0): a whole number from 1
/// to the column count; any other literal is invalid (FQL0012).
fn ordinal(l: &Literal, columns: usize) -> Option<usize> {
    match l {
        Literal::Number(t) if t.chars().all(|c| c.is_ascii_digit()) => {
            let n: usize = t.parse().ok()?;
            (1..=columns).contains(&n).then(|| n - 1)
        }
        _ => None,
    }
}

fn ordinal_error(l: &Literal, pos: Pos) -> SqlError {
    let text = match l {
        Literal::Number(t) | Literal::Text(t) | Literal::Bit(t) | Literal::Hex(t) => t.clone(),
        Literal::Date(t, _) | Literal::Time(t, _) | Literal::Timestamp(t, _) => t.clone(),
        Literal::Null => "NULL".into(),
        Literal::True => "TRUE".into(),
        Literal::False => "FALSE".into(),
    };
    SqlError::fql(12, pos, &[&text])
}

/// An OFFSET or FETCH count: a non-negative integer constant.
fn count(e: &Expr) -> Option<u64> {
    match &e.kind {
        ExprKind::Literal(Literal::Number(t)) if t.chars().all(|c| c.is_ascii_digit()) => {
            t.parse().ok()
        }
        _ => None,
    }
}

fn term_columns(t: &Term) -> (Vec<Ty>, Vec<bool>, Vec<Option<String>>) {
    match t {
        Term::Spec(s) => (
            s.cols.iter().map(|c| c.ty).collect(),
            s.cols.iter().map(|c| c.container).collect(),
            s.cols.iter().map(|c| c.name.clone()).collect(),
        ),
        Term::Query(q) => (q.types.clone(), q.containers.clone(), q.names.clone()),
    }
}

/// The type of a UNION column (7): the same type, or NULL beside any.
fn union_type(a: Ty, b: Ty) -> Option<Ty> {
    match (a, b) {
        _ if a == b => Some(a),
        (Ty::Null | Ty::Param, t) | (t, Ty::Null | Ty::Param) => Some(t),
        (Ty::Bool, Ty::Number) | (Ty::Number, Ty::Bool) => Some(Ty::Number),
        _ => None,
    }
}

fn is_agg(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Aggregate { .. } => true,
        ExprKind::Paren(inner) => is_agg(inner),
        _ => false,
    }
}

fn numeric(t: Ty) -> bool {
    matches!(t, Ty::Number | Ty::Bool)
}

/// The result type of `+` and `-` (5.4, 5.6; calc-values.md 6.1), `None`
/// for operands that do not add.
fn sum_type(l: Ty, r: Ty) -> Option<Ty> {
    use Ty::*;
    let num_or_text = |t: Ty| numeric(t) || t == Text;
    Some(match (l, r) {
        (Container, _) | (_, Container) => return None,
        (Param, t) | (t, Param) => t,
        (Null, t) | (t, Null) => t,
        (Text, Text) => Text,
        (a, b) if num_or_text(a) && num_or_text(b) => Number,
        (Date, b) | (b, Date) if num_or_text(b) => Date,
        (Date, Date) => Number,
        (Date, Time) | (Time, Date) => Timestamp,
        (Date, Timestamp) | (Timestamp, Date) => Time,
        (Time, b) | (b, Time) if num_or_text(b) => Time,
        (Time, Time) => Time,
        (Time, Timestamp) | (Timestamp, Time) => Timestamp,
        (Timestamp, b) | (b, Timestamp) if num_or_text(b) => Timestamp,
        (Timestamp, Timestamp) => Time,
        _ => return None,
    })
}

/// Whether values of two types can be compared (5.6).
fn comparable(l: Ty, r: Ty) -> bool {
    use Ty::*;
    match (l, r) {
        (Param, _) | (_, Param) => true,
        (Container, _) | (_, Container) => false,
        (Null, _) | (_, Null) => true,
        (a, b) if a == b => true,
        (a, b) if numeric(a) && numeric(b) => true,
        (a, Text) | (Text, a) => a.is_temporal(),
        (a, b) => a.is_temporal() && b.is_temporal(),
    }
}

/// Whether two bound operands can be compared: [`comparable`] types,
/// except a date, time or timestamp literal beside text that is not a
/// constant (`Task = DATE '2014-02-17'` is FQL0018, `"Due Date" = Task`
/// and `DATE '2014-02-17' = '2/17/2014'` are not; 5.6).
fn comparable_operands(l: &E, lt: Ty, r: &E, rt: Ty) -> bool {
    let constant = |e: &E| matches!(e, E::Const(_) | E::Param { .. });
    match (lt, rt) {
        (Ty::Text, t) if t.is_temporal() => constant(l) || !constant(r),
        (t, Ty::Text) if t.is_temporal() => constant(r) || !constant(l),
        _ => comparable(lt, rt),
    }
}

/// The stream names `GETAS` accepts (8.1).
const STREAMS: &[&str] = &[
    "FILE", "FNAM", "JPEG", "PNGf", "GIFf", "PDF ", "snd ", "SND ", "SIZE", "MAIN",
];

impl Binder<'_> {
    /// The table a `FROM` name reads.
    fn table(&mut self, name: &Name) -> R<InstSrc> {
        if let Some(sys) = SystemTable::by_name(&name.text) {
            let columns = sys
                .columns()
                .iter()
                .map(|(n, t)| {
                    let ty = match t {
                        SystemColumn::Text => Ty::Text,
                        SystemColumn::Number => Ty::Number,
                    };
                    (n.to_string(), ty)
                })
                .collect();
            let rows = sys.rows(&*self.source);
            return Ok(InstSrc::Rows {
                columns,
                rows: Rc::new(rows),
            });
        }
        if let Some(list) = source::value_list_table(&name.text) {
            let rows = match self.source.value_list_values(list) {
                Some(ValueListValues::Values(values)) => {
                    values.into_iter().map(|v| vec![Value::text(v)]).collect()
                }
                Some(ValueListValues::Unsupported) => {
                    vec![vec![Value::text("<Valuelist Unsupported>")]]
                }
                None => vec![vec![Value::text("<Value List Missing>")]],
            };
            return Ok(InstSrc::Rows {
                columns: vec![("FirstValues".to_string(), Ty::Text)],
                rows: Rc::new(rows),
            });
        }
        let def = self
            .tables
            .iter()
            .find(|t| same_name(&t.name, &name.text))
            .cloned()
            .ok_or_else(|| SqlError::fql(2, name.pos, &[&name.text]))?;
        let fields = self.source.fields(&def);
        Ok(InstSrc::User { table: def, fields })
    }

    fn from(&mut self, t: &TableRef, insts: &mut Vec<Inst>, names: &mut Vec<String>) -> R<From> {
        stack::guard(|| self.table_ref_inner(t, insts, names))
    }

    fn table_ref_inner(
        &mut self,
        t: &TableRef,
        insts: &mut Vec<Inst>,
        names: &mut Vec<String>,
    ) -> R<From> {
        match t {
            TableRef::Table { name, alias } => {
                let src = self.table(name)?;
                let shown = alias.as_ref().unwrap_or(name);
                if names.iter().any(|n| same_name(n, &shown.text)) {
                    return Err(SqlError::fql(3, shown.pos, &[&shown.text]));
                }
                names.push(shown.text.clone());
                insts.push(Inst {
                    name: shown.text.clone(),
                    src,
                });
                Ok(From::Inst(insts.len() - 1))
            }
            TableRef::Join {
                left, kind, right, ..
            } => {
                // The ON conditions are bound once every table is known
                // ([`Self::bind_on`]).
                let left = self.from(left, insts, names)?;
                let right = self.from(right, insts, names)?;
                Ok(From::Join {
                    left: Box::new(left),
                    kind: *kind,
                    right: Box::new(right),
                    on: None,
                })
            }
        }
    }

    /// Binds the `ON` conditions of a planned join tree.
    fn bind_on(&mut self, plan: &mut From, t: &TableRef) -> R<()> {
        stack::guard(|| self.bind_on_inner(plan, t))
    }

    fn bind_on_inner(&mut self, plan: &mut From, t: &TableRef) -> R<()> {
        if let (
            From::Join {
                left, right, on, ..
            },
            TableRef::Join {
                left: tl,
                right: tr,
                on: ton,
                ..
            },
        ) = (plan, t)
        {
            self.bind_on(left, tl)?;
            self.bind_on(right, tr)?;
            if let Some(p) = ton {
                *on = Some(self.predicate(p, AggMode::None)?);
            }
        }
        Ok(())
    }

    fn spec(&mut self, s: &QuerySpec, order_by: &[OrderSpec]) -> R<(Spec, Vec<(OrderKey, bool)>)> {
        let mut insts = Vec::new();
        let mut names = Vec::new();
        let mut from = Vec::new();
        for t in &s.from {
            from.push(self.from(t, &mut insts, &mut names)?);
        }
        self.scopes.push(Scope { insts });
        self.aggs.push(Vec::new());
        self.plain_columns.push(Vec::new());
        let result = self.spec_body(s, order_by, &mut from);
        let scope = self.scopes.pop().expect("scope");
        let aggs = self.aggs.pop().expect("aggregates");
        self.plain_columns.pop();
        let (mut spec, order) = result?;
        spec.insts = scope.insts;
        spec.from = from;
        spec.aggs = aggs;
        Ok((spec, order))
    }

    fn spec_body(
        &mut self,
        s: &QuerySpec,
        order_by: &[OrderSpec],
        from: &mut [From],
    ) -> R<(Spec, Vec<(OrderKey, bool)>)> {
        for (plan, t) in from.iter_mut().zip(&s.from) {
            self.bind_on(plan, t)?;
        }
        let select_start = self.plain_columns.last().expect("spec").len();
        let mut cols = Vec::new();
        // Plain column references per select item, for FQL0025.
        let mut item_columns = Vec::new();
        match &s.select {
            SelectList::Star(_) => {
                let n = self.scopes.last().expect("scope").insts.len();
                for inst in 0..n {
                    self.star_columns(inst, &mut cols)?;
                }
            }
            SelectList::Items(items) => {
                for item in items {
                    let before = self.plain_columns.last().expect("spec").len();
                    match item {
                        SelectItem::TableStar(name) => {
                            let scope = self.scopes.last().expect("scope");
                            let inst = scope
                                .insts
                                .iter()
                                .position(|i| same_name(&i.name, &name.text))
                                .ok_or_else(|| SqlError::fql(8, name.pos, &[&name.text]))?;
                            self.star_columns(inst, &mut cols)?;
                        }
                        SelectItem::GetAs {
                            expr,
                            stream,
                            alias,
                        } => {
                            let (e, ty) = self.expr(expr, AggMode::Top)?;
                            if ty != Ty::Container && ty != Ty::Param {
                                return Err(SqlError::fql(21, expr.pos, &["1", "GETAS"]));
                            }
                            if let Some(st) = stream {
                                if !STREAMS.contains(&st.text.as_str()) {
                                    return Err(SqlError::fql(43, expr.pos, &[&st.text]));
                                }
                            }
                            cols.push(OutCol {
                                expr: e,
                                ty,
                                name: alias.as_ref().map(|a| a.text.clone()),
                                container: true,
                                aggregate: false,
                            });
                        }
                        SelectItem::Expr { expr, alias } => {
                            let (e, ty) = self.expr(expr, AggMode::Top)?;
                            let name =
                                alias.as_ref().map(|a| a.text.clone()).or(match &expr.kind {
                                    ExprKind::Column(ColumnRef {
                                        column: Some(c), ..
                                    }) => Some(c.text.clone()),
                                    _ => None,
                                });
                            cols.push(OutCol {
                                expr: e,
                                ty,
                                name,
                                container: ty == Ty::Container,
                                aggregate: is_agg(expr),
                            });
                        }
                    }
                    let after = self.plain_columns.last().expect("spec").len();
                    item_columns.push(after > before);
                }
            }
        }
        let select_plain = self.plain_columns.last().expect("spec")[select_start..].to_vec();
        let filter = match &s.filter {
            Some(p) => Some(self.predicate(p, AggMode::Where)?),
            None => None,
        };
        let mut group_by = Vec::new();
        let mut group_ids = Vec::new();
        if let Some(g) = &s.group_by {
            for c in &g.columns {
                let before = self.plain_columns.last().expect("spec").len();
                let (e, _) = self.column(c)?;
                let cols_now = self.plain_columns.last_mut().expect("spec");
                group_ids.extend(cols_now.drain(before..));
                group_by.push(e);
            }
        }
        let having_start = self.plain_columns.last().expect("spec").len();
        let having = match &s.having {
            Some(p) => Some(self.predicate(p, AggMode::Top)?),
            None => None,
        };
        let having_plain: Vec<_> =
            self.plain_columns.last().expect("spec")[having_start..].to_vec();
        let grouped = s.group_by.is_some() || !self.aggs.last().expect("spec").is_empty();
        if grouped {
            let ungrouped = select_plain
                .iter()
                .chain(&having_plain)
                .any(|c| !group_ids.contains(c));
            if ungrouped {
                let pos = match (&s.group_by, &s.select) {
                    (Some(g), _) => g.pos,
                    (None, SelectList::Items(items)) => item_pos(&items[0]),
                    (None, SelectList::Star(pos)) => *pos,
                };
                return Err(SqlError::fql(25, pos, &[]));
            }
        }
        let names: Vec<Option<String>> = cols.iter().map(|c| c.name.clone()).collect();
        let mut order = Vec::new();
        let mut order_exprs = Vec::new();
        for o in order_by {
            if let Some(key) = self.order_item(&o.expr, &names, &cols, grouped, &mut order_exprs)? {
                order.push((key, o.descending));
            }
        }
        let spec = Spec {
            insts: Vec::new(),
            from: Vec::new(),
            filter,
            group_by,
            aggs: Vec::new(),
            grouped,
            having,
            cols,
            distinct: s.distinct,
            order_exprs,
        };
        Ok((spec, order))
    }

    fn order_item(
        &mut self,
        e: &Expr,
        names: &[Option<String>],
        cols: &[OutCol],
        grouped: bool,
        hidden: &mut Vec<E>,
    ) -> R<Option<OrderKey>> {
        match &e.kind {
            ExprKind::Literal(l) => {
                let n = ordinal(l, cols.len()).ok_or_else(|| ordinal_error(l, e.pos))?;
                return Ok(Some(OrderKey::Output(n)));
            }
            ExprKind::Column(ColumnRef {
                table: None,
                column: Some(name),
                repetition: None,
                ..
            }) => {
                if let Some(i) = names
                    .iter()
                    .position(|n| n.as_deref().is_some_and(|n| same_name(n, &name.text)))
                {
                    return Ok(Some(OrderKey::Output(i)));
                }
            }
            ExprKind::Param => {
                self.expr(e, AggMode::Top)?;
                return Ok(None);
            }
            _ => {}
        }
        let (bound, _) = self.expr(e, AggMode::Top)?;
        if grouped && is_agg(e) {
            return Ok(None);
        }
        hidden.push(bound);
        Ok(Some(OrderKey::Hidden(hidden.len() - 1)))
    }

    /// The columns of `SELECT *` or `t.*` for the instance `inst` of the
    /// current scope.
    fn star_columns(&mut self, inst: usize, cols: &mut Vec<OutCol>) -> R<()> {
        let scope = self.scopes.last().expect("scope");
        match &scope.insts[inst].src {
            InstSrc::User { table, fields } => {
                if table.base.is_none() {
                    return Err(SqlError::FileMissing);
                }
                for (i, f) in fields.iter().enumerate() {
                    let ty = Ty::of_field(f.ty);
                    cols.push(OutCol {
                        expr: E::Field {
                            depth: 0,
                            inst,
                            field: i,
                            rep: Rep::Const(1),
                        },
                        ty,
                        name: Some(f.name.clone()),
                        container: ty == Ty::Container,
                        aggregate: false,
                    });
                }
            }
            InstSrc::Rows { columns, .. } => {
                for (i, (name, ty)) in columns.iter().enumerate() {
                    cols.push(OutCol {
                        expr: E::Col {
                            depth: 0,
                            inst,
                            col: i,
                        },
                        ty: *ty,
                        name: Some(name.clone()),
                        container: false,
                        aggregate: false,
                    });
                }
            }
        }
        // Plain columns of SELECT * matter for FQL0025 too.
        let n = cols.len();
        let _ = n;
        Ok(())
    }

    fn column(&mut self, c: &ColumnRef) -> R<(E, Ty)> {
        // The scope and instance a qualifier names, innermost first.
        let find_inst = |scopes: &[Scope], name: &Name| -> Option<(usize, usize)> {
            scopes.iter().rev().enumerate().find_map(|(depth, s)| {
                s.insts
                    .iter()
                    .position(|i| same_name(&i.name, &name.text))
                    .map(|i| (depth, i))
            })
        };
        let Some(name) = &c.column else {
            // ROWID.
            let (depth, inst) = match &c.table {
                Some(t) => {
                    find_inst(&self.scopes, t).ok_or_else(|| SqlError::fql(8, t.pos, &[&t.text]))?
                }
                None => {
                    let scope = self.scopes.last().expect("scope");
                    if scope.insts.len() > 1 {
                        return Err(SqlError::fql(24, c.column_pos, &[]));
                    }
                    (0, 0)
                }
            };
            return Ok((E::RowId { depth, inst }, Ty::Number));
        };
        let (depth, inst, slot) = match &c.table {
            Some(t) => {
                let (depth, inst) = find_inst(&self.scopes, t)
                    .ok_or_else(|| SqlError::fql(8, t.pos, &[&t.text]))?;
                let scope = &self.scopes[self.scopes.len() - 1 - depth];
                match slot_of(&scope.insts[inst], &name.text) {
                    Some(slot) => (depth, inst, slot),
                    None => {
                        return Err(SqlError::fql(9, c.pos, &[&t.text, &name.text]));
                    }
                }
            }
            None => {
                let mut found = None;
                for (depth, scope) in self.scopes.iter().rev().enumerate() {
                    let hits: Vec<(usize, Slot)> = scope
                        .insts
                        .iter()
                        .enumerate()
                        .filter_map(|(i, inst)| slot_of(inst, &name.text).map(|s| (i, s)))
                        .collect();
                    match hits.len() {
                        0 => continue,
                        1 => {
                            found = Some((depth, hits[0].0, hits[0].1));
                            break;
                        }
                        _ => {
                            // ROWMODID is every table's.
                            if hits.iter().all(|(_, s)| matches!(s, Slot::ModId)) {
                                return Err(SqlError::fql(24, c.column_pos, &[]));
                            }
                            return Err(SqlError::fql(6, c.column_pos, &[&name.text]));
                        }
                    }
                }
                found.ok_or_else(|| SqlError::fql(7, c.column_pos, &[&name.text]))?
            }
        };
        let (e, ty) = match slot {
            Slot::ModId => (E::RowModId { depth, inst }, Ty::Number),
            Slot::Col(col, ty) => (E::Col { depth, inst, col }, ty),
            Slot::Field(field, ty, _reps) => {
                let rep = match &c.repetition {
                    None => Rep::Const(1),
                    Some(r) => self.repetition(r)?,
                };
                if depth == 0 && self.aggregate_depth == 0 {
                    let rep_key = match rep {
                        Rep::Const(n) => Some(n),
                        Rep::Expr(_) => None,
                    };
                    if let Some(cols) = self.plain_columns.last_mut() {
                        cols.push((inst, field, rep_key));
                    }
                }
                (
                    E::Field {
                        depth,
                        inst,
                        field,
                        rep,
                    },
                    ty,
                )
            }
        };
        Ok((e, ty))
    }

    /// A field subscript (4.5).
    fn repetition(&mut self, r: &Expr) -> R<Rep> {
        if let ExprKind::Negate(inner) = &r.kind {
            if let ExprKind::Literal(Literal::Number(_)) = inner.kind {
                return Err(SqlError::fql(28, r.pos, &["32000"]));
            }
        }
        match &r.kind {
            ExprKind::Literal(Literal::Number(t)) => {
                let n = Number::from_literal(t, '.')
                    .map(|n| n.int().as_long_long())
                    .unwrap_or(0);
                if n < 1 {
                    return Err(SqlError::fql(28, r.pos, &["32000"]));
                }
                Ok(Rep::Const(n.min(i64::from(u32::MAX)) as u32))
            }
            // FileMaker leaves the placeholder unfilled here.
            ExprKind::Literal(Literal::Text(_)) | ExprKind::Param => {
                Err(SqlError::fql(28, r.pos, &["\u{7f}0"]))
            }
            _ => {
                let (e, ty) = self.expr(r, AggMode::Nested)?;
                if !numeric(ty) && ty != Ty::Null {
                    return Err(SqlError::fql(28, r.pos, &["\u{7f}0"]));
                }
                Ok(Rep::Expr(Box::new(e)))
            }
        }
    }

    fn param(&mut self, pos: Pos, named: bool) -> R<()> {
        if named {
            if self.dynamic {
                return Err(SqlError::fql(32, pos, &[]));
            }
            self.named = true;
        } else {
            if self.named {
                return Err(SqlError::fql(32, pos, &[]));
            }
            self.dynamic = true;
        }
        Ok(())
    }

    fn nested(mode: AggMode) -> AggMode {
        match mode {
            AggMode::Top | AggMode::Nested => AggMode::Nested,
            other => other,
        }
    }

    /// FQL0005 for an aggregate inside the expression at `pos`.
    fn no_agg(&self, child: &Expr, mode: AggMode, pos: Pos) -> R<()> {
        if is_agg(child) && mode != AggMode::Where {
            return Err(SqlError::fql(5, pos, &[]));
        }
        Ok(())
    }

    pub(crate) fn expr(&mut self, e: &Expr, mode: AggMode) -> R<(E, Ty)> {
        stack::guard(|| self.expr_inner(e, mode))
    }

    fn expr_inner(&mut self, e: &Expr, mode: AggMode) -> R<(E, Ty)> {
        let inner = Self::nested(mode);
        match &e.kind {
            ExprKind::Column(c) => self.column(c),
            ExprKind::Literal(l) => literal(l, e.pos),
            ExprKind::Param => {
                self.param(e.pos, false)?;
                let index = self.next_param;
                self.next_param += 1;
                Ok((
                    E::Param {
                        index,
                        mode: PMode::Raw,
                    },
                    Ty::Param,
                ))
            }
            ExprKind::Named(_) => {
                self.param(e.pos, true)?;
                Ok((E::Const(Val::Null), Ty::Param))
            }
            ExprKind::Default => Ok((E::Const(Val::Null), Ty::Null)),
            ExprKind::Negate(x) | ExprKind::Plus(x) => {
                self.no_agg(x, mode, e.pos)?;
                let (b, ty) = self.expr(x, inner)?;
                if !(numeric(ty) || ty == Ty::Param || ty == Ty::Null) {
                    return Err(SqlError::fql(18, e.pos, &[]));
                }
                let b = param_mode(b, PMode::Compare(Ty::Number));
                let ty = if ty == Ty::Bool { Ty::Number } else { ty };
                if matches!(e.kind, ExprKind::Negate(_)) {
                    Ok((
                        E::Neg(Box::new(b)),
                        if ty == Ty::Param { Ty::Number } else { ty },
                    ))
                } else {
                    Ok((b, if ty == Ty::Param { Ty::Number } else { ty }))
                }
            }
            ExprKind::Binary { op, left, right } => {
                self.no_agg(left, mode, e.pos)?;
                self.no_agg(right, mode, e.pos)?;
                let (l, lt) = self.expr(left, inner)?;
                let (r, rt) = self.expr(right, inner)?;
                if lt == Ty::Param && rt == Ty::Param {
                    return Err(SqlError::fql(31, e.pos, &[]));
                }
                match op {
                    BinOp::Concat => {
                        if !matches!(lt, Ty::Text | Ty::Param | Ty::Null) {
                            return Err(SqlError::fql(18, left.pos, &[]));
                        }
                        Ok((E::Concat(Box::new(l), Box::new(r)), Ty::Text))
                    }
                    BinOp::Add | BinOp::Subtract => {
                        let ty = sum_type(lt, rt).ok_or_else(|| SqlError::fql(18, e.pos, &[]))?;
                        Ok((
                            E::Arith {
                                op: *op,
                                l: Box::new(l),
                                r: Box::new(r),
                                ty,
                            },
                            ty,
                        ))
                    }
                    _ => {
                        let ok = |t: Ty| numeric(t) || t == Ty::Param;
                        if !ok(lt) {
                            return Err(SqlError::fql(18, left.pos, &[]));
                        }
                        if !ok(rt) {
                            return Err(SqlError::fql(18, left.pos, &[]));
                        }
                        let num = PMode::Compare(Ty::Number);
                        Ok((
                            E::Arith {
                                op: *op,
                                l: Box::new(param_mode(l, num)),
                                r: Box::new(param_mode(r, num)),
                                ty: Ty::Number,
                            },
                            Ty::Number,
                        ))
                    }
                }
            }
            ExprKind::Interval {
                expr,
                subtract,
                amount,
                unit,
            } => {
                let (x, ty) = self.expr(expr, inner)?;
                let (n, nt) = self.expr(amount, inner)?;
                if !(ty.is_temporal() || ty == Ty::Null || ty == Ty::Param) {
                    return Err(SqlError::fql(18, e.pos, &[]));
                }
                if !(numeric(nt) || matches!(nt, Ty::Text | Ty::Null | Ty::Param)) {
                    return Err(SqlError::fql(18, e.pos, &[]));
                }
                let time_unit =
                    matches!(unit, DatePart::Hour | DatePart::Minute | DatePart::Second);
                let ty = if ty == Ty::Date && time_unit {
                    Ty::Timestamp
                } else {
                    ty
                };
                Ok((
                    E::Interval {
                        e: Box::new(x),
                        subtract: *subtract,
                        amount: Box::new(n),
                        unit: *unit,
                        ty,
                    },
                    ty,
                ))
            }
            ExprKind::Case {
                operand,
                whens,
                otherwise,
            } => self.case(e, operand.as_deref(), whens, otherwise.as_ref(), inner),
            ExprKind::Cast { expr, ty } => {
                self.no_agg(expr, mode, e.pos)?;
                let (x, from) = self.expr(expr, inner)?;
                let to = cast_type(*ty);
                let ok = match (from, *ty) {
                    (_, CastType::Boolean | CastType::Binary) => false,
                    (Ty::Null | Ty::Param, _) => true,
                    (Ty::Container, CastType::Varchar) => true,
                    (Ty::Container, _) => false,
                    (Ty::Number | Ty::Text, _) => true,
                    (Ty::Bool, CastType::Varchar | CastType::Int | CastType::Decimal) => true,
                    (Ty::Date, CastType::Date) => true,
                    (Ty::Date, CastType::Varchar | CastType::Int | CastType::Decimal) => true,
                    (Ty::Date, CastType::Time | CastType::Timestamp) => true,
                    (Ty::Time, CastType::Varchar | CastType::Int | CastType::Decimal) => true,
                    (Ty::Time, CastType::Time | CastType::Timestamp) => true,
                    (Ty::Timestamp, CastType::Varchar | CastType::Date | CastType::Time) => true,
                    (Ty::Timestamp, CastType::Timestamp) => true,
                    _ => false,
                };
                if !ok {
                    return Err(SqlError::fql(23, expr.pos, &[]));
                }
                Ok((
                    E::Cast {
                        e: Box::new(x),
                        to: *ty,
                    },
                    to,
                ))
            }
            ExprKind::Aggregate {
                func,
                distinct,
                arg,
            } => {
                match mode {
                    AggMode::Where => return Err(SqlError::fql(49, e.pos, &[])),
                    AggMode::Nested | AggMode::None => {
                        return Err(SqlError::fql(5, e.pos, &[]));
                    }
                    AggMode::Top => {}
                }
                let (arg, ty) = match arg {
                    None => (None, Ty::Number),
                    Some(a) => {
                        self.no_agg(a, AggMode::Nested, e.pos)?;
                        self.aggregate_depth += 1;
                        let r = self.expr(a, AggMode::Nested);
                        self.aggregate_depth -= 1;
                        let (b, ty) = r?;
                        (Some(b), ty)
                    }
                };
                let name = match func {
                    Aggregate::Avg => "AVG",
                    Aggregate::Sum => "SUM",
                    _ => "",
                };
                if matches!(func, Aggregate::Avg | Aggregate::Sum)
                    && !(numeric(ty) || ty == Ty::Param || ty == Ty::Null)
                {
                    return Err(SqlError::fql(21, e.pos, &["1", name]));
                }
                let result = match func {
                    Aggregate::Min | Aggregate::Max => {
                        if ty == Ty::Bool {
                            Ty::Number
                        } else {
                            ty
                        }
                    }
                    _ => Ty::Number,
                };
                let aggs = self.aggs.last_mut().expect("spec");
                aggs.push(AggPlan {
                    func: *func,
                    distinct: *distinct,
                    arg,
                });
                Ok((E::Agg(aggs.len() - 1), result))
            }
            ExprKind::Function { name, args, parens } => {
                self.function(e, name, args, *parens, mode)
            }
            ExprKind::Extract { part, expr } => {
                self.no_agg(expr, mode, e.pos)?;
                let (x, ty) = self.expr(expr, inner)?;
                let f = match part {
                    DatePart::Day => Func::Day,
                    DatePart::Month => Func::Month,
                    DatePart::Year => Func::Year,
                    DatePart::Hour => Func::Hour,
                    DatePart::Minute => Func::Minute,
                    DatePart::Second => Func::Second,
                };
                if !func::accepts(f.params().0[0], ty) {
                    return Err(SqlError::fql(18, e.pos, &[]));
                }
                Ok((E::Func { f, args: vec![x] }, Ty::Number))
            }
            ExprKind::Trim { side, chars, expr } => {
                let chars = match chars {
                    Some(c) => {
                        let (b, ty) = self.expr(c, inner)?;
                        if !matches!(ty, Ty::Text | Ty::Null | Ty::Param) {
                            return Err(SqlError::fql(21, e.pos, &["1", "TRIM"]));
                        }
                        Some(Box::new(param_mode(b, PMode::Convert(Ty::Text))))
                    }
                    None => None,
                };
                let (x, ty) = self.expr(expr, inner)?;
                if !matches!(ty, Ty::Text | Ty::Null | Ty::Param) {
                    return Err(SqlError::fql(21, e.pos, &["1", "TRIM"]));
                }
                let side = side.unwrap_or(TrimSide::Both);
                Ok((
                    E::Trim {
                        e: Box::new(param_mode(x, PMode::Convert(Ty::Text))),
                        chars,
                        leading: side != TrimSide::Trailing,
                        trailing: side != TrimSide::Leading,
                    },
                    Ty::Text,
                ))
            }
            ExprKind::Subquery(q) => {
                let plan = self.query_expr(q)?;
                if plan.types.len() != 1 {
                    return Err(SqlError::fql(22, q.first.pos(), &[]));
                }
                let ty = plan.types[0];
                Ok((E::Scalar(Box::new(plan)), ty))
            }
            ExprKind::Nullif(a, b) => {
                let (x, xt) = self.expr(a, inner)?;
                let (y, yt) = self.expr(b, inner)?;
                if !comparable(xt, yt)
                    || (xt != yt
                        && !matches!(
                            (xt, yt),
                            (Ty::Null | Ty::Param, _) | (_, Ty::Null | Ty::Param)
                        ))
                {
                    return Err(SqlError::fql(18, b.pos, &[]));
                }
                Ok((
                    E::Nullif(
                        Box::new(param_mode(x, PMode::Compare(yt))),
                        Box::new(param_mode(y, PMode::Compare(xt))),
                    ),
                    if xt == Ty::Param { yt } else { xt },
                ))
            }
            ExprKind::Coalesce(args) => {
                if args.len() < 2 {
                    return Err(SqlError::fql(20, e.pos, &["COALESCE"]));
                }
                let mut bound = Vec::new();
                let mut ty = Ty::Null;
                for a in args {
                    let Arg::Value(v) = a else {
                        return Err(SqlError::fql(18, e.pos, &[]));
                    };
                    self.no_agg(v, mode, e.pos)?;
                    let (b, t) = self.expr(v, inner)?;
                    ty = match (ty, t) {
                        (Ty::Null | Ty::Param, t) => t,
                        (ty, Ty::Null | Ty::Param) => ty,
                        (a, b) if a == b => a,
                        (a, b) if numeric(a) && numeric(b) => Ty::Number,
                        _ => return Err(SqlError::fql(18, e.pos, &[])),
                    };
                    bound.push(b);
                }
                let bound = bound
                    .into_iter()
                    .map(|b| param_mode(b, PMode::Convert(ty)))
                    .collect();
                Ok((E::Coalesce(bound), ty))
            }
            ExprKind::Paren(x) => self.expr(x, mode),
        }
    }

    fn case(
        &mut self,
        e: &Expr,
        operand: Option<&Expr>,
        whens: &[(Arg, Expr)],
        otherwise: Option<&(Pos, Option<Box<Expr>>)>,
        mode: AggMode,
    ) -> R<(E, Ty)> {
        let operand = match operand {
            Some(x) => {
                let (x, xt) = self.expr(x, mode)?;
                // Every WHEN compares the operand. A parameter takes the
                // type of each WHEN's value (5.7), anything else is shared
                // rather than copied: a copy of a deep operand for each
                // WHEN took time and memory, and stack for each level.
                let x = match x {
                    E::Param { .. } => x,
                    other => E::Shared(Rc::new(other)),
                };
                Some((x, xt))
            }
            None => None,
        };
        let mut bound = Vec::new();
        let mut ty = Ty::Null;
        for (cond, value) in whens {
            let c = match (cond, &operand) {
                (Arg::Predicate(p), _) => self.predicate(p, mode)?,
                (Arg::Value(v), Some((x, xt))) => {
                    let (y, yt) = self.expr(v, mode)?;
                    if !comparable(*xt, yt) {
                        return Err(SqlError::fql(18, v.pos, &[]));
                    }
                    C::Cmp {
                        l: param_mode(x.clone(), PMode::Compare(yt)),
                        op: CompareOp::Eq,
                        r: param_mode(y, PMode::Compare(*xt)),
                    }
                }
                (Arg::Value(v), None) => {
                    let (y, _) = self.expr(v, mode)?;
                    C::IsBool {
                        e: y,
                        negated: false,
                        what: IsWhat::True,
                    }
                }
            };
            let (r, rt) = self.expr(value, mode)?;
            ty = merge_case_type(ty, rt).ok_or_else(|| SqlError::fql(18, value.pos, &[]))?;
            bound.push((c, r));
        }
        let mut other = None;
        if let Some((pos, Some(v))) = otherwise {
            let (r, rt) = self.expr(v, mode)?;
            ty = merge_case_type(ty, rt).ok_or_else(|| SqlError::fql(18, *pos, &[]))?;
            other = Some(Box::new(r));
        }
        if ty == Ty::Null {
            let pos = otherwise.map_or(e.pos, |(p, _)| *p);
            return Err(SqlError::fql(19, pos, &[]));
        }
        Ok((
            E::Case {
                whens: bound,
                otherwise: other,
            },
            ty,
        ))
    }

    fn function(
        &mut self,
        e: &Expr,
        name: &Name,
        args: &[Arg],
        parens: bool,
        mode: AggMode,
    ) -> R<(E, Ty)> {
        let inner = Self::nested(mode);
        let keyword = crate::parse::lex::keyword(&name.text).is_some();
        if !keyword && !Func::in_function_table(&name.text) {
            return Err(SqlError::fql(30, name.pos, &[&name.text]));
        }
        let f =
            Func::by_name(&name.text).ok_or_else(|| SqlError::fql(30, name.pos, &[&name.text]))?;
        let _ = parens;
        let (required, optional) = f.params();
        let shown = name.text.to_ascii_uppercase();
        if args.len() < required.len() || args.len() > required.len() + optional.len() {
            return Err(SqlError::fql(20, name.pos, &[&shown]));
        }
        let params: Vec<func::Param> = required.iter().chain(optional).copied().collect();
        let mut bound = Vec::new();
        for (i, (a, p)) in args.iter().zip(&params).enumerate() {
            let number = (i + 1).to_string();
            let Arg::Value(v) = a else {
                return Err(SqlError::fql(21, name.pos, &[&number, &shown]));
            };
            self.no_agg(v, mode, e.pos)?;
            let (b, ty) = self.expr(v, inner)?;
            if !func::accepts(*p, ty) {
                return Err(SqlError::fql(21, name.pos, &[&number, &shown]));
            }
            let b = match p {
                func::Param::Number => param_mode(b, PMode::Convert(Ty::Number)),
                func::Param::Text => param_mode(b, PMode::Convert(Ty::Text)),
                _ => b,
            };
            bound.push(b);
        }
        Ok((E::Func { f, args: bound }, f.result()))
    }

    fn values(&mut self, list: &ListOrQuery, mode: AggMode) -> R<(InList, Vec<Ty>, bool)> {
        match list {
            ListOrQuery::Query(q) => self.subquery_values(q),
            ListOrQuery::List(items) => {
                // A list of one subquery is the subquery.
                if let [Arg::Value(Expr {
                    kind: ExprKind::Subquery(q),
                    ..
                })] = items.as_slice()
                {
                    return self.subquery_values(q);
                }
                let mut bound = Vec::new();
                let mut types = Vec::new();
                let mut predicate = false;
                for item in items {
                    match item {
                        Arg::Value(v) => {
                            let (b, t) = self.expr(v, mode)?;
                            bound.push(b);
                            types.push(t);
                        }
                        Arg::Predicate(_) => predicate = true,
                    }
                }
                Ok((InList::Values(bound), types, !predicate))
            }
        }
    }

    /// The values of a one-column subquery.
    fn subquery_values(&mut self, q: &QueryExpr) -> R<(InList, Vec<Ty>, bool)> {
        let plan = self.query_expr(q)?;
        if plan.types.len() != 1 {
            return Err(SqlError::fql(22, q.first.pos(), &[]));
        }
        let types = plan.types.clone();
        Ok((InList::Query(Box::new(plan)), types, true))
    }

    pub(crate) fn predicate(&mut self, p: &Predicate, mode: AggMode) -> R<C> {
        stack::guard(|| self.predicate_inner(p, mode))
    }

    fn predicate_inner(&mut self, p: &Predicate, mode: AggMode) -> R<C> {
        match p {
            Predicate::Or(items) => Ok(C::Or(
                items
                    .iter()
                    .map(|i| self.predicate(i, mode))
                    .collect::<R<_>>()?,
            )),
            Predicate::And(items) => Ok(C::And(
                items
                    .iter()
                    .map(|i| self.predicate(i, mode))
                    .collect::<R<_>>()?,
            )),
            Predicate::Not(inner) => Ok(C::Not(Box::new(self.predicate(inner, mode)?))),
            Predicate::Compare {
                left,
                op,
                right,
                op_pos,
            } => {
                let (l, lt) = self.expr(left, mode)?;
                let (r, rt) = self.expr(right, mode)?;
                if lt == Ty::Param && rt == Ty::Param {
                    return Err(SqlError::fql(31, *op_pos, &[]));
                }
                if !comparable_operands(&l, lt, &r, rt) {
                    return Err(SqlError::fql(18, *op_pos, &[]));
                }
                Ok(C::Cmp {
                    l: param_mode(l, PMode::Compare(rt)),
                    op: *op,
                    r: param_mode(r, PMode::Compare(lt)),
                })
            }
            Predicate::Quantified {
                left,
                op,
                quantifier,
                list,
                op_pos,
            } => {
                let (l, lt) = self.expr(left, mode)?;
                let (list, types, ok) = self.values(list, mode)?;
                if !ok || types.iter().any(|t| *t == Ty::Null || !comparable(lt, *t)) {
                    return Err(SqlError::fql(17, *op_pos, &[]));
                }
                let first = types.first().copied().unwrap_or(Ty::Null);
                Ok(C::Quant {
                    l: param_mode(l, PMode::Compare(first)),
                    op: *op,
                    all: *quantifier == Quantifier::All,
                    list: in_list_modes(list, lt),
                })
            }
            Predicate::In {
                left,
                negated,
                list,
                pos,
            } => {
                let (l, lt) = self.expr(left, mode)?;
                let (list, types, ok) = self.values(list, mode)?;
                if !ok || types.iter().any(|t| *t == Ty::Null || !comparable(lt, *t)) {
                    return Err(SqlError::fql(17, *pos, &[]));
                }
                if lt == Ty::Param && types.iter().all(|t| *t == Ty::Param) {
                    return Err(SqlError::fql(31, *pos, &[]));
                }
                let first = types
                    .iter()
                    .copied()
                    .find(|t| *t != Ty::Param)
                    .unwrap_or(Ty::Null);
                Ok(C::In {
                    l: param_mode(l, PMode::Compare(first)),
                    list: in_list_modes(list, lt),
                    negated: *negated,
                })
            }
            Predicate::Is {
                expr,
                negated,
                what,
                ..
            } => {
                let (e, _) = self.expr(expr, mode)?;
                Ok(match what {
                    IsWhat::Null | IsWhat::Unknown => C::IsNull {
                        e,
                        negated: *negated,
                    },
                    _ => C::IsBool {
                        e,
                        negated: *negated,
                        what: *what,
                    },
                })
            }
            Predicate::Between {
                expr,
                negated,
                low,
                high,
                pos,
            } => {
                let (e, et) = self.expr(expr, mode)?;
                let (lo, lot) = self.expr(low, mode)?;
                let (hi, hit) = self.expr(high, mode)?;
                if !comparable_operands(&e, et, &lo, lot) || !comparable_operands(&e, et, &hi, hit)
                {
                    return Err(SqlError::fql(18, *pos, &[]));
                }
                if et == Ty::Param && (lot == Ty::Param || hit == Ty::Param) {
                    return Err(SqlError::fql(31, *pos, &[]));
                }
                let other = if lot != Ty::Param { lot } else { hit };
                Ok(C::Between {
                    e: param_mode(e, PMode::Compare(other)),
                    low: param_mode(lo, PMode::Compare(et)),
                    high: param_mode(hi, PMode::Compare(et)),
                    negated: *negated,
                })
            }
            Predicate::Like {
                expr,
                negated,
                pattern,
                pos,
            } => {
                let (e, et) = self.expr(expr, mode)?;
                let (pt_e, pt) = self.expr(pattern, mode)?;
                let text = |t: Ty| matches!(t, Ty::Text | Ty::Param | Ty::Null);
                if !text(et) || !text(pt) {
                    return Err(SqlError::fql(18, *pos, &[]));
                }
                if et == Ty::Param && pt == Ty::Param {
                    return Err(SqlError::fql(31, *pos, &[]));
                }
                Ok(C::Like {
                    e: param_mode(e, PMode::Compare(Ty::Text)),
                    pattern: param_mode(pt_e, PMode::Compare(Ty::Text)),
                    negated: *negated,
                })
            }
            Predicate::PartOf {
                part,
                negated,
                whole,
                pos,
            } => {
                let (a, at) = self.expr(part, mode)?;
                let (b, bt) = self.expr(whole, mode)?;
                let text = |t: Ty| matches!(t, Ty::Text | Ty::Param | Ty::Null);
                if !text(at) || !text(bt) {
                    return Err(SqlError::fql(18, *pos, &[]));
                }
                Ok(C::PartOf {
                    part: param_mode(a, PMode::Compare(Ty::Text)),
                    whole: param_mode(b, PMode::Compare(Ty::Text)),
                    negated: *negated,
                })
            }
            Predicate::Interval { expr, .. } => {
                self.expr(expr, mode)?;
                Ok(C::False)
            }
            Predicate::Exists { unique, query, .. } => Ok(C::Exists {
                q: Box::new(self.query_expr(query)?),
                unique: *unique,
            }),
        }
    }
}

fn item_pos(item: &SelectItem) -> Pos {
    match item {
        SelectItem::TableStar(n) => n.pos,
        SelectItem::GetAs { expr, .. } | SelectItem::Expr { expr, .. } => expr.pos,
    }
}

/// A column of an instance by name.
#[derive(Clone, Copy, Debug)]
enum Slot {
    Field(usize, Ty, u32),
    Col(usize, Ty),
    ModId,
}

fn slot_of(inst: &Inst, name: &str) -> Option<Slot> {
    match &inst.src {
        InstSrc::User { fields, .. } => fields
            .iter()
            .position(|f| same_name(&f.name, name))
            .map(|i| {
                let f = &fields[i];
                let _ = FieldClass::Normal;
                Slot::Field(i, Ty::of_field(f.ty), f.repetitions)
            })
            .or_else(|| same_name(name, "ROWMODID").then_some(Slot::ModId)),
        InstSrc::Rows { columns, .. } => columns
            .iter()
            .position(|(n, _)| same_name(n, name))
            .map(|i| Slot::Col(i, columns[i].1))
            .or_else(|| same_name(name, "ROWMODID").then_some(Slot::ModId)),
    }
}

/// A `?` expression read in `mode`; other expressions unchanged.
fn param_mode(e: E, mode: PMode) -> E {
    match e {
        E::Param { index, .. } => match mode {
            PMode::Compare(Ty::Param | Ty::Null) | PMode::Convert(Ty::Param | Ty::Null) => {
                E::Param {
                    index,
                    mode: PMode::Raw,
                }
            }
            mode => E::Param { index, mode },
        },
        other => other,
    }
}

fn in_list_modes(list: InList, left: Ty) -> InList {
    match list {
        InList::Values(v) => InList::Values(
            v.into_iter()
                .map(|e| param_mode(e, PMode::Compare(left)))
                .collect(),
        ),
        q => q,
    }
}

fn merge_case_type(acc: Ty, t: Ty) -> Option<Ty> {
    Some(match (acc, t) {
        (Ty::Null, t) => t,
        (acc, Ty::Null) => acc,
        (Ty::Param, t) => t,
        (acc, Ty::Param) => acc,
        (a, b) if a == b => a,
        (a, b) if numeric(a) && numeric(b) => Ty::Number,
        _ => return None,
    })
}

fn cast_type(t: CastType) -> Ty {
    match t {
        CastType::Int | CastType::Decimal | CastType::Boolean => Ty::Number,
        CastType::Date => Ty::Date,
        CastType::Time => Ty::Time,
        CastType::Timestamp => Ty::Timestamp,
        CastType::Varchar => Ty::Text,
        CastType::Binary => Ty::Container,
    }
}

fn literal(l: &Literal, pos: Pos) -> R<(E, Ty)> {
    let c = |v: Val, t: Ty| Ok((E::Const(v), t));
    match l {
        Literal::Number(t) => {
            let n = Number::from_literal(t, '.').map_err(|_| SqlError::syntax(pos))?;
            c(Val::Number(n), Ty::Number)
        }
        Literal::Text(s) => c(Val::text(s.as_str()), Ty::Text),
        Literal::Date(s, p) => match func::parse_iso_date(s) {
            Some(d) => c(Val::Date(d), Ty::Date),
            None => Err(SqlError::fql(10, *p, &[s])),
        },
        Literal::Time(s, p) => match func::parse_iso_time(s) {
            Some(t) => c(Val::Time(t), Ty::Time),
            None => Err(SqlError::fql(10, *p, &[s])),
        },
        Literal::Timestamp(s, p) => match func::parse_iso_timestamp(s) {
            Some(t) => c(Val::Timestamp(t), Ty::Timestamp),
            None => Err(SqlError::fql(10, *p, &[s])),
        },
        Literal::Bit(raw) => Err(SqlError::fql(45, pos, &[raw])),
        Literal::Hex(raw) => {
            let digits = &raw[2..raw.len() - 1];
            if digits.len() % 2 == 1 {
                return Err(SqlError::fql(45, pos, &[raw]));
            }
            c(Val::text(digits), Ty::Text)
        }
        Literal::Null => c(Val::Null, Ty::Null),
        Literal::True => c(Val::Bool(true), Ty::Bool),
        Literal::False => c(Val::Bool(false), Ty::Bool),
    }
}
