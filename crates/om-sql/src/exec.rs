//! Running a plan (execute-sql.md 6, 7, 10): nested loops over the `FROM`
//! tables in creation order, joins, `WHERE`, groups and aggregates,
//! `DISTINCT` and `UNION` by the sort key, `ORDER BY`, `OFFSET` and
//! `FETCH`, and the three-valued predicates over SQL values.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use om_calc::{ops, Ctx, Date, Locale, Number, Time, TimeStyle, Timestamp, Value};

use crate::error::SqlError;
use crate::func;
use crate::parse::ast::{Aggregate, BinOp, CastType, CompareOp, DatePart, IsWhat, JoinKind};
use crate::plan::{AggPlan, From, InList, InstSrc, OrderKey, PMode, QPlan, Rep, Spec, Term, C, E};
use crate::source::{Cursor, SqlSource};
use crate::stack;
use crate::value::{self, compare, keys_cmp, Key, Ty, Val};

type R<T> = Result<T, SqlError>;

/// How many rows the executor visits between two
/// [`SqlSource::interrupted`] checks.
const INTERRUPT_CHECK: u32 = 256;

/// FQL's engine error 1, `Operation aborted`: a query its source
/// interrupted.
const ABORTED: i32 = 1;
/// FQL's engine error 2, `Memory allocation failure`: an intermediate
/// result larger than the source's [`SqlSource::memory_budget`].
const MEMORY: i32 = 2;

/// A row of a `FROM`: per table instance, the index of its record or row
/// (`None` in the null-supplied side of a `LEFT JOIN`).
type Row = Vec<Option<usize>>;

/// The bytes `rows` rows of `width` instances take: each row's vector and
/// its slots.
fn row_bytes(rows: usize, width: usize) -> usize {
    let slot = std::mem::size_of::<Option<usize>>();
    rows.saturating_mul(std::mem::size_of::<Row>() + width.saturating_mul(slot))
}

struct Frame<'p> {
    spec: &'p Spec,
    /// Per instance, its record ids (user tables) or row count: the
    /// candidates its rows come from.
    records: Vec<Rc<Vec<u64>>>,
    /// Per instance, every record before its conditions narrowed it: the
    /// found set calculations see on a table other than the first of a
    /// join (12, S3).
    all: Vec<Rc<Vec<u64>>>,
    row: Row,
    aggs: Vec<Val>,
}

/// An output row with the values of the ORDER BY expressions that are not
/// output columns.
struct OutRow {
    vals: Vec<Val>,
    hidden: Vec<Val>,
    /// The record a row of one ungrouped table comes from: the summaries
    /// of the rows a query keeps are read again over those records
    /// ([`Exec::cut_summaries`]).
    record: Option<u64>,
}

pub struct Exec<'s, 'p> {
    source: &'s mut dyn SqlSource,
    args: &'s [Value],
    locale: Rc<Locale>,
    today: Date,
    now: Timestamp,
    user: String,
    frames: Vec<Frame<'p>>,
    records: HashMap<u64, Rc<Vec<u64>>>,
    /// Rows visited since the source was last asked whether to stop.
    visited: u32,
    /// The source's [`SqlSource::memory_budget`].
    budget: usize,
}

impl<'s, 'p> Exec<'s, 'p> {
    pub fn new(source: &'s mut dyn SqlSource, args: &'s [Value]) -> Exec<'s, 'p> {
        let locale = Rc::new(source.locale().clone());
        let now = source.now();
        let user = source.user_name();
        let budget = source.memory_budget();
        Exec {
            source,
            args,
            locale,
            today: now.date(),
            now,
            user,
            frames: Vec::new(),
            records: HashMap::new(),
            visited: 0,
            budget,
        }
    }

    fn cx_parts(&self) -> (Rc<Locale>, Date) {
        (self.locale.clone(), self.today)
    }

    /// Count one row visited; every [`INTERRUPT_CHECK`] rows, error 1 when
    /// the source asks the query to stop.
    fn visit(&mut self) -> R<()> {
        self.visited += 1;
        if self.visited < INTERRUPT_CHECK {
            return Ok(());
        }
        self.visited = 0;
        if self.source.interrupted() {
            return Err(SqlError::Engine(ABORTED));
        }
        Ok(())
    }

    /// One more row in an intermediate result that holds `rows` rows of
    /// `width` instances: error 2 once they take more than the budget.
    fn hold(&self, rows: usize, width: usize) -> R<()> {
        if row_bytes(rows, width) > self.budget {
            return Err(SqlError::Engine(MEMORY));
        }
        Ok(())
    }

    /// The rows of a query.
    pub fn query(&mut self, q: &'p QPlan) -> R<Vec<Vec<Val>>> {
        stack::guard(|| self.query_inner(q))
    }

    fn query_inner(&mut self, q: &'p QPlan) -> R<Vec<Vec<Val>>> {
        let mut rows = self.term(&q.first)?;
        for (all, term) in &q.unions {
            rows.extend(self.term(term)?);
            if !*all {
                rows = self.distinct(rows);
            }
        }
        if !q.order.is_empty() {
            if let Term::Spec(spec) = &q.first {
                let by_aggregate = q
                    .order
                    .iter()
                    .any(|(k, _)| matches!(k, OrderKey::Output(i) if spec.cols[*i].aggregate));
                if spec.grouped && by_aggregate {
                    rows.reverse();
                }
            }
            let keys: Vec<Vec<Key>> = rows
                .iter()
                .map(|r| {
                    q.order
                        .iter()
                        .map(|(k, _)| {
                            let v = match k {
                                OrderKey::Output(i) => &r.vals[*i],
                                OrderKey::Hidden(i) => &r.hidden[*i],
                            };
                            Key::of(v)
                        })
                        .collect()
                })
                .collect();
            let mut index: Vec<usize> = (0..rows.len()).collect();
            index.sort_by(|&a, &b| {
                for (i, (_, desc)) in q.order.iter().enumerate() {
                    let o = keys[a][i].order(&keys[b][i]);
                    let o = if *desc { o.reverse() } else { o };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            });
            let mut taken: Vec<Option<OutRow>> = rows.into_iter().map(Some).collect();
            let sorted_keys: Vec<Vec<Key>> = index.iter().map(|&i| keys[i].clone()).collect();
            rows = index
                .iter()
                .map(|&i| taken[i].take().expect("row"))
                .collect();
            let kept = self.limit(q, rows, Some(&sorted_keys));
            return self.finish(q, kept);
        }
        let kept = self.limit(q, rows, None);
        self.finish(q, kept)
    }

    /// The values of the rows a query keeps, its summaries over them.
    fn finish(&mut self, q: &'p QPlan, rows: Vec<OutRow>) -> R<Vec<Vec<Val>>> {
        let rows = self.cut_summaries(q, rows)?;
        Ok(rows.into_iter().map(|r| r.vals).collect())
    }

    /// FileMaker cuts a summary's found set to the records a query keeps
    /// (execute-sql.md 8.1, q20): `FETCH FIRST 2 ROWS ONLY` totals the two
    /// rows, `OFFSET` the rest. With a `WHERE` and an `ORDER BY` a running
    /// summary follows the `ORDER BY`; otherwise record order. For one
    /// ungrouped table, not `DISTINCT` and without `UNION`, the columns
    /// that read a summary field are read again on the kept rows over that
    /// found set; every other value, the rows and their order stay as
    /// they are. Only plain reads of the table qualify (fields, constants,
    /// operators, functions, casts), and an `ORDER BY` reading a summary
    /// keeps the rows' first values, since it chose them.
    fn cut_summaries(&mut self, q: &'p QPlan, mut rows: Vec<OutRow>) -> R<Vec<OutRow>> {
        let Term::Spec(spec) = &q.first else {
            return Ok(rows);
        };
        if !q.unions.is_empty() || spec.grouped || spec.distinct || spec.insts.len() != 1 {
            return Ok(rows);
        }
        let InstSrc::User { fields, .. } = &spec.insts[0].src else {
            return Ok(rows);
        };
        let ordered = spec.filter.is_some() && !q.order.is_empty();
        if q.offset == 0 && q.fetch.is_none() && !ordered {
            return Ok(rows);
        }
        let columns: Vec<usize> = (0..spec.cols.len())
            .filter(|&i| reads_summary(&spec.cols[i].expr, fields))
            .collect();
        let plain = |e: &E| reads(e, &mut Vec::new());
        if columns.is_empty()
            || !spec.cols.iter().all(|c| plain(&c.expr))
            || !spec
                .order_exprs
                .iter()
                .all(|e| plain(e) && !reads_summary(e, fields))
            || q.order
                .iter()
                .any(|(k, _)| matches!(k, OrderKey::Output(i) if columns.contains(i)))
        {
            return Ok(rows);
        }
        let Some(kept) = rows.iter().map(|r| r.record).collect::<Option<Vec<u64>>>() else {
            return Ok(rows);
        };
        let all = self.records_of(&spec.insts[0].src)?;
        let found: Vec<u64> = if ordered {
            kept
        } else {
            let kept: std::collections::HashSet<u64> = kept.into_iter().collect();
            all.iter().copied().filter(|r| kept.contains(r)).collect()
        };
        let position: HashMap<u64, usize> =
            found.iter().enumerate().map(|(k, r)| (*r, k)).collect();
        self.frames.push(Frame {
            spec,
            all: vec![all],
            records: vec![Rc::new(found)],
            row: vec![None],
            aggs: Vec::new(),
        });
        let result = self.reread(spec, &columns, &position, &mut rows);
        self.frames.pop();
        result?;
        Ok(rows)
    }

    /// Columns `columns` of `rows` read again in the frame on top.
    fn reread(
        &mut self,
        spec: &'p Spec,
        columns: &[usize],
        position: &HashMap<u64, usize>,
        rows: &mut [OutRow],
    ) -> R<()> {
        for row in rows {
            self.visit()?;
            let k = row.record.and_then(|r| position.get(&r).copied());
            self.frame().row = vec![k];
            for &i in columns {
                let c = &spec.cols[i];
                let v = self.eval(&c.expr)?;
                row.vals[i] = column_value(c.ty, v);
            }
        }
        Ok(())
    }

    /// OFFSET and FETCH.
    fn limit(&self, q: &QPlan, rows: Vec<OutRow>, keys: Option<&[Vec<Key>]>) -> Vec<OutRow> {
        let total = rows.len();
        let mut out: Vec<(usize, OutRow)> = rows.into_iter().enumerate().skip(q.offset).collect();
        if let Some(f) = q.fetch {
            let n = if f.percent {
                (f.count as usize * total).div_ceil(100)
            } else {
                f.count as usize
            };
            let mut end = n.min(out.len());
            if f.with_ties && end > 0 {
                if let Some(keys) = keys {
                    let last = out[end - 1].0;
                    while end < out.len()
                        && keys_cmp(&keys[out[end].0], &keys[last]) == Ordering::Equal
                    {
                        end += 1;
                    }
                }
            }
            out.truncate(end);
        }
        out.into_iter().map(|(_, r)| r).collect()
    }

    fn term(&mut self, t: &'p Term) -> R<Vec<OutRow>> {
        match t {
            Term::Spec(spec) => self.spec(spec),
            Term::Query(q) => Ok(self
                .query(q)?
                .into_iter()
                .map(|vals| OutRow {
                    vals,
                    hidden: Vec::new(),
                    record: None,
                })
                .collect()),
        }
    }

    /// Rows deduplicated by their keys, the first kept, in key order.
    fn distinct(&self, rows: Vec<OutRow>) -> Vec<OutRow> {
        let keyed: Vec<(Vec<Key>, OutRow)> = rows
            .into_iter()
            .map(|r| (r.vals.iter().map(Key::of).collect(), r))
            .collect();
        let mut keyed = keyed;
        keyed.sort_by(|a, b| keys_cmp(&a.0, &b.0));
        let mut out: Vec<(Vec<Key>, OutRow)> = Vec::new();
        for (k, r) in keyed {
            if out
                .last()
                .is_some_and(|(lk, _)| keys_cmp(lk, &k) == Ordering::Equal)
            {
                continue;
            }
            out.push((k, r));
        }
        out.into_iter().map(|(_, r)| r).collect()
    }

    fn records_of(&mut self, src: &InstSrc) -> R<Rc<Vec<u64>>> {
        match src {
            InstSrc::User { table, .. } => {
                if table.base.is_none() {
                    return Ok(Rc::new(Vec::new()));
                }
                if let Some(r) = self.records.get(&table.id) {
                    return Ok(r.clone());
                }
                let r = Rc::new(self.source.records(table)?);
                self.records.insert(table.id, r.clone());
                Ok(r)
            }
            InstSrc::Rows { rows, .. } => Ok(Rc::new((1..=rows.len() as u64).collect())),
        }
    }

    fn spec(&mut self, spec: &'p Spec) -> R<Vec<OutRow>> {
        let mut records = Vec::new();
        for inst in &spec.insts {
            records.push(self.records_of(&inst.src)?);
        }
        let width = spec.insts.len();
        self.frames.push(Frame {
            spec,
            all: records.clone(),
            records,
            row: vec![None; width],
            aggs: Vec::new(),
        });
        let result = self.found_sets(spec).and_then(|_| self.spec_rows(spec));
        self.frames.pop();
        let mut rows = result?;
        if spec.distinct {
            rows = self.distinct(rows);
        }
        Ok(rows)
    }

    /// Each table's found set (10.1): its records left by the `WHERE`
    /// conjuncts that read only that table, whatever their form (`OR`,
    /// `NOT`, arithmetic, functions; 12, S3), as FileMaker's finds leave
    /// them (an index proposes candidates for `field = constant`; every
    /// record is checked again). Tables on the null-supplied side of a
    /// `LEFT JOIN` keep all records. The whole `WHERE` still runs on every
    /// row. In a join only the first table's found set is what its
    /// calculations see; the others narrow their candidates only
    /// ([`Frame::all`]).
    fn found_sets(&mut self, spec: &'p Spec) -> R<()> {
        let Some(filter) = &spec.filter else {
            return Ok(());
        };
        let conjuncts: Vec<&'p C> = match filter {
            C::And(items) => items.iter().collect(),
            other => vec![other],
        };
        let mut supplied = Vec::new();
        for f in &spec.from {
            null_supplied(f, false, &mut supplied);
        }
        let width = spec.insts.len();
        for c in conjuncts {
            let mut used = Vec::new();
            if !findable(c, &mut used) || used.len() != 1 || supplied.contains(&used[0]) {
                continue;
            }
            let i = used[0];
            let mut candidates = self.frame().records[i].clone();
            if let (Some(mut subset), InstSrc::User { table, fields }) =
                (self.indexed(c, i)?, &spec.insts[i].src)
            {
                let _ = (table, fields);
                // An index can return many candidates, in any order.
                // Sort its owned ids in place so intersection does not
                // scan the entire index result for every table record.
                // Keep the table's order, and recheck the SQL predicate.
                subset.sort_unstable();
                candidates = Rc::new(
                    candidates
                        .iter()
                        .copied()
                        .filter(|r| subset.binary_search(r).is_ok())
                        .collect(),
                );
            }
            self.frame().records[i] = candidates.clone();
            let mut kept = Vec::new();
            for (k, record) in candidates.iter().enumerate() {
                self.visit()?;
                let mut row = vec![None; width];
                row[i] = Some(k);
                self.frame().row = row;
                if self.cond(c)? == Some(true) {
                    kept.push(*record);
                }
            }
            self.frame().records[i] = Rc::new(kept);
        }
        Ok(())
    }

    /// The records an index proposes for `field = constant` on instance `i`.
    fn indexed(&mut self, c: &'p C, i: usize) -> R<Option<Vec<u64>>> {
        let C::Cmp {
            l,
            op: CompareOp::Eq,
            r,
        } = c
        else {
            return Ok(None);
        };
        let (field, value) = match (l, r) {
            (
                E::Field {
                    field,
                    rep: Rep::Const(1),
                    depth: 0,
                    ..
                },
                other,
            )
            | (
                other,
                E::Field {
                    field,
                    rep: Rep::Const(1),
                    depth: 0,
                    ..
                },
            ) if is_constant(other) => (*field, self.eval(other)?),
            _ => return Ok(None),
        };
        if value.is_null() {
            return Ok(None);
        }
        let spec = self.frame().spec;
        let InstSrc::User { table, fields } = &spec.insts[i].src else {
            return Ok(None);
        };
        self.source.lookup(table, &fields[field], &value.to_calc())
    }

    fn frame(&mut self) -> &mut Frame<'p> {
        self.frames.last_mut().expect("a frame")
    }

    fn spec_rows(&mut self, spec: &'p Spec) -> R<Vec<OutRow>> {
        let width = spec.insts.len();
        // FROM: the comma-separated references (and CROSS JOINs) as a
        // Cartesian product, in order, the first reference's rows
        // outermost. Each row is made and tested with the WHERE on its own
        // and only the rows it keeps are held, so a large product with a
        // selective WHERE takes the memory of its result, not of the
        // product.
        let mut parts = Vec::new();
        for f in &spec.from {
            self.product_parts(f, width, &mut parts)?;
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut at = vec![0usize; parts.len()];
        if parts.iter().all(|p| !p.is_empty()) {
            loop {
                self.visit()?;
                let mut row = vec![None; width];
                for (part, &k) in parts.iter().zip(&at) {
                    for (slot, value) in row.iter_mut().zip(&part[k]) {
                        *slot = slot.or(*value);
                    }
                }
                let keep = match &spec.filter {
                    Some(filter) => {
                        self.frame().row = row.clone();
                        self.cond(filter)? == Some(true)
                    }
                    None => true,
                };
                if keep {
                    self.hold(rows.len() + 1, width)?;
                    rows.push(row);
                }
                // The next combination, the last reference's rows first.
                let Some(i) = (0..parts.len()).rev().find(|&i| at[i] + 1 < parts[i].len()) else {
                    break;
                };
                at[i] += 1;
                at[i + 1..].fill(0);
            }
        }
        let mut out = Vec::new();
        if !spec.grouped {
            for r in rows {
                self.visit()?;
                self.frame().row = r;
                out.push(self.project(spec)?);
            }
            return Ok(out);
        }
        // Groups by their keys, in key order.
        let mut groups: Vec<Vec<Row>> = Vec::new();
        if spec.group_by.is_empty() {
            groups.push(rows);
        } else {
            let mut keyed = Vec::new();
            for r in rows {
                self.visit()?;
                self.frame().row = r.clone();
                let mut keys = Vec::new();
                for g in &spec.group_by {
                    let v = self.eval(g)?;
                    keys.push(Key::of(&v));
                }
                keyed.push((keys, r));
            }
            keyed.sort_by(|a, b| keys_cmp(&a.0, &b.0));
            let mut last: Option<Vec<Key>> = None;
            for (k, r) in keyed {
                match &last {
                    Some(lk) if keys_cmp(lk, &k) == Ordering::Equal => {
                        groups.last_mut().expect("group").push(r)
                    }
                    _ => {
                        groups.push(vec![r]);
                        last = Some(k);
                    }
                }
            }
        }
        for group in groups {
            self.visit()?;
            let mut aggs = Vec::new();
            for a in &spec.aggs {
                aggs.push(self.aggregate(a, &group)?);
            }
            let frame = self.frame();
            frame.aggs = aggs;
            frame.row = group.first().cloned().unwrap_or_else(|| vec![None; width]);
            if let Some(h) = &spec.having {
                if self.cond(h)? != Some(true) {
                    continue;
                }
            }
            out.push(self.project(spec)?);
        }
        Ok(out)
    }

    fn project(&mut self, spec: &'p Spec) -> R<OutRow> {
        let mut vals = Vec::with_capacity(spec.cols.len());
        for c in &spec.cols {
            let v = self.eval(&c.expr)?;
            vals.push(column_value(c.ty, v));
        }
        let mut hidden = Vec::new();
        for e in &spec.order_exprs {
            hidden.push(self.eval(e)?);
        }
        let record = match (&spec.insts[..], spec.grouped) {
            ([_], false) => {
                let frame = self.frame();
                frame.row[0].map(|k| frame.records[0][k])
            }
            _ => None,
        };
        Ok(OutRow {
            vals,
            hidden,
            record,
        })
    }

    /// The factors of the Cartesian product a `FROM` reference makes: a
    /// chain of CROSS JOINs (and joins without a condition) is one factor
    /// per side, any other reference one factor of its rows.
    fn product_parts(&mut self, f: &'p From, width: usize, parts: &mut Vec<Vec<Row>>) -> R<()> {
        stack::guard(|| self.product_parts_inner(f, width, parts))
    }

    fn product_parts_inner(
        &mut self,
        f: &'p From,
        width: usize,
        parts: &mut Vec<Vec<Row>>,
    ) -> R<()> {
        match f {
            From::Join {
                left,
                kind,
                right,
                on,
            } if *kind == JoinKind::Cross || (on.is_none() && *kind != JoinKind::Left) => {
                self.product_parts(left, width, parts)?;
                self.product_parts(right, width, parts)
            }
            _ => {
                let rows = self.rows_of_from(f, width)?;
                parts.push(rows);
                Ok(())
            }
        }
    }

    /// The rows one `FROM` reference contributes, with its instances set.
    fn rows_of_from(&mut self, f: &'p From, width: usize) -> R<Vec<Row>> {
        stack::guard(|| self.rows_of_from_inner(f, width))
    }

    fn rows_of_from_inner(&mut self, f: &'p From, width: usize) -> R<Vec<Row>> {
        match f {
            From::Inst(i) => {
                let n = self.frame().records[*i].len();
                self.hold(n, width)?;
                Ok((0..n)
                    .map(|k| {
                        let mut r = vec![None; width];
                        r[*i] = Some(k);
                        r
                    })
                    .collect())
            }
            From::Join {
                left,
                kind,
                right,
                on,
            } => {
                let l = self.rows_of_from(left, width)?;
                let r = self.rows_of_from(right, width)?;
                let mut out = Vec::new();
                for lr in &l {
                    let mut matched = false;
                    for rr in &r {
                        self.visit()?;
                        let row = merge(lr, rr);
                        let keep = match (kind, on) {
                            (JoinKind::Cross, _) | (_, None) => true,
                            (_, Some(c)) => {
                                self.frame().row = row.clone();
                                self.cond(c)? == Some(true)
                            }
                        };
                        if keep {
                            matched = true;
                            self.hold(out.len() + 1, width)?;
                            out.push(row);
                        }
                    }
                    if *kind == JoinKind::Left && !matched {
                        self.hold(out.len() + 1, width)?;
                        out.push(lr.clone());
                    }
                }
                Ok(out)
            }
        }
    }

    fn aggregate(&mut self, a: &'p AggPlan, group: &[Row]) -> R<Val> {
        let Some(arg) = &a.arg else {
            return Ok(Val::Number(Number::from_i64(group.len() as i64)));
        };
        let (loc, today) = self.cx_parts();
        let cx = Ctx::new(&loc, today);
        let mut values = Vec::new();
        for r in group {
            self.visit()?;
            self.frame().row = r.clone();
            let v = self.eval(arg)?;
            if !v.is_null() && !matches!(v, Val::Summary) {
                values.push(v);
            }
        }
        if a.distinct {
            let mut keyed: Vec<(Key, Val)> = values.into_iter().map(|v| (Key::of(&v), v)).collect();
            keyed.sort_by(|x, y| x.0.order(&y.0));
            keyed.dedup_by(|x, y| x.0.order(&y.0) == Ordering::Equal);
            values = keyed.into_iter().map(|(_, v)| v).collect();
        }
        let numbers = |values: &[Val]| -> Vec<Number> {
            values.iter().map(|v| v.to_calc().to_number(&cx)).collect()
        };
        Ok(match a.func {
            Aggregate::Count => Val::Number(Number::from_i64(values.len() as i64)),
            Aggregate::Sum | Aggregate::Avg => {
                if values.is_empty() {
                    return Ok(Val::Null);
                }
                // Values reach the sum as stored results, at the display
                // precision (`SUM(1/3)` over 12 rows is 3.9999999999999996).
                let nums: Vec<Number> = numbers(&values)
                    .iter()
                    .map(|n| n.round_to_precision(cx.precision))
                    .collect();
                let mut sum = Number::zero();
                for n in &nums {
                    sum = sum.add(n, cx.precision);
                }
                if a.func == Aggregate::Avg {
                    sum = sum.div(&Number::from_i64(nums.len() as i64), cx.precision)?;
                }
                Val::Number(sum)
            }
            Aggregate::Min | Aggregate::Max => {
                let mut best: Option<Val> = None;
                for v in values {
                    if matches!(v, Val::Container(_)) {
                        return Ok(Val::Null);
                    }
                    best = Some(match best {
                        None => v,
                        Some(b) => {
                            let o = cmp_values(&v, &b, &cx);
                            let better = match a.func {
                                Aggregate::Min => o == Some(Ordering::Less),
                                _ => o == Some(Ordering::Greater),
                            };
                            if better {
                                v
                            } else {
                                b
                            }
                        }
                    });
                }
                match best {
                    Some(Val::Bool(b)) => Val::Number(Number::from_bool(b)),
                    Some(v) => v,
                    None => Val::Null,
                }
            }
            Aggregate::Every | Aggregate::Some => {
                if values.is_empty() {
                    return Ok(Val::Null);
                }
                let truth: Vec<bool> = values.iter().map(|v| v.to_calc().is_true(&cx)).collect();
                let r = if a.func == Aggregate::Every {
                    truth.iter().all(|t| *t)
                } else {
                    truth.iter().any(|t| *t)
                };
                Val::Number(Number::from_bool(r))
            }
        })
    }

    fn frame_at(&self, depth: usize) -> &Frame<'p> {
        &self.frames[self.frames.len() - 1 - depth]
    }

    fn eval(&mut self, e: &'p E) -> R<Val> {
        stack::guard(|| self.eval_inner(e))
    }

    fn eval_inner(&mut self, e: &'p E) -> R<Val> {
        let (loc, today) = self.cx_parts();
        let cx = Ctx::new(&loc, today);
        Ok(match e {
            E::Const(v) => v.clone(),
            E::Field {
                depth,
                inst,
                field,
                rep,
            } => {
                let rep = match rep {
                    Rep::Const(n) => *n,
                    Rep::Expr(x) => {
                        let v = self.eval(x)?;
                        let n = v.to_calc().to_number(&cx).int().as_long_long();
                        if n < 1 {
                            return Ok(Val::Null);
                        }
                        n as u32
                    }
                };
                let frame = self.frame_at(*depth);
                let InstSrc::User { table, fields } = &frame.spec.insts[*inst].src else {
                    return Ok(Val::Null);
                };
                let Some(row) = frame.row[*inst] else {
                    return Ok(Val::Null);
                };
                let record = frame.records[*inst][row];
                let f = &fields[*field];
                if rep > f.repetitions {
                    return Ok(Val::Null);
                }
                // In a join, a table after the first keeps every record
                // as its found set (12, S3).
                let found = if *inst == 0 || Rc::ptr_eq(&frame.records[*inst], &frame.all[*inst]) {
                    &frame.records[*inst]
                } else {
                    &frame.all[*inst]
                };
                let record_number = if Rc::ptr_eq(found, &frame.records[*inst]) {
                    row + 1
                } else {
                    found.iter().position(|r| *r == record).map_or(0, |p| p + 1)
                };
                let cursor = Cursor {
                    found_count: found.len(),
                    record_number,
                    found: Some(found.clone()),
                };
                let (table, f) = (table.clone(), f.clone());
                let v = self.source.value(&table, record, &f, rep, cursor)?;
                if f.class == crate::source::FieldClass::Summary && v.is_empty() {
                    // An empty summary is empty but not NULL (8.1).
                    return Ok(Val::Summary);
                }
                Val::from_calc(v)
            }
            E::Col { depth, inst, col } => {
                let frame = self.frame_at(*depth);
                let InstSrc::Rows { rows, .. } = &frame.spec.insts[*inst].src else {
                    return Ok(Val::Null);
                };
                // Rows are numbered from 1 in the instance's found set.
                match frame.row[*inst] {
                    Some(k) => {
                        let r = frame.records[*inst][k] as usize - 1;
                        Val::from_calc(rows[r][*col].clone())
                    }
                    None => Val::Null,
                }
            }
            E::RowId { depth, inst } => {
                let frame = self.frame_at(*depth);
                match frame.row[*inst] {
                    Some(r) => Val::Number(Number::from_i64(frame.records[*inst][r] as i64)),
                    None => Val::Number(Number::zero()),
                }
            }
            E::RowModId { depth, inst } => {
                let frame = self.frame_at(*depth);
                match (frame.row[*inst], &frame.spec.insts[*inst].src) {
                    (Some(r), InstSrc::User { table, .. }) => {
                        let (table, record) = (table.clone(), frame.records[*inst][r]);
                        let n = self.source.mod_count(&table, record);
                        Val::Number(Number::from_i64(n as i64))
                    }
                    (Some(_), _) => Val::Number(Number::zero()),
                    (None, _) => Val::Number(Number::zero()),
                }
            }
            E::Param { index, mode } => {
                let Some(v) = self.args.get(*index) else {
                    return Ok(Val::Null);
                };
                param(v, *mode, &cx)
            }
            E::Neg(x) => {
                let v = self.eval(x)?;
                if v.is_null() {
                    return Ok(Val::Null);
                }
                Val::from_calc(ops::negate(&v.to_calc(), &cx))
            }
            E::Arith { op, l, r, ty } => {
                let a = self.eval(l)?;
                let b = self.eval(r)?;
                if a.is_null()
                    || b.is_null()
                    || matches!(a, Val::Summary)
                    || matches!(b, Val::Summary)
                {
                    return Ok(Val::Null);
                }
                arith(*op, &a, &b, *ty, &cx)?
            }
            E::Concat(l, r) => {
                let a = self.eval(l)?;
                let b = self.eval(r)?;
                let text = |v: &Val| match v {
                    Val::Null | Val::Summary => String::new(),
                    Val::Text(s) | Val::Container(s) => s.clone(),
                    other => other.to_calc().to_text(&cx).into_string(),
                };
                Val::text(text(&a) + &text(&b))
            }
            E::Interval {
                e,
                subtract,
                amount,
                unit,
                ty,
            } => {
                let v = self.eval(e)?;
                let n = self.eval(amount)?;
                if v.is_null() || n.is_null() {
                    return Ok(Val::Null);
                }
                let mut n = n.to_calc().to_number(&cx);
                if *subtract {
                    n = n.neg();
                }
                interval(&v, &n, *unit, *ty, &cx)?
            }
            E::Case { whens, otherwise } => {
                for (c, r) in whens {
                    if self.cond(c)? == Some(true) {
                        return self.eval(r);
                    }
                }
                match otherwise {
                    Some(o) => self.eval(o)?,
                    None => Val::Null,
                }
            }
            E::Cast { e, to } => {
                let v = self.eval(e)?;
                cast(v, *to, &cx)?
            }
            E::Agg(i) => self.frame().aggs.get(*i).cloned().unwrap_or(Val::Null),
            E::Func { f, args } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a)?);
                }
                func::call(*f, &vals, &cx, self.now, &self.user)?
            }
            E::Trim {
                e,
                chars,
                leading,
                trailing,
            } => {
                let v = self.eval(e)?;
                let c = match chars {
                    Some(c) => Some(self.eval(c)?),
                    None => None,
                };
                if v.is_null() || c.as_ref().is_some_and(Val::is_null) {
                    return Ok(Val::Null);
                }
                let text = v.to_calc().to_text(&cx).into_string();
                let chars = c.map(|c| c.to_calc().to_text(&cx).into_string());
                func::trim(&text, chars.as_deref(), *leading, *trailing)
            }
            E::Scalar(q) => {
                let rows = self.query(q)?;
                match rows.len() {
                    0 => Val::Null,
                    1 => rows
                        .into_iter()
                        .next()
                        .and_then(|r| r.into_iter().next())
                        .unwrap_or(Val::Null),
                    _ => return Err(SqlError::Engine(8303)),
                }
            }
            E::Nullif(a, b) => {
                let x = self.eval(a)?;
                let y = self.eval(b)?;
                if !x.is_null() && !y.is_null() && cmp_values(&x, &y, &cx) == Some(Ordering::Equal)
                {
                    Val::Null
                } else {
                    x
                }
            }
            E::Shared(x) => self.eval(x)?,
            E::Coalesce(items) => {
                for i in items {
                    let v = self.eval(i)?;
                    if !v.is_null() {
                        return Ok(v);
                    }
                }
                Val::Null
            }
        })
    }

    fn list(&mut self, list: &'p InList) -> R<Vec<Val>> {
        match list {
            InList::Values(items) => {
                let mut out = Vec::new();
                for i in items {
                    out.push(self.eval(i)?);
                }
                Ok(out)
            }
            InList::Query(q) => Ok(self
                .query(q)?
                .into_iter()
                .filter_map(|r| r.into_iter().next())
                .collect()),
        }
    }

    fn cond(&mut self, c: &'p C) -> R<Option<bool>> {
        stack::guard(|| self.cond_inner(c))
    }

    fn cond_inner(&mut self, c: &'p C) -> R<Option<bool>> {
        let (loc, today) = self.cx_parts();
        let cx = Ctx::new(&loc, today);
        Ok(match c {
            C::And(items) => {
                let mut result = Some(true);
                for i in items {
                    match self.cond(i)? {
                        Some(false) => return Ok(Some(false)),
                        None => result = None,
                        Some(true) => {}
                    }
                }
                result
            }
            C::Or(items) => {
                let mut result = Some(false);
                for i in items {
                    match self.cond(i)? {
                        Some(true) => return Ok(Some(true)),
                        None => result = None,
                        Some(false) => {}
                    }
                }
                result
            }
            C::Not(inner) => self.cond(inner)?.map(|b| !b),
            C::Cmp { l, op, r } => {
                let a = self.eval(l)?;
                let b = self.eval(r)?;
                test(*op, sql_cmp(&a, &b, &cx))
            }
            C::In { l, list, negated } => {
                let v = self.eval(l)?;
                let items = self.list(list)?;
                let r = any_equal(&v, &items, &cx);
                if *negated {
                    r.map(|b| !b)
                } else {
                    r
                }
            }
            C::Quant { l, op, all, list } => {
                let v = self.eval(l)?;
                let items = self.list(list)?;
                let mut unknown = false;
                let mut result = *all;
                for i in &items {
                    match test(*op, sql_cmp(&v, i, &cx)) {
                        None => unknown = true,
                        Some(b) => {
                            if *all && !b {
                                return Ok(Some(false));
                            }
                            if !*all && b {
                                return Ok(Some(true));
                            }
                        }
                    }
                    let _ = &mut result;
                }
                if unknown {
                    None
                } else {
                    Some(*all)
                }
            }
            C::IsNull { e, negated } => {
                let v = self.eval(e)?;
                Some(v.is_null() != *negated)
            }
            C::IsBool { e, negated, what } => {
                let v = self.eval(e)?;
                let truth = if v.is_null() {
                    None
                } else {
                    Some(v.to_calc().is_true(&cx))
                };
                let r = match (what, truth) {
                    (_, None) => false,
                    (IsWhat::True, Some(t)) => t,
                    (_, Some(t)) => !t,
                };
                Some(r != *negated)
            }
            C::Between {
                e,
                low,
                high,
                negated,
            } => {
                let v = self.eval(e)?;
                let lo = self.eval(low)?;
                let hi = self.eval(high)?;
                let a = test(CompareOp::Ge, sql_cmp(&v, &lo, &cx));
                let b = test(CompareOp::Le, sql_cmp(&v, &hi, &cx));
                let r = match (a, b) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                };
                if *negated {
                    r.map(|b| !b)
                } else {
                    r
                }
            }
            C::Like {
                e,
                pattern,
                negated,
            } => {
                let v = self.eval(e)?;
                let p = self.eval(pattern)?;
                match (text_of(&v, &cx), text_of(&p, &cx)) {
                    (Some(t), Some(p)) => Some(value::like(&t, &p) != *negated),
                    _ => None,
                }
            }
            C::PartOf {
                part,
                whole,
                negated,
            } => {
                let a = self.eval(part)?;
                let b = self.eval(whole)?;
                match (text_of(&a, &cx), text_of(&b, &cx)) {
                    (Some(a), Some(b)) => Some(b.contains(&a) != *negated),
                    _ => None,
                }
            }
            C::Exists { q, unique } => {
                let rows = self.query(q)?;
                if *unique {
                    let keys: Vec<Vec<Key>> = rows
                        .iter()
                        .map(|r| r.iter().map(Key::of).collect())
                        .collect();
                    let mut dup = false;
                    for i in 0..keys.len() {
                        for j in 0..i {
                            if keys_cmp(&keys[i], &keys[j]) == Ordering::Equal {
                                dup = true;
                            }
                        }
                    }
                    Some(!dup)
                } else {
                    Some(!rows.is_empty())
                }
            }
            C::False => Some(false),
        })
    }
}

/// The instances on the null-supplied side of a `LEFT JOIN`.
fn null_supplied(f: &From, inside: bool, out: &mut Vec<usize>) {
    match f {
        From::Inst(i) => {
            if inside {
                out.push(*i);
            }
        }
        From::Join {
            left, kind, right, ..
        } => {
            null_supplied(left, inside, out);
            null_supplied(right, inside || *kind == JoinKind::Left, out);
        }
    }
}

/// A conjunct a find can serve: comparisons, ranges, lists, patterns and
/// emptiness tests over the current query's columns and constants, and
/// their `AND`, `OR` and `NOT` (a subquery is not). `used` collects the
/// instances it reads.
fn findable(c: &C, used: &mut Vec<usize>) -> bool {
    match c {
        C::And(items) | C::Or(items) => items.iter().all(|i| findable(i, used)),
        C::Not(x) => findable(x, used),
        C::IsBool { e, .. } => reads(e, used),
        C::Quant {
            l,
            list: InList::Values(items),
            ..
        } => reads(l, used) && items.iter().all(|i| reads(i, used)),
        C::Cmp { l, r, .. } => reads(l, used) && reads(r, used),
        C::Between { e, low, high, .. } => reads(e, used) && reads(low, used) && reads(high, used),
        C::In {
            l,
            list: InList::Values(items),
            ..
        } => reads(l, used) && items.iter().all(|i| reads(i, used)),
        C::Like { e, pattern, .. } => reads(e, used) && reads(pattern, used),
        C::IsNull { e, .. } => reads(e, used),
        _ => false,
    }
}

/// Whether an expression reads only fields of the current query (and
/// constants), collecting the instances.
fn reads(e: &E, used: &mut Vec<usize>) -> bool {
    match e {
        E::Const(_) | E::Param { .. } => true,
        E::Field {
            depth: 0,
            inst,
            rep,
            ..
        } => {
            if !used.contains(inst) {
                used.push(*inst);
            }
            !matches!(rep, Rep::Expr(_))
        }
        E::Col { depth: 0, inst, .. }
        | E::RowId { depth: 0, inst }
        | E::RowModId { depth: 0, inst } => {
            if !used.contains(inst) {
                used.push(*inst);
            }
            true
        }
        E::Neg(x) => reads(x, used),
        E::Arith { l, r, .. } | E::Concat(l, r) => reads(l, used) && reads(r, used),
        E::Func { args, .. } => args.iter().all(|a| reads(a, used)),
        E::Cast { e, .. } => reads(e, used),
        _ => false,
    }
}

/// An output column's value: a date column's timestamp as its date.
fn column_value(ty: Ty, v: Val) -> Val {
    match (ty, v) {
        (Ty::Date, Val::Timestamp(t)) => Val::Date(t.date()),
        (_, v) => v,
    }
}

/// Whether a plain read of the first table ([`reads`]) reads one of its
/// summary fields.
fn reads_summary(e: &E, fields: &[crate::source::FieldDef]) -> bool {
    match e {
        E::Field {
            depth: 0,
            inst: 0,
            field,
            ..
        } => fields
            .get(*field)
            .is_some_and(|f| f.class == crate::source::FieldClass::Summary),
        E::Neg(x) | E::Cast { e: x, .. } => reads_summary(x, fields),
        E::Arith { l, r, .. } | E::Concat(l, r) => {
            reads_summary(l, fields) || reads_summary(r, fields)
        }
        E::Func { args, .. } => args.iter().any(|a| reads_summary(a, fields)),
        _ => false,
    }
}

fn is_constant(e: &E) -> bool {
    matches!(e, E::Const(_) | E::Param { .. })
}

fn merge(a: &Row, b: &Row) -> Row {
    a.iter().zip(b).map(|(x, y)| x.or(*y)).collect()
}

fn text_of(v: &Val, cx: &Ctx) -> Option<String> {
    match v {
        Val::Text(s) => Some(s.clone()),
        Val::Param(p) => Some(p.to_text(cx).into_string()),
        v if v.is_null() => None,
        _ => None,
    }
}

fn test(op: CompareOp, o: Option<Ordering>) -> Option<bool> {
    let o = o?;
    Some(match op {
        CompareOp::Eq => o == Ordering::Equal,
        CompareOp::Ne => o != Ordering::Equal,
        CompareOp::Lt => o == Ordering::Less,
        CompareOp::Le => o != Ordering::Greater,
        CompareOp::Gt => o == Ordering::Greater,
        CompareOp::Ge => o != Ordering::Less,
    })
}

fn any_equal(v: &Val, items: &[Val], cx: &Ctx) -> Option<bool> {
    let mut unknown = false;
    for i in items {
        match sql_cmp(v, i, cx) {
            Some(Ordering::Equal) => return Some(true),
            None => unknown = true,
            _ => {}
        }
    }
    if unknown {
        None
    } else {
        Some(false)
    }
}

/// SQL's comparison of two values (6.1, 5.4): NULL compares to nothing;
/// text beside a date, time or timestamp converts with the file's locale;
/// a date beside a time or timestamp matches nothing.
pub fn sql_cmp(a: &Val, b: &Val, cx: &Ctx) -> Option<Ordering> {
    if a.is_null() || b.is_null() || matches!(a, Val::Summary) || matches!(b, Val::Summary) {
        return None;
    }
    cmp_values(a, b, cx)
}

fn cmp_values(a: &Val, b: &Val, cx: &Ctx) -> Option<Ordering> {
    match (a, b) {
        (Val::Param(p), other) => cmp_values(&Val::from_calc(p.clone()), other, cx),
        (other, Val::Param(p)) => cmp_values(other, &Val::from_calc(p.clone()), cx),
        (Val::Text(t), x) if matches!(x, Val::Date(_) | Val::Time(_) | Val::Timestamp(_)) => {
            Some(compare(&temporal_of(t, x, cx)?, x)?)
        }
        (x, Val::Text(t)) if matches!(x, Val::Date(_) | Val::Time(_) | Val::Timestamp(_)) => {
            Some(compare(x, &temporal_of(t, x, cx)?)?)
        }
        // A date beside a time or timestamp compares their numbers (days
        // against seconds), so `=` never matches.
        (Val::Date(_), Val::Time(_) | Val::Timestamp(_))
        | (Val::Time(_) | Val::Timestamp(_), Val::Date(_)) => a
            .to_calc()
            .to_number(cx)
            .compare(&b.to_calc().to_number(cx)),
        _ => compare(a, b),
    }
}

/// Text as a value of the temporal type of `like`, in the file's locale.
fn temporal_of(text: &str, like: &Val, cx: &Ctx) -> Option<Val> {
    match like {
        Val::Date(_) if text.contains(':') => None,
        Val::Date(_) => Date::parse(text, cx.locale, cx.today)
            .ok()
            .flatten()
            .map(Val::Date),
        Val::Time(_) => Time::parse(text, cx.locale, cx.today)
            .ok()
            .flatten()
            .map(Val::Time),
        Val::Timestamp(_) => Timestamp::parse(text, cx.locale, cx.today)
            .ok()
            .flatten()
            .map(Val::Timestamp),
        _ => None,
    }
}

/// A number at the start of text, as a `?` compared with a number reads
/// it: digits with thousands separators and a decimal part, up to the
/// first other character (`'3abc'` is 3, `'1,2'` 12, `'a3b4'` none).
fn leading_number(text: &str, cx: &Ctx) -> Option<Number> {
    let t = text.trim_start_matches(' ');
    let mut s = String::new();
    let mut chars = t.chars().peekable();
    if let Some(&c) = chars.peek() {
        if c == '-' || c == '+' {
            s.push(c);
            chars.next();
        }
    }
    let mut digits = 0;
    let mut point = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            s.push(c);
            digits += 1;
        } else if c == cx.locale.decimal_separator && !point {
            s.push('.');
            point = true;
        } else if c == cx.locale.thousands_separator && digits > 0 && !point {
        } else {
            break;
        }
        chars.next();
    }
    if digits == 0 {
        return None;
    }
    let s = s.trim_end_matches('.');
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.trim_start_matches('+')),
    };
    let n = Number::from_literal(body, '.').ok()?;
    Some(if neg { n.neg() } else { n })
}

/// A `?` argument read in `mode` (5.7); empty arguments are NULL.
fn param(v: &Value, mode: PMode, cx: &Ctx) -> Val {
    if v.is_empty() {
        return Val::Null;
    }
    match mode {
        PMode::Raw => Val::Param(v.clone()),
        PMode::Compare(ty) => match ty {
            Ty::Text => match v {
                Value::Text(t) => Val::text(t.as_str()),
                _ => Val::Null,
            },
            Ty::Number | Ty::Bool => match v {
                Value::Number(n) | Value::NumberText(n, _) => Val::number(n.clone()),
                Value::Text(t) => leading_number(t.as_str(), cx).map_or(Val::Null, Val::Number),
                _ => Val::Null,
            },
            Ty::Date => match v {
                Value::Date(d) | Value::DateText(d, _) => Val::Date(*d),
                Value::Text(t) => {
                    temporal_of(t.as_str(), &Val::Date(*d0()), cx).unwrap_or(Val::Null)
                }
                _ => Val::Null,
            },
            Ty::Time => match v {
                Value::Time(t) => Val::Time(*t),
                Value::Text(t) => temporal_of(
                    t.as_str(),
                    &Val::Time(Time::from_micros(0, TimeStyle::Elapsed)),
                    cx,
                )
                .unwrap_or(Val::Null),
                _ => Val::Null,
            },
            Ty::Timestamp => match v {
                Value::Timestamp(t) => Val::Timestamp(*t),
                Value::Text(t) => temporal_of(t.as_str(), &Val::Timestamp(d0().to_timestamp()), cx)
                    .unwrap_or(Val::Null),
                _ => Val::Null,
            },
            Ty::Container => Val::Null,
            Ty::Null | Ty::Param => Val::Param(v.clone()),
        },
        PMode::Convert(ty) => match ty {
            Ty::Text => Val::text(v.to_text(cx).into_string()),
            Ty::Number | Ty::Bool => Val::number(v.to_number(cx)),
            _ => Val::from_calc(v.clone()),
        },
    }
}

fn d0() -> &'static Date {
    use std::sync::OnceLock;
    static D: OnceLock<Date> = OnceLock::new();
    D.get_or_init(|| Date::from_ymd(1, 1, 1).expect("1/1/0001"))
}

/// `+ - * / ^` (5.6).
fn arith(op: BinOp, a: &Val, b: &Val, ty: Ty, cx: &Ctx) -> R<Val> {
    if let (BinOp::Add | BinOp::Subtract, Val::Text(x), Val::Text(y)) = (op, a, b) {
        return Ok(Val::text(format!("{x}{y}")));
    }
    let (x, y) = (a.to_calc(), b.to_calc());
    let v = match op {
        BinOp::Add => ops::add(&x, &y, cx)?,
        BinOp::Subtract => ops::subtract(&x, &y, cx)?,
        BinOp::Multiply => ops::multiply(&x, &y, cx),
        BinOp::Divide => ops::divide(&x, &y, cx)?,
        BinOp::Power => ops::power(&x, &y, cx)?,
        BinOp::Concat => unreachable!("concatenation is not arithmetic"),
    };
    if matches!(a, Val::Param(_)) || matches!(b, Val::Param(_)) {
        // The argument's own type acted; the result converts through its
        // text to the type the other operand implies (5.7).
        let text = v.to_text(cx).into_string();
        return Ok(match ty {
            Ty::Number => Val::number(om_calc::value::parse_number(&text, cx.locale)),
            Ty::Text => Val::text(text),
            Ty::Date => temporal_of(&text, &Val::Date(*d0()), cx).unwrap_or(Val::Null),
            _ => Val::from_calc(v),
        });
    }
    Ok(Val::from_result(v, ty))
}

fn interval(v: &Val, n: &Number, unit: DatePart, ty: Ty, cx: &Ctx) -> R<Val> {
    let p = cx.precision;
    let seconds = |factor: i64| n.mul(&Number::from_i64(factor), p);
    let months = |date: Date, by: i64| {
        let (y, m, d) = date.ymd();
        Date::normalized(i64::from(m) + by, i64::from(d), y)
    };
    let whole = n.int().as_long_long();
    Ok(match (v, unit) {
        // A date's interval arithmetic yields its timestamp; output shows the
        // date (the column's type), comparisons see the timestamp.
        (Val::Date(d), DatePart::Month | DatePart::Year) => {
            let by = if unit == DatePart::Year {
                whole * 12
            } else {
                whole
            };
            months(*d, by).map_or(Val::Null, |d| Val::Timestamp(d.to_timestamp()))
        }
        (Val::Timestamp(t), DatePart::Month | DatePart::Year) => {
            let by = if unit == DatePart::Year {
                whole * 12
            } else {
                whole
            };
            match months(t.date(), by) {
                Some(d) => {
                    Timestamp::from_date_time(d, t.time_of_day()).map_or(Val::Null, Val::Timestamp)
                }
                None => Val::Null,
            }
        }
        (other, unit) => {
            let secs = match unit {
                DatePart::Day => seconds(86_400),
                DatePart::Hour => seconds(3_600),
                DatePart::Minute => seconds(60),
                DatePart::Second => n.clone(),
                DatePart::Month | DatePart::Year => return Ok(Val::Null),
            };
            let ty = if ty == Ty::Date { Ty::Timestamp } else { ty };
            let base = match other {
                Val::Date(d) => Value::Timestamp(d.to_timestamp()),
                v => v.to_calc(),
            };
            Val::from_result(ops::add(&base, &Value::Number(secs), cx)?, ty)
        }
    })
}

/// `CAST` (5.5).
fn cast(v: Val, to: CastType, cx: &Ctx) -> R<Val> {
    let v = match v {
        Val::Param(p) => Val::from_calc(p),
        v => v,
    };
    if v.is_null() {
        return Ok(Val::Null);
    }
    Ok(match to {
        CastType::Varchar => Val::text(v.strval(cx)),
        CastType::Int | CastType::Decimal | CastType::Boolean => match v {
            Val::Text(s) => func::numval(&s, cx)?,
            Val::Number(n) => Val::Number(n),
            Val::Bool(b) => Val::Number(Number::from_bool(b)),
            other => Val::number(other.to_calc().to_number(cx)),
        },
        CastType::Date => match v {
            Val::Text(s) => func::parse_iso_date(&s)
                .map(Val::Date)
                .ok_or(SqlError::Engine(500))?,
            Val::Number(n) => Date::from_number(&n).map_or(Val::Null, Val::Date),
            Val::Date(d) => Val::Date(d),
            Val::Timestamp(t) => Val::Date(t.date()),
            _ => Val::Null,
        },
        CastType::Time => match v {
            Val::Text(s) => func::parse_iso_time(&s)
                .map(Val::Time)
                .ok_or(SqlError::Engine(501))?,
            Val::Number(n) => Time::from_seconds(&n).map_or(Val::Null, Val::Time),
            Val::Date(d) => {
                Time::from_seconds(&d.to_timestamp().to_number()).map_or(Val::Null, Val::Time)
            }
            Val::Time(t) => Val::Time(t),
            Val::Timestamp(t) => Val::Time(t.time_of_day().with_style(TimeStyle::Elapsed)),
            _ => Val::Null,
        },
        CastType::Timestamp => match v {
            Val::Text(s) => func::parse_iso_timestamp(&s)
                .or_else(|| func::parse_iso_date(&s).map(Date::to_timestamp))
                .map(Val::Timestamp)
                .ok_or(SqlError::Engine(501))?,
            Val::Number(n) => Timestamp::from_seconds(&n).map_or(Val::Null, Val::Timestamp),
            Val::Date(d) => Val::Timestamp(d.to_timestamp()),
            Val::Time(t) => t
                .to_timestamp(TimeStyle::Elapsed)
                .map_or(Val::Null, Val::Timestamp),
            Val::Timestamp(t) => Val::Timestamp(t),
            _ => Val::Null,
        },
        CastType::Binary => Val::Null,
    })
}
