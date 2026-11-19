//! The statement of a parse tree (FileMaker's
//! `FQLSQLSemanticAnalyzerCustomVisitor` walks the same tree).

use super::ast::*;
use super::lex::{self, Token, TokenType};
use super::tree::{Node, Tree};

/// The statement of a successful parse.
pub fn statement(tokens: &[Token], node: &Node) -> Statement {
    B { tokens }.statement(node)
}

struct B<'t> {
    tokens: &'t [Token],
}

/// A node's children as tokens and rules.
struct Kids<'n, 't> {
    items: &'n [Tree],
    tokens: &'t [Token],
    at: usize,
}

impl<'n, 't> Kids<'n, 't> {
    fn peek_token(&self) -> Option<&'t Token> {
        match self.items.get(self.at)? {
            Tree::Token(i) => Some(&self.tokens[*i]),
            Tree::Rule(_) => None,
        }
    }

    fn peek_rule(&self) -> Option<&'n Node> {
        match self.items.get(self.at)? {
            Tree::Rule(n) => Some(n),
            Tree::Token(_) => None,
        }
    }

    fn peek_is(&self, ty: TokenType) -> bool {
        self.peek_token().is_some_and(|t| t.ty == ty)
    }

    fn peek_rule_is(&self, name: &str) -> bool {
        self.peek_rule()
            .is_some_and(|n| super::rule_name(n.rule) == name)
    }

    fn token(&mut self) -> &'t Token {
        let t = self.peek_token().expect("a token child");
        self.at += 1;
        t
    }

    /// The next child when it is the token `ty`.
    fn eat(&mut self, ty: TokenType) -> Option<&'t Token> {
        if self.peek_is(ty) {
            Some(self.token())
        } else {
            None
        }
    }

    fn rule(&mut self) -> &'n Node {
        let n = self.peek_rule().expect("a rule child");
        self.at += 1;
        n
    }

    /// The next child when it is the rule `name`.
    fn eat_rule(&mut self, name: &str) -> Option<&'n Node> {
        if self.peek_rule_is(name) {
            Some(self.rule())
        } else {
            None
        }
    }

    fn done(&self) -> bool {
        self.at >= self.items.len()
    }
}

fn kw(word: &str) -> TokenType {
    lex::kw(word)
}

fn p(text: &str) -> TokenType {
    lex::punct(text)
}

fn name_of(t: &Token) -> Name {
    Name {
        text: t.value.clone(),
        pos: t.pos,
    }
}

impl<'t> B<'t> {
    fn kids<'n>(&self, node: &'n Node) -> Kids<'n, 't> {
        Kids {
            items: &node.children,
            tokens: self.tokens,
            at: 0,
        }
    }

    /// The position of a subtree's first token.
    fn first_pos(&self, node: &Node) -> Pos {
        for child in &node.children {
            match child {
                Tree::Token(i) => return self.tokens[*i].pos,
                Tree::Rule(n) => {
                    if let Some(pos) = self.try_first_pos(n) {
                        return pos;
                    }
                }
            }
        }
        Pos::default()
    }

    fn try_first_pos(&self, node: &Node) -> Option<Pos> {
        for child in &node.children {
            match child {
                Tree::Token(i) => return Some(self.tokens[*i].pos),
                Tree::Rule(n) => {
                    if let Some(pos) = self.try_first_pos(n) {
                        return Some(pos);
                    }
                }
            }
        }
        None
    }

    fn statement(&self, node: &Node) -> Statement {
        let mut k = self.kids(node);
        let inner = k.rule();
        let inner = self.kids(inner).rule();
        match super::rule_name(inner.rule) {
            "query_statement" => self.query_statement(inner),
            "update_statement" => self.update(inner),
            "delete_statement" => self.delete(inner),
            "insert_statement" => self.insert(inner),
            "declare_cursor_statement" => {
                let mut k = self.kids(inner);
                k.token();
                let name = name_of(k.token());
                k.token();
                k.token();
                Statement::DeclareCursor {
                    name,
                    query: self.query(k.rule()),
                }
            }
            "deallocate_cursor_statement" | "open_cursor_statement" | "close_cursor_statement" => {
                let mut k = self.kids(inner);
                k.token();
                Statement::Cursor {
                    name: name_of(k.token()),
                }
            }
            "fetch_cursor_statement" => {
                let mut k = self.kids(inner);
                k.token();
                k.rule();
                k.eat(kw("FROM"));
                Statement::Cursor {
                    name: name_of(k.token()),
                }
            }
            "truncate_table_statement" => {
                let mut k = self.kids(inner);
                k.token();
                k.token();
                Statement::Ddl {
                    kind: DdlKind::Truncate,
                    table: name_of(k.token()),
                    column: None,
                }
            }
            "create_table_statement" => self.ddl(inner, DdlKind::CreateTable),
            "drop_table_statement" => self.ddl(inner, DdlKind::DropTable),
            "create_index_statement" => self.ddl(inner, DdlKind::CreateIndex),
            "drop_index_statement" => self.ddl(inner, DdlKind::DropIndex),
            "alter_table_statement" => self.ddl(inner, DdlKind::AlterTable),
            other => unreachable!("statement rule {other}"),
        }
    }

    /// A DDL statement: the first IDENTIFIER is the table; index forms and
    /// `ALTER TABLE ... DROP/ALTER COLUMN` name a column after it.
    fn ddl(&self, node: &Node, kind: DdlKind) -> Statement {
        let mut names = Vec::new();
        self.identifiers(node, &mut names, kind == DdlKind::AlterTable);
        let mut names = names.into_iter();
        let table = names.next().expect("a table name");
        let column = match kind {
            DdlKind::CreateIndex | DdlKind::DropIndex => names.next(),
            DdlKind::AlterTable => {
                let action = self.kids(node).items.last().and_then(|t| match t {
                    Tree::Rule(n) => Some(n),
                    Tree::Token(_) => None,
                });
                action.and_then(|a| {
                    let mut k = self.kids(a);
                    let first = k.token().ty;
                    if first == kw("DROP") || first == kw("ALTER") {
                        k.eat(kw("COLUMN"));
                        k.peek_token().map(name_of)
                    } else {
                        None
                    }
                })
            }
            _ => None,
        };
        Statement::Ddl {
            kind,
            table,
            column,
        }
    }

    fn identifiers(&self, node: &Node, out: &mut Vec<Name>, shallow: bool) {
        for child in &node.children {
            match child {
                Tree::Token(i) if self.tokens[*i].ty == lex::IDENTIFIER => {
                    out.push(name_of(&self.tokens[*i]))
                }
                Tree::Rule(n) if !shallow => self.identifiers(n, out, false),
                _ => {}
            }
        }
    }

    fn update(&self, node: &Node) -> Statement {
        let mut k = self.kids(node);
        k.token();
        let table = name_of(k.token());
        k.token();
        let mut assignments = Vec::new();
        let mut filter = None;
        let mut cursor = None;
        while !k.done() {
            if k.eat(p(",")).is_some() {
                continue;
            }
            let n = k.rule();
            match super::rule_name(n.rule) {
                "update_assignment" => {
                    let mut a = self.kids(n);
                    let column = name_of(a.token());
                    if a.eat(p("[")).is_some() {
                        a.rule();
                        a.token();
                    }
                    a.token();
                    if a.eat(kw("PUTAS")).is_some() {
                        a.token();
                    }
                    assignments.push((column, self.value_expression(a.rule())));
                }
                "where_clause" => filter = Some(self.where_clause(n)),
                "where_cursor" => {
                    let mut c = self.kids(n);
                    c.token();
                    c.token();
                    c.token();
                    cursor = Some(name_of(c.token()));
                }
                other => unreachable!("update child {other}"),
            }
        }
        Statement::Update {
            table,
            assignments,
            filter,
            cursor,
        }
    }

    fn delete(&self, node: &Node) -> Statement {
        let mut k = self.kids(node);
        k.token();
        k.token();
        let table = name_of(k.token());
        let mut filter = None;
        let mut cursor = None;
        if let Some(n) = k.eat_rule("where_clause") {
            filter = Some(self.where_clause(n));
        } else if let Some(n) = k.eat_rule("where_cursor") {
            let mut c = self.kids(n);
            c.token();
            c.token();
            c.token();
            cursor = Some(name_of(c.token()));
        }
        Statement::Delete {
            table,
            filter,
            cursor,
        }
    }

    fn insert(&self, node: &Node) -> Statement {
        let mut k = self.kids(node);
        k.token();
        k.token();
        let table = name_of(k.token());
        let mut columns = Vec::new();
        if k.eat(p("(")).is_some() {
            let cols = k.rule();
            let mut c = self.kids(cols);
            while !c.done() {
                if c.eat(p(",")).is_some() {
                    continue;
                }
                let item = c.rule();
                columns.push(name_of(self.kids(item).token()));
            }
            k.token();
        }
        let source = if k.eat(kw("DEFAULT")).is_some() {
            InsertSource::Default
        } else if let Some(values) = k.eat_rule("insert_value_source") {
            let mut v = self.kids(values);
            v.token();
            let mut rows = Vec::new();
            while !v.done() {
                if v.eat(p(",")).is_some() {
                    continue;
                }
                let row = v.rule();
                let mut r = self.kids(row);
                let open = r.token().pos;
                let mut items = Vec::new();
                while !r.done() {
                    if r.eat(p(",")).is_some() || r.eat(p(")")).is_some() {
                        continue;
                    }
                    let element = r.rule();
                    let mut e = self.kids(element);
                    if e.eat(kw("PUTAS")).is_some() {
                        e.token();
                    }
                    items.push(self.value_expression(e.rule()));
                }
                rows.push((open, items));
            }
            InsertSource::Values(rows)
        } else {
            InsertSource::Query(self.query_expression(k.rule()))
        };
        Statement::Insert {
            table,
            columns,
            source,
        }
    }

    fn query_statement(&self, node: &Node) -> Statement {
        Statement::Query(self.query(node))
    }

    fn query(&self, node: &Node) -> Query {
        let mut k = self.kids(node);
        let body = self.query_expression(k.rule());
        let mut query = Query {
            body,
            order_by: Vec::new(),
            offset: None,
            fetch: None,
        };
        if let Some(n) = k.eat_rule("order_by") {
            let mut o = self.kids(n);
            o.token();
            o.token();
            while !o.done() {
                if o.eat(p(",")).is_some() {
                    continue;
                }
                let spec = o.rule();
                let mut s = self.kids(spec);
                let expr = self.value_expression(s.rule());
                let descending = s.eat(kw("DESC")).is_some();
                query.order_by.push(OrderSpec { expr, descending });
            }
        }
        if let Some(n) = k.eat_rule("result_offset") {
            let mut o = self.kids(n);
            let pos = o.token().pos;
            query.offset = Some(Offset {
                pos,
                count: self.constant(o.rule()),
            });
        }
        if let Some(n) = k.eat_rule("fetch_first") {
            let mut f = self.kids(n);
            let pos = f.token().pos;
            let next = f.token().ty == kw("NEXT");
            let count = f.eat_rule("constant").map(|c| self.constant(c));
            let percent = f.eat(kw("PERCENT")).is_some();
            f.token();
            let with_ties = f.eat(kw("WITH")).is_some();
            query.fetch = Some(Fetch {
                pos,
                next,
                count,
                percent,
                with_ties,
            });
        }
        query
    }

    fn query_expression(&self, node: &Node) -> QueryExpr {
        crate::stack::guard(|| self.query_expression_inner(node))
    }

    fn query_expression_inner(&self, node: &Node) -> QueryExpr {
        let mut k = self.kids(node);
        let first = self.query_term(k.rule());
        let mut unions = Vec::new();
        while let Some(u) = k.eat_rule("query_expression_union") {
            let mut uk = self.kids(u);
            uk.token();
            let all = uk
                .eat_rule("query_dup_opt")
                .is_some_and(|d| self.kids(d).token().ty == kw("ALL"));
            unions.push(Union {
                all,
                term: self.query_term(uk.rule()),
            });
        }
        QueryExpr { first, unions }
    }

    fn query_term(&self, node: &Node) -> QueryTerm {
        let mut k = self.kids(node);
        if let Some(t) = k.eat(kw("TABLE")) {
            return QueryTerm::Table { pos: t.pos };
        }
        if k.eat(p("(")).is_some() {
            return QueryTerm::Paren(Box::new(self.query_expression(k.rule())));
        }
        QueryTerm::Spec(Box::new(self.query_spec(k.rule())))
    }

    fn query_spec(&self, node: &Node) -> QuerySpec {
        let mut k = self.kids(node);
        let pos = k.token().pos;
        let distinct = k
            .eat_rule("query_dup_opt")
            .is_some_and(|d| self.kids(d).token().ty == kw("DISTINCT"));
        let select = self.select_list(k.rule());
        let from = self.table_list(k.rule());
        let filter = k.eat_rule("where_clause").map(|n| self.where_clause(n));
        let group_by = k.eat_rule("group_by_clause").map(|n| {
            let mut g = self.kids(n);
            let pos = g.token().pos;
            g.token();
            GroupBy {
                pos,
                columns: self.column_list(g.rule()),
            }
        });
        let having = k.eat_rule("having_clause").map(|n| {
            let mut h = self.kids(n);
            h.token();
            self.search_term(h.rule())
        });
        QuerySpec {
            pos,
            distinct,
            select,
            from,
            filter,
            group_by,
            having,
        }
    }

    fn where_clause(&self, node: &Node) -> Predicate {
        let mut k = self.kids(node);
        k.token();
        self.search_term(k.rule())
    }

    fn column_list(&self, node: &Node) -> Vec<ColumnRef> {
        let mut k = self.kids(node);
        let mut out = Vec::new();
        while !k.done() {
            if k.eat(p(",")).is_some() {
                continue;
            }
            out.push(self.column_reference(k.rule()));
        }
        out
    }

    fn select_list(&self, node: &Node) -> SelectList {
        let mut k = self.kids(node);
        if let Some(t) = k.eat(p("*")) {
            return SelectList::Star(t.pos);
        }
        let mut items = Vec::new();
        while !k.done() {
            if k.eat(p(",")).is_some() {
                continue;
            }
            items.push(self.select_item(k.rule()));
        }
        SelectList::Items(items)
    }

    fn alias(&self, k: &mut Kids<'_, 't>) -> Option<Name> {
        k.eat(kw("AS"));
        k.eat(lex::IDENTIFIER).map(name_of)
    }

    fn select_item(&self, node: &Node) -> SelectItem {
        let mut k = self.kids(node);
        if k.peek_is(lex::IDENTIFIER) {
            let t = k.token();
            return SelectItem::TableStar(name_of(t));
        }
        if k.eat(kw("GETAS")).is_some() {
            k.token();
            let expr = self.value_expression(k.rule());
            k.token();
            let stream = k.eat(lex::STRING_CONST).map(name_of);
            k.eat(kw("DEFAULT"));
            k.token();
            let alias = self.alias(&mut k);
            return SelectItem::GetAs {
                expr,
                stream,
                alias,
            };
        }
        let expr = self.value_expression(k.rule());
        let alias = self.alias(&mut k);
        SelectItem::Expr { expr, alias }
    }

    fn table_list(&self, node: &Node) -> Vec<TableRef> {
        let mut k = self.kids(node);
        k.token();
        let mut out = Vec::new();
        while !k.done() {
            if k.eat(p(",")).is_some() {
                continue;
            }
            out.push(self.table_reference(k.rule()));
        }
        out
    }

    fn table_reference(&self, node: &Node) -> TableRef {
        crate::stack::guard(|| self.table_reference_inner(node))
    }

    fn table_reference_inner(&self, node: &Node) -> TableRef {
        let mut k = self.kids(node);
        let mut left = self.simple_table(k.rule());
        while let Some(j) = k.eat_rule("table_join_reference") {
            let mut jk = self.kids(j);
            let kind = if jk.eat(kw("LEFT")).is_some() {
                jk.eat(kw("OUTER"));
                JoinKind::Left
            } else if jk.eat(kw("CROSS")).is_some() {
                JoinKind::Cross
            } else {
                jk.eat(kw("INNER"));
                JoinKind::Inner
            };
            jk.token();
            let right = self.simple_table(jk.rule());
            let on = jk.eat(kw("ON")).map(|_| self.search_term(jk.rule()));
            left = TableRef::Join {
                left: Box::new(left),
                kind,
                right: Box::new(right),
                on,
            };
        }
        left
    }

    fn simple_table(&self, node: &Node) -> TableRef {
        let mut k = self.kids(node);
        if k.eat(p("(")).is_some() {
            return self.table_reference(k.rule());
        }
        let name = name_of(k.token());
        let alias = self.alias(&mut k);
        TableRef::Table { name, alias }
    }

    fn search_term(&self, node: &Node) -> Predicate {
        crate::stack::guard(|| self.search_term_inner(node))
    }

    fn search_term_inner(&self, node: &Node) -> Predicate {
        let mut k = self.kids(node);
        let mut terms = Vec::new();
        while !k.done() {
            if k.eat(kw("OR")).is_some() {
                continue;
            }
            terms.push(self.boolean_term(k.rule()));
        }
        if terms.len() == 1 {
            terms.pop().expect("one term")
        } else {
            Predicate::Or(terms)
        }
    }

    fn boolean_term(&self, node: &Node) -> Predicate {
        let mut k = self.kids(node);
        let mut factors = Vec::new();
        while !k.done() {
            if k.eat(kw("AND")).is_some() {
                continue;
            }
            factors.push(self.boolean_factor(k.rule()));
        }
        if factors.len() == 1 {
            factors.pop().expect("one factor")
        } else {
            Predicate::And(factors)
        }
    }

    fn boolean_factor(&self, node: &Node) -> Predicate {
        let mut k = self.kids(node);
        let negated = k.eat(kw("NOT")).is_some();
        let primary = self.boolean_primary(k.rule());
        if negated {
            Predicate::Not(Box::new(primary))
        } else {
            primary
        }
    }

    fn list_or_query(&self, node: &Node) -> ListOrQuery {
        match super::rule_name(node.rule) {
            "value_expression_list" => ListOrQuery::List(self.expression_list(node)),
            _ => ListOrQuery::Query(self.subquery(node)),
        }
    }

    fn boolean_primary(&self, node: &Node) -> Predicate {
        let mut k = self.kids(node);
        if let Some(t) = k.eat(kw("EXISTS")).or_else(|| k.eat(kw("UNIQUE"))) {
            return Predicate::Exists {
                unique: t.ty == kw("UNIQUE"),
                query: self.subquery(k.rule()),
                pos: t.pos,
            };
        }
        if k.eat(p("(")).is_some() {
            return self.search_term(k.rule());
        }
        let left = self.value_expression(k.rule());
        if let Some(c) = k.eat_rule("comparison") {
            let op_token = self.kids(c).token();
            let op = compare_op(op_token.ty);
            if let Some(q) = k.eat_rule("quantifier_op") {
                let quantifier = if self.kids(q).token().ty == kw("ALL") {
                    Quantifier::All
                } else {
                    Quantifier::Any
                };
                k.token();
                let list = self.list_or_query(k.rule());
                return Predicate::Quantified {
                    left,
                    op,
                    quantifier,
                    list,
                    op_pos: op_token.pos,
                };
            }
            let right = self.value_expression(k.rule());
            return Predicate::Compare {
                left,
                op,
                right,
                op_pos: op_token.pos,
            };
        }
        if let Some(t) = k.eat(kw("IS")) {
            let negated = k.eat(kw("NOT")).is_some();
            let what = match k.token().ty {
                ty if ty == kw("FALSE") => IsWhat::False,
                ty if ty == kw("NULL") => IsWhat::Null,
                ty if ty == kw("TRUE") => IsWhat::True,
                _ => IsWhat::Unknown,
            };
            return Predicate::Is {
                expr: left,
                negated,
                what,
                pos: t.pos,
            };
        }
        if k.peek_is(p("+")) || k.peek_is(p("-")) {
            let pos = k.token().pos;
            return Predicate::Interval { expr: left, pos };
        }
        let negated = k.eat(kw("NOT")).is_some();
        let t = k.token();
        if t.ty == kw("IN") {
            k.token();
            let list = self.list_or_query(k.rule());
            return Predicate::In {
                left,
                negated,
                list,
                pos: t.pos,
            };
        }
        if t.ty == kw("BETWEEN") {
            let low = self.value_expression(k.rule());
            k.token();
            let high = self.value_expression(k.rule());
            return Predicate::Between {
                expr: left,
                negated,
                low,
                high,
                pos: t.pos,
            };
        }
        if t.ty == kw("LIKE") {
            let pattern = self.value_expression(k.rule());
            return Predicate::Like {
                expr: left,
                negated,
                pattern,
                pos: t.pos,
            };
        }
        // PART OF.
        k.token();
        let whole = self.value_expression(k.rule());
        Predicate::PartOf {
            part: left,
            negated,
            whole,
            pos: t.pos,
        }
    }

    fn expression_list(&self, node: &Node) -> Vec<Arg> {
        let mut k = self.kids(node);
        let mut out = Vec::new();
        while !k.done() {
            if k.eat(p(",")).is_some() {
                continue;
            }
            out.push(self.common(k.rule()));
        }
        out
    }

    fn common(&self, node: &Node) -> Arg {
        let inner = self.kids(node).rule();
        match super::rule_name(inner.rule) {
            "search_term" => Arg::Predicate(self.search_term(inner)),
            _ => Arg::Value(self.value_expression(inner)),
        }
    }

    fn subquery(&self, node: &Node) -> QueryExpr {
        let mut k = self.kids(node);
        if k.eat(p("(")).is_some() {
            return self.query_expression(k.rule());
        }
        self.query_expression(k.rule())
    }

    fn value_expression(&self, node: &Node) -> Expr {
        crate::stack::guard(|| self.value_expression_inner(node))
    }

    fn value_expression_inner(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let mut expr = self.value_term(k.rule());
        while !k.done() {
            let op = k.token();
            if k.eat(kw("INTERVAL")).is_some() {
                let amount = self.value_term(k.rule());
                let unit = date_part(self.kids(k.rule()).token().ty);
                let pos = expr.pos;
                expr = Expr {
                    kind: ExprKind::Interval {
                        expr: Box::new(expr),
                        subtract: op.ty == p("-"),
                        amount: Box::new(amount),
                        unit,
                    },
                    pos,
                };
                continue;
            }
            let right = self.value_term(k.rule());
            let bin = match op.ty {
                ty if ty == p("+") => BinOp::Add,
                ty if ty == p("-") => BinOp::Subtract,
                _ => BinOp::Concat,
            };
            expr = binary(bin, expr, right);
        }
        expr
    }

    fn value_term(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let mut expr = self.value_pow(k.rule());
        while !k.done() {
            let op = if k.token().ty == p("/") {
                BinOp::Divide
            } else {
                BinOp::Multiply
            };
            let right = self.value_pow(k.rule());
            expr = binary(op, expr, right);
        }
        expr
    }

    fn value_pow(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let mut expr = self.value_factor(k.rule());
        while !k.done() {
            k.token();
            let right = self.value_factor(k.rule());
            expr = binary(BinOp::Power, expr, right);
        }
        expr
    }

    fn value_factor(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let sign = if k.peek_is(p("+")) || k.peek_is(p("-")) {
            Some(k.token())
        } else {
            None
        };
        let primary = self.value_primary(k.rule());
        match sign {
            Some(t) if t.ty == p("-") => Expr {
                kind: ExprKind::Negate(Box::new(primary)),
                pos: t.pos,
            },
            Some(t) => Expr {
                kind: ExprKind::Plus(Box::new(primary)),
                pos: t.pos,
            },
            None => primary,
        }
    }

    fn value_primary(&self, node: &Node) -> Expr {
        let pos = self.first_pos(node);
        let mut k = self.kids(node);
        let kind = if let Some(t) = k.eat(kw("NULLIF")) {
            let _ = t;
            k.token();
            let a = self.value_expression(k.rule());
            k.token();
            let b = self.value_expression(k.rule());
            ExprKind::Nullif(Box::new(a), Box::new(b))
        } else if k.eat(kw("COALESCE")).is_some() {
            k.token();
            ExprKind::Coalesce(self.expression_list(k.rule()))
        } else if k.eat(p("(")).is_some() {
            ExprKind::Paren(Box::new(self.value_expression(k.rule())))
        } else {
            let inner = k.rule();
            return match super::rule_name(inner.rule) {
                "column_reference" => Expr {
                    kind: ExprKind::Column(self.column_reference(inner)),
                    pos,
                },
                "constant" => self.constant(inner),
                "case_expression" => self.case(inner),
                "cast_expression" => self.cast(inner),
                "aggregation_function" => self.aggregate(inner),
                "sql_function" => self.function(inner),
                "subquery" => Expr {
                    kind: ExprKind::Subquery(Box::new(self.subquery(inner))),
                    pos,
                },
                other => unreachable!("value_primary child {other}"),
            };
        };
        Expr { kind, pos }
    }

    fn column_reference(&self, node: &Node) -> ColumnRef {
        let mut k = self.kids(node);
        let first = k.peek_token().expect("a token").pos;
        let mut table = None;
        if k.peek_is(lex::IDENTIFIER) {
            let t = k.token();
            if k.eat(p(".")).is_some() {
                table = Some(name_of(t));
            } else {
                // An unqualified column.
                let repetition = k
                    .eat(p("["))
                    .map(|_| Box::new(self.value_expression(k.rule())));
                return ColumnRef {
                    table: None,
                    column: Some(name_of(t)),
                    repetition,
                    pos: first,
                    column_pos: t.pos,
                };
            }
        }
        let t = k.token();
        if t.ty == kw("ROWID") || t.ty == lex::QUOTED_ROWID {
            return ColumnRef {
                table,
                column: None,
                repetition: None,
                pos: first,
                column_pos: t.pos,
            };
        }
        let repetition = k
            .eat(p("["))
            .map(|_| Box::new(self.value_expression(k.rule())));
        ColumnRef {
            table,
            column: Some(name_of(t)),
            repetition,
            pos: first,
            column_pos: t.pos,
        }
    }

    fn constant(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let t = k.token();
        let literal = |l| ExprKind::Literal(l);
        let kind = match t.ty {
            ty if ty == lex::FLOAT_CONST || ty == lex::UNSIGNED_CONST => {
                literal(Literal::Number(t.text.clone()))
            }
            ty if ty == lex::STRING_CONST => literal(Literal::Text(t.value.clone())),
            ty if ty == p("?") => ExprKind::Param,
            ty if ty == p(":") => ExprKind::Named(name_of(k.token())),
            ty if ty == kw("DATE") || ty == kw("TIME") || ty == kw("TIMESTAMP") => {
                let s = k.token();
                let (v, pos) = (s.value.clone(), s.pos);
                literal(if ty == kw("DATE") {
                    Literal::Date(v, pos)
                } else if ty == kw("TIME") {
                    Literal::Time(v, pos)
                } else {
                    Literal::Timestamp(v, pos)
                })
            }
            ty if ty == lex::BIT_CONST => literal(Literal::Bit(t.text.clone())),
            ty if ty == lex::HEX_CONST => literal(Literal::Hex(t.text.clone())),
            ty if ty == kw("DEFAULT") => ExprKind::Default,
            ty if ty == kw("NULL") => literal(Literal::Null),
            ty if ty == kw("TRUE") => literal(Literal::True),
            _ => literal(Literal::False),
        };
        Expr { kind, pos: t.pos }
    }

    fn case(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let pos = k.token().pos;
        let operand = k
            .eat_rule("value_expression")
            .map(|n| Box::new(self.value_expression(n)));
        let mut whens = Vec::new();
        let mut otherwise = None;
        while let Some(n) = k.peek_rule() {
            k.at += 1;
            match super::rule_name(n.rule) {
                "case_simple_when_then" | "case_searched_when_then" => {
                    let mut w = self.kids(n);
                    w.token();
                    let cond = w.rule();
                    let cond = match super::rule_name(cond.rule) {
                        "search_term" => Arg::Predicate(self.search_term(cond)),
                        _ => self.common(cond),
                    };
                    w.token();
                    whens.push((cond, self.value_expression(w.rule())));
                }
                "case_else" => {
                    let mut e = self.kids(n);
                    if let Some(t) = e.eat(kw("ELSE")) {
                        let value = e
                            .eat_rule("value_expression")
                            .map(|v| Box::new(self.value_expression(v)));
                        otherwise = Some((t.pos, value));
                    }
                }
                other => unreachable!("case child {other}"),
            }
        }
        Expr {
            kind: ExprKind::Case {
                operand,
                whens,
                otherwise,
            },
            pos,
        }
    }

    fn cast(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let pos = k.token().pos;
        k.token();
        let expr = self.value_expression(k.rule());
        k.token();
        let ty = self.kids(k.rule()).token().ty;
        let ty = match ty {
            t if t == kw("INT") => CastType::Int,
            t if t == kw("DECIMAL") || t == kw("NUMERIC") => CastType::Decimal,
            t if t == kw("BOOLEAN") => CastType::Boolean,
            t if t == kw("DATE") => CastType::Date,
            t if t == kw("TIME") => CastType::Time,
            t if t == kw("TIMESTAMP") => CastType::Timestamp,
            t if t == kw("VARCHAR") || t == kw("CHARACTER") => CastType::Varchar,
            _ => CastType::Binary,
        };
        Expr {
            kind: ExprKind::Cast {
                expr: Box::new(expr),
                ty,
            },
            pos,
        }
    }

    fn aggregate(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let t = k.token();
        let func = match t.ty {
            ty if ty == kw("AVG") => Aggregate::Avg,
            ty if ty == kw("COUNT") => Aggregate::Count,
            ty if ty == kw("EVERY") => Aggregate::Every,
            ty if ty == kw("MAX") => Aggregate::Max,
            ty if ty == kw("MIN") => Aggregate::Min,
            ty if ty == kw("SOME") => Aggregate::Some,
            _ => Aggregate::Sum,
        };
        k.token();
        if k.eat(p("*")).is_some() {
            return Expr {
                kind: ExprKind::Aggregate {
                    func,
                    distinct: false,
                    arg: None,
                },
                pos: t.pos,
            };
        }
        let distinct = k.eat(kw("DISTINCT")).is_some();
        k.eat(kw("ALL"));
        let arg = self.value_expression(k.rule());
        Expr {
            kind: ExprKind::Aggregate {
                func,
                distinct,
                arg: Some(Box::new(arg)),
            },
            pos: t.pos,
        }
    }

    fn function(&self, node: &Node) -> Expr {
        let mut k = self.kids(node);
        let pos = self.first_pos(node);
        if k.eat(kw("EXTRACT")).is_some() {
            k.token();
            let part = date_part(self.kids(k.rule()).token().ty);
            k.token();
            let expr = self.value_expression(k.rule());
            return Expr {
                kind: ExprKind::Extract {
                    part,
                    expr: Box::new(expr),
                },
                pos,
            };
        }
        if k.eat(kw("TRIM")).is_some() {
            k.token();
            let side = k
                .eat_rule("trim_option")
                .map(|o| match self.kids(o).token().ty {
                    t if t == kw("LEADING") => TrimSide::Leading,
                    t if t == kw("TRAILING") => TrimSide::Trailing,
                    _ => TrimSide::Both,
                });
            let first = k
                .eat_rule("value_expression")
                .map(|e| self.value_expression(e));
            let (chars, expr) = if k.eat(kw("FROM")).is_some() {
                (first.map(Box::new), self.value_expression(k.rule()))
            } else {
                (None, first.expect("TRIM's argument"))
            };
            return Expr {
                kind: ExprKind::Trim {
                    side,
                    chars,
                    expr: Box::new(expr),
                },
                pos,
            };
        }
        let name_token = match k.eat_rule("function_name") {
            Some(f) => self.kids(f).token(),
            None => k.token(),
        };
        let mut name = name_of(name_token);
        if name_token.ty.is_keyword() {
            name.text = name_token.ty.name().to_string();
        }
        let parens = k.eat(p("(")).is_some();
        let args = k
            .eat_rule("value_expression_list")
            .map(|l| self.expression_list(l))
            .unwrap_or_default();
        Expr {
            kind: ExprKind::Function { name, args, parens },
            pos,
        }
    }
}

fn binary(op: BinOp, left: Expr, right: Expr) -> Expr {
    let pos = left.pos;
    Expr {
        kind: ExprKind::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        },
        pos,
    }
}

fn compare_op(ty: TokenType) -> CompareOp {
    match ty {
        t if t == lex::NEQ => CompareOp::Ne,
        t if t == p("=") => CompareOp::Eq,
        t if t == p("<") => CompareOp::Lt,
        t if t == p("<=") => CompareOp::Le,
        t if t == p(">") => CompareOp::Gt,
        _ => CompareOp::Ge,
    }
}

fn date_part(ty: TokenType) -> DatePart {
    match ty {
        t if t == kw("DAY") => DatePart::Day,
        t if t == kw("HOUR") => DatePart::Hour,
        t if t == kw("MINUTE") => DatePart::Minute,
        t if t == kw("MONTH") => DatePart::Month,
        t if t == kw("SECOND") => DatePart::Second,
        _ => DatePart::Year,
    }
}
