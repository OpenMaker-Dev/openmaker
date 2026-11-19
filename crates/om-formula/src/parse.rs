//! The parser and compiler (`DBCalcParse`, calc-encoding.md 5 and 6): source
//! text or stored formatted tokens to the Polish tree, with FileMaker's
//! precedence, argument checks, local variable slots, tail calls and
//! field-reference positions.
//!
//! The parser pulls tokens on demand, so a name is resolved against the
//! scope where it stands: a Let or While variable or a custom function
//! parameter before a function of the same name (`Let ( Abs = 1 ; Abs ( 1 )
//! )` is error 1212), a function only before `(`, then constants, then the
//! fields of the context table occurrence. An unknown name is reported as
//! soon as it is read, before any operator check (`1 foo` is error 106).

use std::ops::Range;

use crate::catalog::{self, Kind, NArgs, Opcode};
use crate::error::{ErrorCode, ParseError};
use crate::expr::Expr;
use crate::formatted::Token;
use crate::lex::{Lexeme, Name, Scanner};
use crate::names::Names;
use crate::number::{Decimal, Number};
use crate::operand::{Field, Key, PluginFunctionId, Symbol, Text};
use crate::polish;

/// What a formula is compiled against.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub names: &'a dyn Names,
    /// The context table occurrence: unqualified field names are its fields.
    /// `None` for formulas without one (custom functions, script steps in
    /// files whose formulas have occurrence 0).
    pub occurrence: Option<u64>,
    /// The parameters when compiling a custom function body; they are
    /// local slots 0, 1, ...
    pub parameters: &'a [String],
    /// `Self` is allowed (`kSupportSelf`: field calculations, conditional
    /// formatting); elsewhere it is error 1225.
    pub support_self: bool,
    /// The file's locale writes decimals with a comma (`1,5`).
    pub decimal_comma: bool,
}

impl<'a> Context<'a> {
    /// No context occurrence, no parameters, `Self` not allowed, a decimal
    /// point.
    pub fn new(names: &'a dyn Names) -> Self {
        Self {
            names,
            occurrence: None,
            parameters: &[],
            support_self: false,
            decimal_comma: false,
        }
    }
}

/// Source text compiled as FileMaker stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compiled {
    /// The formatted stream (child 5).
    pub tokens: Vec<Token>,
    /// The Polish tree (child 6).
    pub expr: Expr,
}

/// Compile formula text: tokenize it as FileMaker stores it and compile it
/// to the Polish tree. Errors carry FileMaker's error number and the range
/// FileMaker selects, in UTF-16 code units of `source` (6.4).
pub fn compile(source: &str, ctx: &Context<'_>) -> std::result::Result<Compiled, ParseError> {
    let utf16 = |byte: usize| source[..byte].encode_utf16().count();
    let mut parser = Parser::new(
        TextSource {
            scanner: Scanner::new(source, ctx.decimal_comma),
            tokens: Vec::new(),
            pending: None,
            last_end: 0,
        },
        ctx,
    );
    let compiled = parser.formula().and_then(|expr| {
        let mut source = parser.src;
        // Trailing whitespace and comments belong to the formatted stream.
        source.flush_trivia()?;
        Ok(Compiled {
            tokens: source.tokens,
            expr,
        })
    });
    compiled.map_err(|e| ParseError {
        code: e.code,
        span: utf16(e.span.start)..utf16(e.span.end),
    })
}

/// Compile a stored formatted stream to the Polish tree, as FileMaker does
/// when it saves a formula. Error spans are token indexes.
pub fn compile_tokens(
    tokens: &[Token],
    ctx: &Context<'_>,
) -> std::result::Result<Expr, ParseError> {
    Parser::new(TokenSource { tokens, pos: 0 }, ctx)
        .formula()
        .map_err(|e| *e)
}

/// How the next name is read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Expr,
    /// A Let or While variable being declared.
    Binding,
    /// The selector inside `Get ( )`.
    Selector,
}

/// Locals in scope: names (case-folded) and their slots, innermost last.
#[derive(Default)]
struct Scope {
    names: Vec<(String, u16)>,
    slots: u16,
}

impl Scope {
    fn find(&self, name: &str) -> Option<u16> {
        let name = name.to_lowercase();
        self.names
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| *s)
    }
}

/// The parser's results. The error is boxed: a debug build gives every
/// temporary of a function its own stack slot, and each `?` holds several.
type Result<T> = std::result::Result<T, Box<ParseError>>;

fn fail<T>(code: ErrorCode, span: Range<usize>) -> Result<T> {
    Err(Box::new(ParseError { code, span }))
}

trait Source {
    /// The next significant token read for `mode`, `None` at the end.
    fn peek(&mut self, mode: Mode, scope: &Scope, ctx: &Context<'_>) -> Result<Option<Token>>;
    /// Consume the token last peeked.
    fn bump(&mut self);
    /// Where the next token stands (after a `peek`).
    fn here(&self) -> Range<usize>;
    /// Where the last consumed token ends.
    fn last_end(&self) -> usize;
    /// Where the formula ends, trailing whitespace excluded.
    fn end(&self) -> usize;
}

struct TokenSource<'t> {
    tokens: &'t [Token],
    pos: usize,
}

impl Source for TokenSource<'_> {
    fn peek(&mut self, _: Mode, _: &Scope, _: &Context<'_>) -> Result<Option<Token>> {
        while self.tokens.get(self.pos).is_some_and(Token::is_trivia) {
            self.pos += 1;
        }
        Ok(self.tokens.get(self.pos).cloned())
    }
    fn bump(&mut self) {
        self.pos += 1;
    }
    fn here(&self) -> Range<usize> {
        self.pos..self.pos + 1
    }
    fn last_end(&self) -> usize {
        self.pos
    }
    fn end(&self) -> usize {
        self.tokens.len()
    }
}

struct Pending {
    trivia: Vec<Token>,
    lexeme: Lexeme,
    span: Range<usize>,
    followed_by_paren: bool,
    resolved: Option<(Mode, Option<Token>)>,
}

struct TextSource<'s> {
    scanner: Scanner<'s>,
    tokens: Vec<Token>,
    pending: Option<Pending>,
    last_end: usize,
}

impl TextSource<'_> {
    fn scan(&mut self) -> Result<&mut Pending> {
        if self.pending.is_none() {
            let mut trivia = Vec::new();
            loop {
                let (lexeme, span) = self.scanner.next()?;
                match lexeme {
                    Lexeme::Token(t) if t.is_trivia() => trivia.push(t),
                    lexeme => {
                        let followed_by_paren =
                            matches!(lexeme, Lexeme::Name(_)) && self.scanner.followed_by_paren();
                        self.pending = Some(Pending {
                            trivia,
                            lexeme,
                            span,
                            followed_by_paren,
                            resolved: None,
                        });
                        break;
                    }
                }
            }
        }
        Ok(self.pending.as_mut().expect("scanned"))
    }

    fn flush_trivia(&mut self) -> Result<()> {
        let pending = self.scan()?;
        let trivia = std::mem::take(&mut pending.trivia);
        self.tokens.extend(trivia);
        Ok(())
    }
}

impl Source for TextSource<'_> {
    fn peek(&mut self, mode: Mode, scope: &Scope, ctx: &Context<'_>) -> Result<Option<Token>> {
        let pending = self.scan()?;
        if let Some((m, token)) = &pending.resolved {
            if *m == mode {
                return Ok(token.clone());
            }
        }
        let token = match &pending.lexeme {
            Lexeme::End => None,
            Lexeme::Token(t) => Some(t.clone()),
            Lexeme::Op(op) => Some(Token::Op(*op)),
            // A keyword that names a local is that local (`Let ( and = 1 ;
            // and )`), and a name where a binding is declared.
            Lexeme::Keyword(op, text) => {
                if mode == Mode::Binding || scope.find(text).is_some() {
                    Some(Token::Symbol(Symbol::new(text.as_str())))
                } else {
                    Some(Token::Op(*op))
                }
            }
            Lexeme::Number(n, format) => Some(Token::Number(n.clone(), *format)),
            Lexeme::Text(t) => Some(Token::Text(Text::plain(t.as_str()))),
            Lexeme::Variable(v) => Some(Token::Variable(v.clone())),
            Lexeme::Name(name) => {
                let token = resolve(name, mode, pending.followed_by_paren, scope, ctx)
                    .map_err(|(code, span)| ParseError { code, span })?;
                // FileMaker places a qualified field at its field name.
                if name.table.is_some() {
                    pending.span.start = name.name.span.start;
                }
                Some(token)
            }
        };
        pending.resolved = Some((mode, token.clone()));
        Ok(token)
    }

    fn bump(&mut self) {
        if let Some(p) = self.pending.take() {
            self.last_end = p.span.end;
            self.tokens.extend(p.trivia);
            if let Some((_, Some(token))) = p.resolved {
                self.tokens.push(token);
            }
        }
    }

    fn here(&self) -> Range<usize> {
        match &self.pending {
            Some(p) => p.span.clone(),
            None => self.scanner.pos..self.scanner.pos,
        }
    }

    fn last_end(&self) -> usize {
        self.last_end
    }

    fn end(&self) -> usize {
        self.scanner.end()
    }
}

/// Resolve a scanned name where it stands. An error points at the part of
/// the name that failed.
fn resolve(
    name: &Name,
    mode: Mode,
    followed_by_paren: bool,
    scope: &Scope,
    ctx: &Context<'_>,
) -> std::result::Result<Token, (ErrorCode, Range<usize>)> {
    let text = name.name.text.as_str();
    let span = name.name.span.clone();
    let symbol = || {
        let mut s = Symbol::new(text);
        // `${...}` is recorded in the symbol's flag (2.6).
        s.flag = u8::from(name.name.quoted);
        Token::Symbol(s)
    };
    if let Some(table) = &name.table {
        let occurrence = ctx
            .names
            .find_occurrence(&table.text)
            .ok_or((ErrorCode::TableMissing, table.span.clone()))?;
        let field = ctx
            .names
            .find_field(occurrence, text)
            .ok_or((ErrorCode::FieldMissing, span))?;
        let mut f = Field::new(occurrence, field, true);
        if name.name.quoted {
            f.flags |= Field::FIELD_BRACKETED;
        }
        if table.quoted {
            f.flags |= Field::TABLE_BRACKETED;
        }
        return Ok(Token::Field(f));
    }
    match mode {
        Mode::Binding => return Ok(symbol()),
        Mode::Selector => {
            return catalog::find_get_selector(text)
                .map(Token::GetSelector)
                .ok_or((ErrorCode::InvalidGetParameter, span))
        }
        Mode::Expr => {}
    }
    if scope.find(text).is_some() {
        return Ok(symbol());
    }
    let ops = catalog::named(text);
    let no_args = |op: &&Opcode| op.info().is_some_and(|i| i.nargs == NArgs::NoArgs);
    if !name.name.quoted {
        if followed_by_paren {
            if let Some(op) = ops
                .iter()
                .find(|op| !no_args(op))
                .or_else(|| ops.iter().find(no_args))
            {
                return Ok(Token::Op(*op));
            }
        } else if let Some(op) = ops.iter().find(no_args) {
            return Ok(Token::Op(*op));
        }
        // Without `(` only functions that need no arguments.
        let callable = |min: u16| followed_by_paren || min == 0;
        if let Some(id) = ctx.names.find_custom_function(text) {
            let min = ctx.names.custom_function(id).map_or(0, |f| f.min_args);
            if callable(min) {
                return Ok(Token::CustomFunction(Key::from_id(id)));
            }
        }
        if let Some(b) = catalog::find_builtin(text).filter(|b| callable(b.min_args)) {
            return Ok(Token::PluginFunction(b.id));
        }
        if let Some(id) = ctx.names.find_plugin_function(text) {
            let min = ctx.names.plugin_function(id).map_or(0, |f| f.min_args);
            if callable(min) {
                return Ok(Token::PluginFunction(id));
            }
        }
        if followed_by_paren {
            return Err((ErrorCode::FunctionNotFound, span));
        }
    }
    let occurrence = ctx
        .occurrence
        .ok_or((ErrorCode::TableMissing, span.clone()))?;
    let field = ctx
        .names
        .find_field(occurrence, text)
        .ok_or((ErrorCode::FieldMissing, span))?;
    let mut f = Field::new(occurrence, field, false);
    if name.name.quoted {
        f.flags |= Field::FIELD_BRACKETED;
    }
    Ok(Token::Field(f))
}

/// Binary operators by precedence level, loosest first (6.2).
fn binary_level(op: Opcode) -> Option<u8> {
    Some(match op {
        Opcode::OR | Opcode::XOR => 0,
        Opcode::AND => 1,
        Opcode::EQUAL
        | Opcode::NOT_EQUAL
        | Opcode::NOT_EQUAL_SIGN
        | Opcode::LESS
        | Opcode::LESS_OR_EQUAL
        | Opcode::LESS_OR_EQUAL_SIGN
        | Opcode::GREATER
        | Opcode::GREATER_OR_EQUAL
        | Opcode::GREATER_OR_EQUAL_SIGN => 2,
        Opcode::CONCAT => 3,
        Opcode::ADD | Opcode::SUBTRACT => 4,
        Opcode::MULTIPLY | Opcode::DIVIDE => 5,
        Opcode::POWER => 6,
        _ => return None,
    })
}

/// The aggregate functions, whose arguments are field references (5.3).
fn is_aggregate(op: Opcode) -> bool {
    matches!(op.0, 0x7f..=0x85 | 0x87 | 0xa0 | 0xa1)
}

/// Functions whose first argument must be a field reference (1214
/// otherwise): GetSummary, Extend, GetRepetition, Last, GetNthRecord, NPV.
fn has_field_parameter(op: Opcode) -> bool {
    op.info()
        .is_some_and(|i| matches!(i.nargs, NArgs::UnaryField | NArgs::BinaryField))
}

/// How many leading arguments are field-reference positions (5.3): both of
/// GetSummary's, the first of the other field functions and of Lookup and
/// LookupNext.
fn field_parameters(op: Opcode) -> usize {
    match op {
        Opcode::GET_SUMMARY => 2,
        Opcode::LOOKUP | Opcode::LOOKUP_NEXT => 1,
        _ => usize::from(has_field_parameter(op)),
    }
}

/// `DBCalcParse::IsFieldRefType`: what may stand where a field is required.
fn is_field_ref_type(e: &Expr) -> bool {
    match e {
        Expr::Field(_)
        | Expr::FieldRef(_)
        | Expr::Local(_)
        | Expr::Variable(_)
        | Expr::CustomCall { .. }
        | Expr::Case(_)
        | Expr::Choose { .. }
        | Expr::Evaluate { .. }
        | Expr::GetField { .. }
        | Expr::Let { .. }
        | Expr::While { .. } => true,
        Expr::Call { op, .. } => {
            matches!(*op, Opcode::IF | Opcode::SET_VARIABLE | Opcode::GET_VAR_REP)
                || matches!(op.0, 0x88..=0x8a)
        }
        _ => false,
    }
}

/// The number 1 as FileMaker writes it (a Case default's test, a variable's
/// default repetition).
fn one() -> Expr {
    Expr::Number(Number::Decimal(
        Decimal::parse_literal("1").expect("valid literal"),
    ))
}

/// A literal's Polish value: U+E004 becomes a space (5.6).
fn polish_text(text: Text) -> Text {
    if text.value.contains('\u{E004}') {
        Text {
            value: text.value.replace('\u{E004}', " "),
            style: text.style,
        }
    } else {
        text
    }
}

/// What a function call allows.
#[derive(Default)]
struct Arity {
    min: u16,
    /// `None` when unlimited; then a trailing `;` before `)` is accepted.
    max: Option<u16>,
    /// `( )` gives zero arguments (custom and plug-in functions).
    empty_parens: bool,
    /// Case: a trailing `;` only after a complete (test ; result) pair.
    pairs: bool,
    /// The first argument must be a field reference (1214 as soon as it is
    /// read).
    field_first: bool,
    /// Evaluate: the second argument may be a `[ ... ]` list of fields.
    field_list: bool,
    /// An aggregate: a lone argument must be a field reference.
    lone_field: bool,
    /// Custom and plug-in functions: counts are checked at the `)`, and a
    /// trailing `;` is always accepted.
    count_at_close: bool,
    /// GetSummary: the first argument must be a summary field.
    summary: bool,
}

enum BindingTarget {
    Name(String),
    Variable(String),
}

/// Deepest nesting the parser accepts, of its own recursion and of the tree
/// it builds. At most this many expressions are being read at once: the
/// formula and each parenthesized expression, argument, list item, binding
/// value and repetition index inside another. Every recursive path of the
/// parser passes through one, so this bounds the parser's stack. A tree
/// may have at most this many nodes on one path, what the Polish decoder
/// reads once stored ([`polish::MAX_DEPTH`] levels below the outermost
/// node); a node that would go deeper is refused as it is built. Binary and
/// prefix operators take no recursion, so they are bounded by the tree
/// alone. The deepest stored formula of the corpus is 54.
pub const MAX_NESTING: usize = polish::MAX_DEPTH + 1;

/// A parsed subtree and its depth, the nodes on its longest path (a leaf
/// is 1), carried up so that no pass walks the tree while it is parsed.
struct Parsed {
    expr: Expr,
    depth: usize,
}

/// Parsing functions return their subtree boxed. A debug build gives each
/// temporary of a function its own stack slot; a pointer in each, rather
/// than the tree's 72 bytes, keeps the frames on a recursive path small.
/// Functions that build a node take it boxed too (`boxed_local`), rather
/// than have a recursive caller unbox it.
type Node = Box<Parsed>;

fn leaf(expr: Expr) -> Node {
    Box::new(Parsed { expr, depth: 1 })
}

/// A node's parsed operands and the depth of the deepest.
#[derive(Default)]
struct Operands {
    exprs: Vec<Expr>,
    depth: usize,
}

impl Operands {
    #[allow(clippy::boxed_local)]
    fn push(&mut self, node: Node) {
        self.depth = self.depth.max(node.depth);
        self.exprs.push(node.expr);
    }

    fn len(&self) -> usize {
        self.exprs.len()
    }
}

/// A call's arguments so far and where the first stands.
#[derive(Default)]
struct Arguments {
    operands: Operands,
    first: Range<usize>,
}

/// A binary operator waiting for its right operand.
struct Waiting {
    lhs: Node,
    op: Opcode,
    level: u8,
}

/// What the arguments of a catalog function allow.
fn function_arity(op: Opcode) -> Arity {
    let info = op.info().expect("catalog opcode");
    Arity {
        min: info.min_args,
        max: (info.max_args != 0).then_some(info.max_args),
        pairs: op == Opcode::CASE,
        field_first: has_field_parameter(op),
        field_list: op == Opcode::EVALUATE,
        lone_field: is_aggregate(op),
        summary: op == Opcode::GET_SUMMARY,
        ..Arity::default()
    }
}

/// Construct a call from its parsed arguments.
fn function_expr(op: Opcode, mut args: Vec<Expr>) -> Expr {
    match op {
        Opcode::IF => {
            if args.len() == 2 {
                args.push(Expr::Empty);
            }
            Expr::call(op, args)
        }
        Opcode::CASE => {
            let mut pairs = Vec::new();
            let mut it = args.into_iter();
            while let Some(test) = it.next() {
                match it.next() {
                    Some(result) => pairs.push((test, result)),
                    None => pairs.push((one(), test)),
                }
            }
            Expr::Case(pairs)
        }
        Opcode::CHOOSE => {
            let test = Box::new(args.remove(0));
            Expr::Choose {
                test,
                results: args,
            }
        }
        Opcode::EVALUATE => {
            // The trigger fields are stored as further arguments, not as
            // a list (3.1).
            if let Some(Expr::List(_)) = args.get(1) {
                if let Some(Expr::List(items)) = args.pop() {
                    args.extend(items);
                }
            }
            Expr::Evaluate {
                args,
                need_field_ref: 0,
            }
        }
        Opcode::GET_FIELD => Expr::GetField {
            arg: Box::new(args.remove(0)),
            need_field_ref: 0,
        },
        _ => Expr::call(op, args),
    }
}

/// What starts an operand, consumed unless it is a custom or plug-in
/// function, which reads its own name.
enum Operand {
    Parenthesis,
    /// A built-in function, operator or constant.
    Function(Opcode),
    Custom(Key),
    Plugin(PluginFunctionId),
    /// A field or variable, which a repetition may follow.
    Field(Field),
    Variable(String),
    /// A literal or local, complete.
    Leaf(Node),
}

/// A function or constant that takes no arguments.
fn constant(op: Opcode) -> Node {
    leaf(Expr::call(op, Vec::new()))
}

struct Parser<'c, S> {
    src: S,
    ctx: &'c Context<'c>,
    scope: Scope,
    /// Active `expression` calls.
    nesting: usize,
}

// The recursive parsing functions keep their checks and node construction
// in separate functions: a debug build gives every temporary of a function
// its own stack slot, and only the frames on a recursive path stay on the
// stack while a nested expression is read.
impl<'c, S: Source> Parser<'c, S> {
    fn new(src: S, ctx: &'c Context<'c>) -> Self {
        let mut scope = Scope::default();
        for p in ctx.parameters {
            scope.names.push((p.to_lowercase(), scope.slots));
            scope.slots += 1;
        }
        Self {
            src,
            ctx,
            scope,
            nesting: 0,
        }
    }

    /// A node over operands `depth` deep; one past [`MAX_NESTING`] nodes
    /// on a path is error 1200.
    fn node(&self, expr: Expr, depth: usize) -> Result<Node> {
        if depth >= MAX_NESTING {
            return self.error(ErrorCode::NestingTooDeep);
        }
        Ok(Box::new(Parsed {
            expr,
            depth: depth + 1,
        }))
    }

    fn peek_mode(&mut self, mode: Mode) -> Result<Option<Token>> {
        self.src.peek(mode, &self.scope, self.ctx)
    }

    fn peek(&mut self) -> Result<Option<Token>> {
        self.peek_mode(Mode::Expr)
    }

    fn bump(&mut self) {
        self.src.bump();
    }

    fn error<T>(&self, code: ErrorCode) -> Result<T> {
        fail(code, self.src.here())
    }

    fn peek_is(&mut self, token: &Token) -> Result<bool> {
        Ok(self.peek()?.as_ref() == Some(token))
    }

    /// `peek_is` where a binding name may stand.
    fn peek_is_binding(&mut self, token: &Token) -> Result<bool> {
        Ok(self.peek_mode(Mode::Binding)?.as_ref() == Some(token))
    }

    /// Where the next operand starts.
    fn operand_start(&mut self) -> Result<usize> {
        self.peek()?;
        Ok(self.src.here().start)
    }

    /// Consume `)`: missing at the end is 1207, anything else 1212.
    fn close_paren(&mut self) -> Result<()> {
        match self.peek()? {
            Some(Token::RightParen) => {
                self.bump();
                Ok(())
            }
            None => self.error(ErrorCode::UnbalancedParenthesis),
            Some(_) => self.error(ErrorCode::OperatorExpected),
        }
    }

    fn open_paren(&mut self) -> Result<()> {
        match self.peek()? {
            Some(Token::LeftParen) => {
                self.bump();
                Ok(())
            }
            None => self.error(ErrorCode::UnexpectedEnd),
            Some(_) => self.error(ErrorCode::OperandExpected),
        }
    }

    /// The `;` before another part of Let or While: a missing part is
    /// 1201.
    fn separator(&mut self) -> Result<()> {
        match self.peek()? {
            Some(Token::Separator) => {
                self.bump();
                Ok(())
            }
            None | Some(Token::RightParen) => self.error(ErrorCode::TooFewParameters),
            Some(_) => self.error(ErrorCode::OperatorExpected),
        }
    }

    /// The `)` after the last part of Let or While: one more part is 1202.
    fn close_parts(&mut self) -> Result<()> {
        if self.peek_is(&Token::Separator)? {
            return self.error(ErrorCode::TooManyParameters);
        }
        self.close_paren()
    }

    fn formula(&mut self) -> Result<Expr> {
        let mut expr = if self.peek()?.is_none() {
            // A formula of only whitespace and comments.
            Expr::Empty
        } else {
            let e = self.expression()?;
            debug_assert_eq!(e.depth, e.expr.depth());
            if self.peek()?.is_some() {
                // FileMaker selects the rest of the formula.
                let start = self.src.here().start;
                return fail(ErrorCode::OperatorExpected, start..self.src.end());
            }
            e.expr
        };
        mark_tail_calls(&mut expr, true);
        mark_field_refs(&mut expr, false, false);
        Ok(expr)
    }

    /// One level of parser recursion, within [`MAX_NESTING`].
    fn expression(&mut self) -> Result<Node> {
        if self.nesting >= MAX_NESTING {
            return self.error(ErrorCode::NestingTooDeep);
        }
        self.nesting += 1;
        let node = self.binary();
        self.nesting -= 1;
        node
    }

    /// Operands joined by binary operators, all left-associative (6.2). An
    /// operator waits until one of its own or a looser level follows its
    /// right operand, so precedence takes no recursion.
    fn binary(&mut self) -> Result<Node> {
        let mut waiting = Vec::new();
        loop {
            let operand = self.unary()?;
            if let Some(node) = self.after_operand(&mut waiting, operand)? {
                return Ok(node);
            }
        }
    }

    /// Queue the binary operator that follows `operand`, or at the end of
    /// the operands join everything waiting.
    fn after_operand(&mut self, waiting: &mut Vec<Waiting>, operand: Node) -> Result<Option<Node>> {
        let next = match self.peek()? {
            Some(Token::Op(op)) => binary_level(op).map(|level| (op, level)),
            _ => None,
        };
        let Some((op, level)) = next else {
            return self.join(waiting, operand, 0).map(Some);
        };
        let lhs = self.join(waiting, operand, level)?;
        self.bump();
        waiting.push(Waiting { lhs, op, level });
        Ok(None)
    }

    /// Join the waiting operators of `level` or tighter, innermost first,
    /// with their right operands.
    fn join(&self, waiting: &mut Vec<Waiting>, mut rhs: Node, level: u8) -> Result<Node> {
        while waiting.last().is_some_and(|w| w.level >= level) {
            let Waiting { lhs, op, .. } = waiting.pop().expect("a waiting operator");
            let depth = lhs.depth.max(rhs.depth);
            rhs = self.node(Expr::call(op, vec![lhs.expr, rhs.expr]), depth)?;
        }
        Ok(rhs)
    }

    /// Prefix operators, which bind tighter than any binary operator, and
    /// their operand.
    fn unary(&mut self) -> Result<Node> {
        let prefixes = self.prefixes()?;
        let operand = self.primary()?;
        self.apply_prefixes(prefixes, operand)
    }

    /// Consume the prefix operators before an operand. Each is a node
    /// above it, so more than the tree allows are refused as they are read.
    fn prefixes(&mut self) -> Result<Vec<Opcode>> {
        let mut prefixes = Vec::new();
        while let Some(op) = self.unary_operator()? {
            self.bump();
            if prefixes.len() + 1 >= MAX_NESTING {
                return self.error(ErrorCode::NestingTooDeep);
            }
            prefixes.push(op);
        }
        Ok(prefixes)
    }

    fn apply_prefixes(&self, mut prefixes: Vec<Opcode>, mut operand: Node) -> Result<Node> {
        while let Some(op) = prefixes.pop() {
            operand = self.node(Expr::call(op, vec![operand.expr]), operand.depth)?;
        }
        Ok(operand)
    }

    fn unary_operator(&mut self) -> Result<Option<Opcode>> {
        Ok(match self.peek()? {
            Some(Token::Op(Opcode::NOT)) => Some(Opcode::NOT),
            Some(Token::Op(Opcode::SUBTRACT | Opcode::NEGATE)) => Some(Opcode::NEGATE),
            Some(Token::Op(Opcode::ADD)) => Some(Opcode::GET_AS_NUMBER),
            _ => None,
        })
    }

    fn primary(&mut self) -> Result<Node> {
        match self.operand()? {
            Operand::Parenthesis => self.parenthesized(),
            Operand::Function(op) => self.function(op),
            Operand::Custom(key) => self.custom_function(key),
            Operand::Plugin(id) => self.plugin_function(id),
            Operand::Field(f) => self.field(f),
            Operand::Variable(name) => self.variable(name),
            Operand::Leaf(node) => Ok(node),
        }
    }

    /// Read what starts an operand.
    fn operand(&mut self) -> Result<Operand> {
        let Some(token) = self.peek()? else {
            return self.error(ErrorCode::OperandExpected);
        };
        let operand = match token {
            Token::Op(Opcode::SELF) if !self.ctx.support_self => {
                return self.error(ErrorCode::NotSupported);
            }
            Token::Op(op) => {
                let kind = op.info().map(|i| i.kind);
                if op != Opcode::PARAGRAPH && !matches!(kind, Some(Kind::Function | Kind::Constant))
                {
                    return self.error(ErrorCode::OperandExpected);
                }
                Operand::Function(op)
            }
            Token::LeftParen => Operand::Parenthesis,
            // The function reads its own name.
            Token::CustomFunction(key) => return Ok(Operand::Custom(key)),
            Token::PluginFunction(id) => return Ok(Operand::Plugin(id)),
            Token::Number(n, _) => Operand::Leaf(leaf(Expr::Number(n))),
            Token::Text(t) => Operand::Leaf(leaf(Expr::Text(polish_text(t)))),
            Token::Field(f) => Operand::Field(f),
            Token::Symbol(s) => {
                // An unresolved name's error stands after it.
                self.bump();
                return Ok(match self.symbol(&s.name.value)? {
                    Expr::Field(f) => Operand::Field(f),
                    local => Operand::Leaf(leaf(local)),
                });
            }
            Token::Variable(name) => Operand::Variable(name),
            _ => return self.error(ErrorCode::OperandExpected),
        };
        self.bump();
        Ok(operand)
    }

    fn parenthesized(&mut self) -> Result<Node> {
        let e = self.expression()?;
        self.end_parenthesis()?;
        Ok(e)
    }

    /// The `)` of a parenthesized expression: anything else is 1207.
    fn end_parenthesis(&mut self) -> Result<()> {
        if !self.peek_is(&Token::RightParen)? {
            return self.error(ErrorCode::UnbalancedParenthesis);
        }
        self.bump();
        Ok(())
    }

    /// A stored name: a local, or else a field of the context occurrence,
    /// as it resolves when compiled (5.6).
    fn symbol(&self, name: &str) -> Result<Expr> {
        if let Some(slot) = self.scope.find(name) {
            return Ok(Expr::Local(slot));
        }
        let Some(occurrence) = self.ctx.occurrence else {
            return self.error(ErrorCode::TableMissing);
        };
        match self.ctx.names.find_field(occurrence, name) {
            Some(id) => Ok(Expr::Field(Field::new(occurrence, id, false))),
            None => self.error(ErrorCode::FieldMissing),
        }
    }

    /// A variable, or `$v[n]`, which reads with GetVarRep.
    fn variable(&mut self, name: String) -> Result<Node> {
        let repetition = self.repetition()?;
        self.variable_node(name, repetition)
    }

    fn variable_node(&self, name: String, repetition: Option<Node>) -> Result<Node> {
        let var = Expr::Variable(name);
        match repetition {
            Some(rep) => self.node(
                Expr::call(Opcode::GET_VAR_REP, vec![rep.expr, var]),
                rep.depth,
            ),
            None => Ok(leaf(var)),
        }
    }

    /// A field, or `field[n]`, which compiles to `GetRepetition` (5.4).
    fn field(&mut self, f: Field) -> Result<Node> {
        let repetition = self.repetition()?;
        self.field_node(f, repetition)
    }

    fn field_node(&self, f: Field, repetition: Option<Node>) -> Result<Node> {
        match repetition {
            Some(rep) => {
                let call = Expr::call(Opcode::GET_REPETITION, vec![Expr::FieldRef(f), rep.expr]);
                self.node(call, rep.depth)
            }
            None => Ok(leaf(Expr::Field(f))),
        }
    }

    /// The `[ n ]` after a field or variable, if any.
    fn repetition(&mut self) -> Result<Option<Node>> {
        if !self.peek_is(&Token::LeftBracket)? {
            return Ok(None);
        }
        self.index(ErrorCode::OperatorExpected).map(Some)
    }

    fn custom_function(&mut self, key: Key) -> Result<Node> {
        self.bump();
        let arity = self.custom_arity(&key);
        let args = self.optional_arguments(&arity)?;
        self.custom_node(key, args)
    }

    fn custom_node(&self, key: Key, args: Operands) -> Result<Node> {
        let call = Expr::CustomCall {
            tail: false,
            function: key,
            args: args.exprs,
        };
        self.node(call, args.depth)
    }

    fn custom_arity(&self, key: &Key) -> Arity {
        let f = key.id().and_then(|id| self.ctx.names.custom_function(id));
        Arity {
            min: f.map_or(0, |f| f.min_args),
            max: f.and_then(|f| f.max_args),
            empty_parens: true,
            count_at_close: true,
            ..Arity::default()
        }
    }

    fn plugin_function(&mut self, id: PluginFunctionId) -> Result<Node> {
        self.bump();
        let arity = self.plugin_arity(id);
        let args = self.optional_arguments(&arity)?;
        self.plugin_node(id, args)
    }

    fn plugin_node(&self, id: PluginFunctionId, args: Operands) -> Result<Node> {
        let call = Expr::PluginCall {
            function: id,
            args: args.exprs,
        };
        self.node(call, args.depth)
    }

    /// Whether a field argument is a summary field; anything else, and a
    /// field whose kind the names do not know, passes.
    fn is_summary(&self, arg: &Expr) -> bool {
        let Expr::Field(f) = arg else { return true };
        f.occurrence
            .id()
            .zip(f.field.id())
            .and_then(|(o, id)| self.ctx.names.is_summary_field(o, id))
            .unwrap_or(true)
    }

    fn plugin_arity(&self, id: PluginFunctionId) -> Arity {
        let (min, max) = match catalog::builtin(id) {
            Some(b) => (b.min_args, b.max_args),
            None => self
                .ctx
                .names
                .plugin_function(id)
                .map_or((0, None), |f| (f.min_args, f.max_args)),
        };
        Arity {
            min,
            max,
            empty_parens: true,
            count_at_close: true,
            ..Arity::default()
        }
    }

    /// `[ expr ]` after a field or variable; `other` is the error for a
    /// token where `]` belongs.
    fn index(&mut self, other: ErrorCode) -> Result<Node> {
        self.open_index()?;
        let e = self.expression()?;
        self.close_index(other)?;
        Ok(e)
    }

    fn open_index(&mut self) -> Result<()> {
        self.bump();
        if self.peek()?.is_none() {
            return self.error(ErrorCode::UnexpectedEnd);
        }
        Ok(())
    }

    fn close_index(&mut self, other: ErrorCode) -> Result<()> {
        match self.peek()? {
            Some(Token::RightBracket) => {
                self.bump();
                Ok(())
            }
            None => self.error(ErrorCode::UnbalancedParenthesis),
            Some(_) => self.error(other),
        }
    }

    /// Arguments of a custom or plug-in function: none without parentheses.
    fn optional_arguments(&mut self, arity: &Arity) -> Result<Operands> {
        if self.peek_is(&Token::LeftParen)? {
            self.arguments(arity)
        } else if arity.min > 0 {
            self.error(ErrorCode::TooFewParameters)
        } else {
            Ok(Operands::default())
        }
    }

    /// `( arg ; arg ... )` with FileMaker's count checks: a `;` after the
    /// last allowed argument is 1202, too few is 1201 at the `)`; a trailing
    /// `;` is accepted when the count is unlimited.
    fn arguments(&mut self, arity: &Arity) -> Result<Operands> {
        let mut args = Arguments::default();
        if self.open_arguments(arity)? {
            loop {
                let start = self.operand_start()?;
                let arg = if self.trigger_list(arity, args.operands.len())? {
                    self.list(None)?
                } else {
                    self.expression()?
                };
                if !self.argument(arity, &mut args, arg, start)? {
                    break;
                }
            }
        }
        self.close_arguments(arity, &args)?;
        Ok(args.operands)
    }

    /// Consume `(`; whether arguments follow.
    fn open_arguments(&mut self, arity: &Arity) -> Result<bool> {
        self.open_paren()?;
        Ok(!(arity.empty_parens && self.peek_is(&Token::RightParen)?))
    }

    /// Whether Evaluate's trigger fields follow as a `[ ... ]` list.
    fn trigger_list(&mut self, arity: &Arity, index: usize) -> Result<bool> {
        Ok(arity.field_list && index == 1 && self.peek_is(&Token::LeftBracket)?)
    }

    /// Check and keep the argument that starts at `start`; whether another
    /// follows.
    fn argument(
        &mut self,
        arity: &Arity,
        args: &mut Arguments,
        mut arg: Node,
        start: usize,
    ) -> Result<bool> {
        let span = start..self.src.last_end();
        let index = args.operands.len();
        if arity.field_list && index == 1 {
            match arg.expr {
                // The trigger fields are stored as further arguments, so
                // the list adds no level.
                Expr::List(_) => arg.depth -= 1,
                Expr::Field(_) => {}
                _ => return fail(ErrorCode::OperandExpected, span),
            }
        }
        if arity.field_first && index == 0 {
            if !is_field_ref_type(&arg.expr) {
                return fail(ErrorCode::FieldRequired, span);
            }
            if arity.summary && !self.is_summary(&arg.expr) {
                return fail(ErrorCode::SummaryFieldRequired, span);
            }
        }
        if index == 0 {
            args.first = span;
        }
        args.operands.push(arg);
        self.after_argument(arity, args.operands.len())
    }

    /// After `count` arguments: whether another follows a `;`.
    fn after_argument(&mut self, arity: &Arity, count: usize) -> Result<bool> {
        match self.peek()? {
            Some(Token::Separator) => {
                let full = arity.max.is_some_and(|m| count >= usize::from(m));
                if full && !arity.count_at_close {
                    return self.error(ErrorCode::TooManyParameters);
                }
                self.bump();
                let trailing = arity.count_at_close
                    || arity.max.is_none()
                        && count >= usize::from(arity.min)
                        && (!arity.pairs || count % 2 == 0);
                Ok(!(trailing && self.peek_is(&Token::RightParen)?))
            }
            Some(Token::RightParen) => Ok(false),
            None => self.error(ErrorCode::UnbalancedParenthesis),
            Some(_) => self.error(ErrorCode::OperatorExpected),
        }
    }

    /// The counts checked at the `)`, which is consumed.
    fn close_arguments(&mut self, arity: &Arity, args: &Arguments) -> Result<()> {
        let count = args.operands.len();
        if arity.lone_field && count == 1 && !is_field_ref_type(&args.operands.exprs[0]) {
            return fail(ErrorCode::FieldRequired, args.first.clone());
        }
        if count < usize::from(arity.min) {
            return self.error(ErrorCode::TooFewParameters);
        }
        if arity.max.is_some_and(|m| count > usize::from(m)) {
            return self.error(ErrorCode::TooManyParameters);
        }
        self.bump();
        Ok(())
    }

    /// `[ a ; b ; ... ]` as an argument, of exactly `size` items if given;
    /// otherwise Evaluate's trigger fields, which may be none.
    fn list(&mut self, size: Option<usize>) -> Result<Node> {
        let mut items = Operands::default();
        if self.open_list(size)? {
            loop {
                let start = self.operand_start()?;
                let item = self.expression()?;
                if !self.list_item(size, &mut items, item, start)? {
                    break;
                }
            }
        }
        self.list_node(items)
    }

    fn list_node(&self, items: Operands) -> Result<Node> {
        self.node(Expr::List(items.exprs), items.depth)
    }

    /// Consume `[`; whether items follow (an empty trigger list's `]` is
    /// consumed too).
    fn open_list(&mut self, size: Option<usize>) -> Result<bool> {
        self.bump();
        if size.is_none() && self.peek_is(&Token::RightBracket)? {
            self.bump();
            return Ok(false);
        }
        Ok(true)
    }

    /// Check and keep the item that starts at `start`; whether another
    /// follows (the `]` is consumed otherwise).
    fn list_item(
        &mut self,
        size: Option<usize>,
        items: &mut Operands,
        item: Node,
        start: usize,
    ) -> Result<bool> {
        if size.is_none() && !matches!(item.expr, Expr::Field(_)) {
            return fail(ErrorCode::OperandExpected, start..self.src.last_end());
        }
        items.push(item);
        match self.peek()? {
            Some(Token::Separator) => {
                if size.is_some_and(|n| items.len() >= n) {
                    return self.error(ErrorCode::TooManyParameters);
                }
                self.bump();
                Ok(true)
            }
            Some(Token::RightBracket) => {
                if size.is_some_and(|n| items.len() < n) {
                    return self.error(ErrorCode::TooFewParameters);
                }
                self.bump();
                Ok(false)
            }
            None => self.error(ErrorCode::UnbalancedParenthesis),
            Some(_) => self.error(ErrorCode::OperatorExpected),
        }
    }

    /// Substitute and JSONSetElement: the first argument, then either their
    /// `size` further parameters or any number of `[ ... ]` groups of
    /// `size` items; mixing the two is 1211.
    fn grouped_arguments(&mut self, size: usize) -> Result<Operands> {
        self.open_paren()?;
        let mut args = Operands::default();
        // Whether `[ ... ]` groups follow, known after the first argument.
        let mut grouped = None;
        loop {
            let arg = if grouped == Some(true) {
                self.list(Some(size))?
            } else {
                self.expression()?
            };
            args.push(arg);
            let more = match grouped {
                None => {
                    grouped = Some(self.first_group()?);
                    true
                }
                Some(grouped) => self.after_group(grouped, size, args.len())?,
            };
            if !more {
                return Ok(args);
            }
            self.group_operand(grouped == Some(true))?;
        }
    }

    /// The `;` after the first argument; whether `[ ... ]` groups follow.
    fn first_group(&mut self) -> Result<bool> {
        match self.peek()? {
            Some(Token::Separator) => self.bump(),
            Some(Token::RightParen) => return self.error(ErrorCode::TooFewParameters),
            None => return self.error(ErrorCode::UnbalancedParenthesis),
            Some(_) => return self.error(ErrorCode::OperatorExpected),
        }
        self.peek_is(&Token::LeftBracket)
    }

    /// Before a further parameter or group: the two cannot be mixed.
    fn group_operand(&mut self, grouped: bool) -> Result<()> {
        let is_list = self.peek_is(&Token::LeftBracket)?;
        if grouped && self.peek_is(&Token::RightParen)? {
            return self.error(ErrorCode::OperandExpected);
        }
        if is_list != grouped {
            return self.error(ErrorCode::ListNotAllowed);
        }
        Ok(())
    }

    /// After `count` arguments: whether another follows a `;` (the `)` is
    /// consumed otherwise).
    fn after_group(&mut self, grouped: bool, size: usize, count: usize) -> Result<bool> {
        match self.peek()? {
            Some(Token::Separator) if grouped || count <= size => {
                self.bump();
                Ok(true)
            }
            Some(Token::Separator) => self.error(ErrorCode::TooManyParameters),
            Some(Token::RightParen) if grouped || count == size + 1 => {
                self.bump();
                Ok(false)
            }
            Some(Token::RightParen) => self.error(ErrorCode::TooFewParameters),
            None => self.error(ErrorCode::UnbalancedParenthesis),
            Some(_) if grouped => self.error(ErrorCode::UnbalancedParenthesis),
            Some(_) => self.error(ErrorCode::OperatorExpected),
        }
    }

    fn function(&mut self, op: Opcode) -> Result<Node> {
        match op {
            Opcode::GET => self.get(),
            Opcode::LET => self.let_(),
            Opcode::WHILE => self.while_(),
            _ => self.function_arguments(op),
        }
    }

    fn function_arguments(&mut self, op: Opcode) -> Result<Node> {
        let info = op.info().expect("catalog opcode");
        if info.nargs == NArgs::NoArgs {
            return Ok(constant(op));
        }
        let args = match op {
            Opcode::SUBSTITUTE => self.grouped_arguments(2)?,
            Opcode::JSON_SET_ELEMENT => self.grouped_arguments(3)?,
            _ => self.arguments(&function_arity(op))?,
        };
        self.call_node(op, args)
    }

    fn call_node(&self, op: Opcode, args: Operands) -> Result<Node> {
        self.node(function_expr(op, args.exprs), args.depth)
    }

    /// `Get ( selector )`.
    fn get(&mut self) -> Result<Node> {
        self.open_paren()?;
        let selector = match self.peek_mode(Mode::Selector)? {
            Some(Token::GetSelector(sel)) => sel,
            Some(Token::RightParen) | None => return self.error(ErrorCode::OperandExpected),
            Some(_) => return self.error(ErrorCode::InvalidGetParameter),
        };
        self.bump();
        match self.peek()? {
            Some(Token::Separator) => return self.error(ErrorCode::TooManyParameters),
            _ => self.close_paren()?,
        }
        Ok(leaf(Expr::Get(selector)))
    }

    /// `Let ( bindings ; expression )`.
    fn let_(&mut self) -> Result<Node> {
        self.open_paren()?;
        let (mark, base) = (self.scope.names.len(), self.scope.slots);
        let bindings = self.bindings(base, &mut Vec::new(), &[])?;
        self.next_part(base + bindings.len() as u16)?;
        let body = self.expression()?;
        self.close_scope(mark, base)?;
        self.let_node(bindings, body)
    }

    #[allow(clippy::boxed_local)]
    fn let_node(&self, bindings: Operands, body: Node) -> Result<Node> {
        let depth = bindings.depth.max(body.depth);
        let expr = Expr::Let {
            bindings: bindings.exprs,
            body: Box::new(body.expr),
        };
        self.node(expr, depth)
    }

    /// `While ( initial ; condition ; logic ; result )`.
    fn while_(&mut self) -> Result<Node> {
        self.open_paren()?;
        let (mark, base) = (self.scope.names.len(), self.scope.slots);
        let initial = self.bindings(base, &mut Vec::new(), &[])?;
        let after_initial = base + initial.len() as u16;
        self.next_part(after_initial)?;
        let condition = self.expression()?;
        self.separator()?;
        let logic = self.while_logic(mark, after_initial)?;
        self.next_part(after_initial + logic.len() as u16)?;
        let result = self.expression()?;
        self.close_scope(mark, base)?;
        self.while_node(initial, condition, logic, result)
    }

    #[allow(clippy::boxed_local)]
    fn while_node(
        &self,
        initial: Operands,
        condition: Node,
        logic: Operands,
        result: Node,
    ) -> Result<Node> {
        let depth = [initial.depth, condition.depth, logic.depth, result.depth]
            .into_iter()
            .max()
            .unwrap_or_default();
        let expr = Expr::While {
            initial: initial.exprs,
            condition: Box::new(condition.expr),
            logic: logic.exprs,
            result: Box::new(result.expr),
        };
        self.node(expr, depth)
    }

    /// The `;` before the next part of Let or While, which has the locals
    /// below `slots` in scope.
    fn next_part(&mut self, slots: u16) -> Result<()> {
        self.separator()?;
        self.scope.slots = slots;
        Ok(())
    }

    /// The `)` after the last part of Let or While, which ends its scope.
    fn close_scope(&mut self, mark: usize, base: u16) -> Result<()> {
        self.close_parts()?;
        self.scope.names.truncate(mark);
        self.scope.slots = base;
        Ok(())
    }

    fn while_logic(&mut self, mark: usize, base: u16) -> Result<Operands> {
        // The logic list continues the slot numbering and treats every name
        // of the initial list as a repeat (5.5).
        let repeats = self.scope.names[mark..].to_vec();
        self.bindings(base, &mut Vec::new(), &repeats)
    }

    /// One binding or a bracketed list of them. Binding `i` has slot
    /// `base + i`; `declared` collects the names this list declares.
    fn bindings(
        &mut self,
        base: u16,
        declared: &mut Vec<(String, u16)>,
        repeats: &[(String, u16)],
    ) -> Result<Operands> {
        let mut out = Operands::default();
        let Some(in_list) = self.open_bindings()? else {
            return Ok(out);
        };
        loop {
            let slot = base + out.len() as u16;
            out.push(self.binding(slot, declared, repeats, in_list)?);
            if !in_list || !self.more_bindings()? {
                return Ok(out);
            }
        }
    }

    /// Whether the bindings are a `[ ... ]` list, whose `[` is consumed;
    /// `None` for an empty list `[ ]`.
    fn open_bindings(&mut self) -> Result<Option<bool>> {
        if !self.peek_is_binding(&Token::LeftBracket)? {
            return Ok(Some(false));
        }
        self.bump();
        if self.peek_is_binding(&Token::RightBracket)? {
            self.bump();
            return Ok(None);
        }
        Ok(Some(true))
    }

    fn more_bindings(&mut self) -> Result<bool> {
        match self.peek()? {
            Some(Token::Separator) => {
                self.bump();
                Ok(true)
            }
            Some(Token::RightBracket) => {
                self.bump();
                Ok(false)
            }
            _ => self.error(ErrorCode::OperandExpected),
        }
    }

    /// `name = value`, `$var[n] = value`; compiles to the value itself when
    /// it declares `name` in `slot`, else to `SetVariable` (5.5). Something
    /// else where the name belongs is 1211 inside `[ ]`, else 1204.
    fn binding(
        &mut self,
        slot: u16,
        declared: &mut Vec<(String, u16)>,
        repeats: &[(String, u16)],
        in_list: bool,
    ) -> Result<Node> {
        let (target, repetition) = self.binding_head(in_list)?;
        self.scope.slots = slot;
        let value = self.expression()?;
        self.scope.slots = slot + 1;
        self.binding_value(target, repetition, value, slot, declared, repeats)
    }

    fn binding_head(&mut self, in_list: bool) -> Result<(BindingTarget, Option<Node>)> {
        let target = self.binding_target(in_list)?;
        let repetition = if self.binding_repetition(&target)? {
            Some(self.index(ErrorCode::OperandExpected)?)
        } else {
            None
        };
        self.binding_equals()?;
        Ok((target, repetition))
    }

    /// Whether `[ n ]` follows the name; only a variable has repetitions.
    fn binding_repetition(&mut self, target: &BindingTarget) -> Result<bool> {
        if !self.peek_is(&Token::LeftBracket)? {
            return Ok(false);
        }
        if !matches!(target, BindingTarget::Variable(_)) {
            return self.error(ErrorCode::OperandExpected);
        }
        Ok(true)
    }

    fn binding_equals(&mut self) -> Result<()> {
        match self.peek()? {
            Some(Token::Op(Opcode::EQUAL)) => {
                self.bump();
                Ok(())
            }
            _ => self.error(ErrorCode::OperandExpected),
        }
    }

    /// The name or variable a binding declares, consumed.
    fn binding_target(&mut self, in_list: bool) -> Result<BindingTarget> {
        let target = match self.peek_mode(Mode::Binding)? {
            Some(Token::Symbol(s)) => BindingTarget::Name(s.name.value),
            Some(Token::Variable(v)) => BindingTarget::Variable(v),
            // Stored streams may name a binding with a resolved token.
            Some(Token::Field(f)) => match f
                .occurrence
                .id()
                .zip(f.field.id())
                .and_then(|(o, id)| self.ctx.names.field_name(o, id))
            {
                Some(name) => BindingTarget::Name(name.to_owned()),
                None => return self.error(ErrorCode::OperandExpected),
            },
            Some(Token::Op(op))
                if op
                    .name()
                    .is_some_and(|n| n.starts_with(char::is_alphabetic)) =>
            {
                BindingTarget::Name(op.name().unwrap_or_default().to_owned())
            }
            Some(_) if in_list => return self.error(ErrorCode::ListNotAllowed),
            _ => return self.error(ErrorCode::OperandExpected),
        };
        self.bump();
        Ok(target)
    }

    fn binding_value(
        &mut self,
        target: BindingTarget,
        repetition: Option<Node>,
        value: Node,
        slot: u16,
        declared: &mut Vec<(String, u16)>,
        repeats: &[(String, u16)],
    ) -> Result<Node> {
        match target {
            BindingTarget::Variable(name) => {
                let repetition = repetition.unwrap_or_else(|| leaf(one()));
                let depth = value.depth.max(repetition.depth);
                let set = Expr::call(
                    Opcode::SET_VARIABLE,
                    vec![value.expr, repetition.expr, Expr::Variable(name)],
                );
                self.node(set, depth)
            }
            BindingTarget::Name(name) => {
                let key = name.to_lowercase();
                let earlier = declared
                    .iter()
                    .chain(repeats)
                    .find(|(n, _)| *n == key)
                    .map(|(_, s)| *s);
                match earlier {
                    Some(first) => {
                        let set = Expr::call(
                            Opcode::SET_VARIABLE,
                            vec![value.expr, one(), Expr::Local(first)],
                        );
                        self.node(set, value.depth)
                    }
                    None => {
                        declared.push((key.clone(), slot));
                        self.scope.names.push((key, slot));
                        Ok(value)
                    }
                }
            }
        }
    }
}

/// Mark custom function calls whose value is the formula's value as tail
/// calls (5.2).
fn mark_tail_calls(e: &mut Expr, tail: bool) {
    match e {
        Expr::CustomCall { tail: t, args, .. } => {
            *t = tail;
            args.iter_mut().for_each(|a| mark_tail_calls(a, false));
        }
        Expr::Call { op, args } if *op == Opcode::IF => {
            for (i, a) in args.iter_mut().enumerate() {
                mark_tail_calls(a, tail && i > 0);
            }
        }
        Expr::Case(pairs) => {
            for (test, result) in pairs {
                mark_tail_calls(test, false);
                mark_tail_calls(result, tail);
            }
        }
        Expr::Choose { test, results } => {
            mark_tail_calls(test, false);
            results.iter_mut().for_each(|a| mark_tail_calls(a, tail));
        }
        Expr::Let { bindings, body } => {
            bindings.iter_mut().for_each(|a| mark_tail_calls(a, false));
            mark_tail_calls(body, tail);
        }
        _ => for_each_child(e, |c| mark_tail_calls(c, false)),
    }
}

/// Turn fields in field-reference positions into `17` references and flag
/// Evaluate and GetField there (5.3). Inside plug-in arguments nothing is
/// converted.
fn mark_field_refs(e: &mut Expr, wanted: bool, suppressed: bool) {
    let want = wanted && !suppressed;
    match e {
        Expr::Field(f) if want => *e = Expr::FieldRef(f.clone()),
        Expr::Evaluate {
            args,
            need_field_ref,
        } => {
            if want {
                *need_field_ref = 1;
            }
            args.iter_mut()
                .for_each(|a| mark_field_refs(a, false, suppressed));
        }
        Expr::GetField {
            arg,
            need_field_ref,
        } => {
            if want {
                *need_field_ref = 1;
            }
            mark_field_refs(arg, false, suppressed);
        }
        Expr::Call { op, args } if *op == Opcode::IF => {
            for (i, a) in args.iter_mut().enumerate() {
                mark_field_refs(a, wanted && i > 0, suppressed);
            }
        }
        Expr::Call { op, args } if is_aggregate(*op) => {
            args.iter_mut()
                .for_each(|a| mark_field_refs(a, true, suppressed));
        }
        Expr::Call { op, args } if field_parameters(*op) > 0 => {
            let n = field_parameters(*op);
            for (i, a) in args.iter_mut().enumerate() {
                mark_field_refs(a, i < n, suppressed);
            }
        }
        Expr::Case(pairs) => {
            for (test, result) in pairs {
                mark_field_refs(test, false, suppressed);
                mark_field_refs(result, wanted, suppressed);
            }
        }
        Expr::Choose { test, results } => {
            mark_field_refs(test, false, suppressed);
            results
                .iter_mut()
                .for_each(|a| mark_field_refs(a, wanted, suppressed));
        }
        Expr::Let { bindings, body } => {
            bindings
                .iter_mut()
                .for_each(|a| mark_field_refs(a, false, suppressed));
            mark_field_refs(body, wanted, suppressed);
        }
        Expr::While {
            initial,
            condition,
            logic,
            result,
        } => {
            for a in initial.iter_mut().chain(logic.iter_mut()) {
                mark_field_refs(a, false, suppressed);
            }
            mark_field_refs(condition, false, suppressed);
            mark_field_refs(result, wanted, suppressed);
        }
        Expr::PluginCall { args, .. } => {
            args.iter_mut()
                .for_each(|a| mark_field_refs(a, false, true));
        }
        _ => for_each_child(e, |c| mark_field_refs(c, false, suppressed)),
    }
}

fn for_each_child(e: &mut Expr, mut f: impl FnMut(&mut Expr)) {
    match e {
        Expr::List(items)
        | Expr::Call { args: items, .. }
        | Expr::Evaluate { args: items, .. }
        | Expr::CustomCall { args: items, .. }
        | Expr::PluginCall { args: items, .. } => items.iter_mut().for_each(f),
        Expr::Case(pairs) => {
            for (a, b) in pairs {
                f(a);
                f(b);
            }
        }
        Expr::Choose { test, results } => {
            f(test);
            results.iter_mut().for_each(f);
        }
        Expr::Let { bindings, body } => {
            bindings.iter_mut().for_each(&mut f);
            f(body);
        }
        Expr::While {
            initial,
            condition,
            logic,
            result,
        } => {
            initial.iter_mut().for_each(&mut f);
            f(condition);
            logic.iter_mut().for_each(&mut f);
            f(result);
        }
        Expr::GetField { arg, .. } => f(arg),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::{NameTable, NoNames};
    use crate::{formatted, polish};

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    const ONE: &str = "10 02 0001 0010 00000001 00000000 00000000";

    fn contacts() -> NameTable {
        let mut names = NameTable::default();
        names.add_occurrence(0xd00002, "Contacts", 0x8002);
        names.add_field(0x8002, 1, "First Name");
        names.add_field(0x8002, 2, "Last Name");
        names.add_custom_function(4, "Twice", 1);
        names
    }

    fn polish_of(src: &str, names: &dyn Names, parameters: &[String]) -> Vec<u8> {
        let ctx = Context {
            occurrence: Some(0xd00002),
            parameters,
            ..Context::new(names)
        };
        polish::encode(
            &compile(src, &ctx)
                .unwrap_or_else(|e| panic!("{src}: {e}"))
                .expr,
        )
    }

    fn error_of(src: &str) -> u16 {
        let names = contacts();
        let ctx = Context {
            occurrence: Some(0xd00002),
            ..Context::new(&names)
        };
        compile(src, &ctx).unwrap_err().code.number()
    }

    #[test]
    fn contacts_initial_compiles_to_the_stored_code() {
        // calc-encoding.md 3.1.
        let src = "Case ( not IsEmpty ( Last Name ) ; Left ( Last Name ; 1 ) ; \
                   not IsEmpty ( First Name ) ; Left ( First Name ; 1 ) )";
        let field = |id| format!("16 00 04 03d00002 02 01{id:02x} 0001");
        let want = hex(&format!(
            "4c 0002 1e 4e {f2} 57 {f2} {ONE} 1e 4e {f1} 57 {f1} {ONE}",
            f2 = field(2),
            f1 = field(1)
        ));
        assert_eq!(polish_of(src, &contacts(), &[]), want);
    }

    #[test]
    fn get_uuid_tokens_and_code() {
        // calc-encoding.md 4.1.
        let compiled = compile("Get ( UUID )", &Context::new(&NoNames)).unwrap();
        assert_eq!(
            formatted::encode(&compiled.tokens),
            hex("9b 0c13017a00 04 0c13017a00 9c65 0c13017a00 05")
        );
        assert_eq!(polish::encode(&compiled.expr), [0x9b, 0x65]);
    }

    /// FileMaker Pro 26.1.1's tokens (calc-tokens.csv rows 1142 and 2318,
    /// #338): Get ( SortOrder ) is `9b 8e`, GetToolDefinitions `f9 12`
    /// with its arguments last first; both decode, re-encode byte for byte
    /// and print back as written.
    #[test]
    fn filemaker_26_1_1_tokens() {
        let ctx = Context::new(&NoNames);
        let compiled = compile("Get ( SortOrder )", &ctx).unwrap();
        assert_eq!(polish::encode(&compiled.expr), [0x9b, 0x8e]);
        assert_eq!(polish::decode(&[0x9b, 0x8e]).unwrap(), compiled.expr);
        assert_eq!(crate::print(&compiled.tokens, &ctx), "Get ( SortOrder )");
        let text = "GetToolDefinitions ( \"openai\" ; \"f\" ; \"g\" )";
        let compiled = compile(text, &ctx).unwrap();
        let polish = polish::encode(&compiled.expr);
        // An `f9` function: its opcode and argument count, then the
        // arguments last first (calc-encoding.md 5.1).
        let arg = |text: &str| polish::encode(&compile(text, &ctx).unwrap().expr);
        let want = [
            vec![0xf9, 0x12, 0x00, 0x03],
            arg("\"g\""),
            arg("\"f\""),
            arg("\"openai\""),
        ]
        .concat();
        assert_eq!(polish, want);
        assert_eq!(polish::encode(&polish::decode(&polish).unwrap()), polish);
        let formatted = formatted::encode(&compiled.tokens);
        assert_eq!(
            formatted::encode(&formatted::decode(&formatted).unwrap()),
            formatted
        );
        assert_eq!(crate::print(&compiled.tokens, &ctx), text);
    }

    #[test]
    fn let_slots_follow_the_binding_positions() {
        // calc-encoding.md 5.5, CURLExecute ( URL ; Post Text ; HTTP Headers ).
        let params: Vec<String> = ["URL", "Post Text", "HTTP Headers"]
            .map(String::from)
            .to_vec();
        let src = "Let ( [ curl = 1 ; r = 1 ; r = 1 ; r = 1 ; r = 1 ; result = 1 ; \
                   $$debug = 1 ; r = 1 ] ; result )";
        let set_r = format!("e3 {ONE} {ONE} 18 0004");
        let debug = "1a 07 7e7e3e3f382f3d";
        let want = hex(&format!(
            "df 0008 {ONE} {ONE} {set_r} {set_r} {set_r} {ONE} e3 {ONE} {ONE} {debug} {set_r} 18 0008"
        ));
        assert_eq!(polish_of(src, &NoNames, &params), want);
        // Parameters are slots 0..p-1, matched case-insensitively.
        assert_eq!(polish_of("post text", &NoNames, &params), hex("18 0001"));
    }

    #[test]
    fn while_logic_treats_initial_names_as_repeats() {
        let src = "While ( [ i = 0 ] ; i < 3 ; [ i = i + 1 ; j = i ] ; j )";
        let ctx = Context::new(&NoNames);
        let Expr::While { logic, result, .. } = compile(src, &ctx).unwrap().expr else {
            panic!()
        };
        assert!(
            matches!(&logic[0], Expr::Call { op, args } if *op == Opcode::SET_VARIABLE
            && args[2] == Expr::Local(0))
        );
        assert_eq!(logic[1], Expr::Local(0));
        assert_eq!(*result, Expr::Local(2));
    }

    #[test]
    fn tail_calls_field_references_and_repetitions() {
        let names = contacts();
        // A custom function call is a tail call where its value is the
        // formula's value (5.2).
        let root = polish_of("Twice ( 1 )", &names, &[]);
        assert_eq!(root[0], 0x1c);
        let inner = polish_of("Twice ( 1 ) + 1", &names, &[]);
        assert_eq!(inner[1], 0x1b);
        let branch = polish_of("If ( 1 ; Twice ( 1 ) )", &names, &[]);
        assert_eq!(branch[1 + 18], 0x1c);
        // Aggregate arguments become field references (5.3).
        let list = polish_of("List ( Last Name )", &names, &[]);
        assert_eq!(&list[..4], &hex("87 0001 17"));
        // field[n] is GetRepetition ( field ; n ) (5.4).
        let rep = polish_of("Last Name[2]", &names, &[]);
        assert_eq!(&rep[..2], &[0x89, 0x17]);
        // $v[n] reads with GetVarRep, `$v[n] = x` assigns with SetVariable.
        assert_eq!(polish_of("$v[1]", &names, &[])[0], 0xe4);
        let set = polish_of("Let ( $v[2] = 1 ; 1 )", &names, &[]);
        assert_eq!(&set[..5], &hex("df 0001 e3 10"));
    }

    #[test]
    fn precedence_matches_filemaker() {
        let tree = |src| compile(src, &Context::new(&NoNames)).unwrap().expr;
        let n = |s| Expr::Number(Number::Decimal(Decimal::parse_literal(s).unwrap()));
        // `-2^2` is (-2)^2 and `not 1 = 0` is (not 1) = 0 (6.2).
        assert_eq!(
            tree("- 2 ^ 2"),
            Expr::call(
                Opcode::POWER,
                vec![Expr::call(Opcode::NEGATE, vec![n("2")]), n("2")]
            )
        );
        assert_eq!(
            tree("not 1 = 0"),
            Expr::call(
                Opcode::EQUAL,
                vec![Expr::call(Opcode::NOT, vec![n("1")]), n("0")]
            )
        );
        assert_eq!(
            tree("+\"5\""),
            Expr::call(Opcode::GET_AS_NUMBER, vec![Expr::Text(Text::plain("5"))])
        );
        // `&` binds tighter than comparison, `+` tighter than `&`.
        assert!(matches!(
            tree("1 & 2 = 12"),
            Expr::Call {
                op: Opcode::EQUAL,
                ..
            }
        ));
        assert!(matches!(
            tree("1 + 2 & 3"),
            Expr::Call {
                op: Opcode::CONCAT,
                ..
            }
        ));
        // A return typed inside quotes evaluates as a space (5.6).
        assert_eq!(tree("\"a\rb\""), Expr::Text(Text::plain("a b")));
    }

    #[test]
    fn names_resolve_in_scope_order() {
        let names = contacts();
        // A local shadows a function, a field and a constant.
        assert_eq!(
            polish_of("Let ( Pi = 2 ; Pi )", &names, &[]).last(),
            Some(&0)
        );
        assert_eq!(error_of("Let ( Abs = 1 ; Abs ( -1 ) )"), 1212);
        // A function only before `(`; an unknown name fails before the
        // operator check.
        assert_eq!(error_of("Abs"), 102);
        assert_eq!(error_of("1 foo"), 102);
        assert_eq!(error_of("Contacts::Nope"), 102);
        assert_eq!(error_of("Nope::Last Name"), 106);
        let ctx = Context::new(&names);
        assert_eq!(compile("Last Name", &ctx).unwrap_err().code.number(), 106);
        // Multi-word names; `${}` quotes any name.
        assert_eq!(
            polish_of("Let ( x y = 1 ; x y )", &names, &[]).last(),
            Some(&0)
        );
        assert_eq!(
            polish_of("${Last Name}", &names, &[])[1],
            Field::FIELD_BRACKETED
        );
    }

    #[test]
    fn filemaker_error_codes() {
        // calc-values.md 7.1 and the oracle answers in calc-encoding.md.
        for (src, code) in [
            ("Abs ( 1 ; 2 )", 1202),
            ("1 +", 1204),
            ("Abs ( )", 1204),
            ("1 == 1", 1204),
            ("Let ( 1x = 1 ; 1 )", 1204),
            ("/* x", 1205),
            ("\"abc", 1206),
            ("( 1", 1207),
            ("Foo ( 1 )", 1208),
            ("${}", 1209),
            ("Let ( [ \"x\" = 1 ] ; 1 )", 1211),
            ("Let ( [ x = 1 ; ] ; x )", 1211),
            ("1 )", 1212),
            ("1 ; 2", 1212),
            ("1 2", 1212),
            ("1,5", 1212),
            ("GetRepetition ( 5 ; 1 )", 1214),
            ("List ( \"\" )", 1214),
            ("Extend ( 1 ; 2 )", 1214),
            ("Get ( Foo )", 1215),
            ("5.", 1218),
            ("1e800", 1218),
            ("If ( 1 )", 1201),
            ("If ( 1 ; 2 ; )", 1204),
            ("Choose ( 1 ; )", 1204),
            ("Case ( 1 ; 2 ; 3 ; )", 1204),
            ("Let ( x = 1", 1201),
            ("Let ( x = 1 ; x ; 2 )", 1202),
            ("$x[", 1203),
            ("$x[1", 1207),
            ("( 1 2 )", 1207),
            ("Pi ( )", 1212),
            ("Substitute ( \"a\" ; \"a\" ; \"b\" ; \"c\" )", 1202),
            ("Substitute ( \"a\" ; [ \"a\" ] )", 1201),
            (
                "Substitute ( \"a\" ; [ \"a\" ; \"b\" ] ; \"a\" ; \"c\" )",
                1211,
            ),
            ("JSONSetElement ( \"{}\" ; \"a\" ; 1 )", 1201),
            ("1 \\ 2", 1204),
        ] {
            assert_eq!(error_of(src), code, "{src}");
        }
        // Accepted: trailing separators of unlimited functions, groups.
        for src in [
            "Case ( 1 ; 2 ; )",
            "List ( 1 ; 2 ; )",
            "Substitute ( \"abc\" ; [ \"a\" ; \"b\" ] ; [ \"c\" ; \"d\" ] )",
            "Let ( [] ; 1 )",
            "Evaluate ( \"1\" ; [ ] )",
            "1 /* a /* b */ c */ + 1",
            "// only a comment",
        ] {
            let names = contacts();
            let ctx = Context {
                occurrence: Some(0xd00002),
                ..Context::new(&names)
            };
            compile(src, &ctx).unwrap_or_else(|e| panic!("{src}: {e}"));
        }
    }

    #[test]
    fn error_spans_point_into_the_source() {
        let names = contacts();
        let ctx = Context {
            occurrence: Some(0xd00002),
            ..Context::new(&names)
        };
        let e = compile("1 + Foo ( 2 )", &ctx).unwrap_err();
        assert_eq!(e.span, 4..7);
        let e = compile("Abs ( 1", &ctx).unwrap_err();
        assert_eq!(e.span, 7..7);
    }

    /// The context of calc-encoding.md 6.4: occurrence `Table` with fields
    /// `Input` and `Output`, and a two-parameter custom function `f`.
    fn table() -> NameTable {
        let mut names = NameTable::default();
        names.add_occurrence(0xd00001, "Table", 0x8001);
        names.add_field(0x8001, 1, "Input");
        names.add_field(0x8001, 2, "Output");
        names.add_custom_function(64, "f", 2);
        names
    }

    fn table_ctx(names: &NameTable) -> Context<'_> {
        Context {
            occurrence: Some(0xd00001),
            ..Context::new(names)
        }
    }

    #[test]
    fn error_spans_match_the_engine() {
        // calc-encoding.md 6.4: (source, error, UTF-16 span).
        let names = table();
        for (src, code, span) in [
            ("1 +", 1204, 3..3),
            ("1 + * 2", 1204, 4..5),
            (") 1", 1204, 0..1),
            ("Abs ( )", 1204, 6..7),
            ("If ( 1 ; 2 ; )", 1204, 13..14),
            ("List ( 1 ; ; 3 )", 1204, 11..12),
            ("Case ( )", 1204, 7..8),
            ("Let ( 1 = 1 ; 1 )", 1204, 6..7),
            ("[ 1 ; 2 ]", 1204, 0..1),
            ("While ( [ i = 0 ] ; i < 3 ; i + 1 )", 1204, 30..31),
            ("( 1", 1207, 3..3),
            ("Abs ( 1", 1207, 7..7),
            ("${abc", 1207, 5..5),
            ("\"abc", 1206, 0..4),
            ("/* open", 1205, 0..7),
            ("Abs ( 1 ; 2 )", 1202, 8..9),
            ("Left ( \"a\" ; 1 ; 2 )", 1202, 15..16),
            ("Left ( \"a\" )", 1201, 11..12),
            ("If ( 1 )", 1201, 7..8),
            ("Substitute ( \"a\" ; [ \"a\" ] ; \"b\" )", 1201, 25..26),
            ("NoSuchFunction ( 1 )", 1208, 0..14),
            ("Get ( NoSuchSelector )", 1215, 6..20),
            ("Extend ( 1 )", 1214, 9..10),
            ("Sum ( 1 + 1 )", 1214, 6..11),
            ("Average ( Table::Input + 1 )", 1214, 17..26),
            ("1,5", 1212, 1..3),
            ("1 2", 1212, 2..3),
            ("Let ( x = 1 ; x[2] )", 1212, 15..16),
            ("nonexistent_name + 1", 102, 0..16),
            ("Abs 1", 102, 0..5),
            ("Table::NoSuchField", 102, 7..18),
            ("NoSuchTable::Input", 106, 0..11),
            ("Self", 1225, 0..4),
            ("5.", 1218, 0..2),
            ("f ( 1 )", 1201, 6..7),
            ("f ( 1 ; 2 ; 3 )", 1202, 14..15),
        ] {
            let e = compile(src, &table_ctx(&names)).unwrap_err();
            assert_eq!((e.code.number(), e.span), (code, span), "{src}");
        }
        // Spans count UTF-16 units.
        let e = compile("\"😀\" + Foo ( 1 )", &table_ctx(&names)).unwrap_err();
        assert_eq!(e.span, 7..10);
    }

    #[test]
    fn commas_separate_arguments_where_the_decimal_is_a_point() {
        // The engine's compile-mode output for battery c06 (Contacts en-US):
        // error, span, formatted and Polish bytes.
        let results =
            include_str!("../../../tools/fms-calc-harness/results/c06-commas.contacts-en.tsv");
        let names = NoNames;
        let ctx = Context::new(&names);
        let mut lines = 0;
        for line in results.lines().filter(|l| !l.starts_with('#')) {
            let cols: Vec<&str> = line.split('\t').collect();
            let (src, error, span) = (cols[0], cols[1], cols[2]);
            match compile(src, &ctx) {
                Ok(c) => {
                    assert_eq!(error, "0", "{src}");
                    assert_eq!(crate::formatted::encode(&c.tokens), hex(cols[4]), "{src}");
                    assert_eq!(polish::encode(&c.expr), hex(cols[5]), "{src}");
                }
                Err(e) => {
                    let got = format!("{}:{}", e.span.start, e.span.end);
                    assert_eq!(
                        (e.code.number().to_string().as_str(), got.as_str()),
                        (error, span),
                        "{src}"
                    );
                }
            }
            lines += 1;
        }
        assert_eq!(lines, 19);
    }

    #[test]
    fn engine_compile_answers() {
        // calc-encoding.md 10.1.
        let names = table();
        let ctx = table_ctx(&names);
        let code = |src| polish::encode(&compile(src, &ctx).unwrap().expr);
        let input = "16 00 04 03d00001 02 0101 0001";
        let output = "16 00 04 03d00001 02 0102 0001";
        let as_ref = |f: &str| f.replacen("16", "17", 1);
        let one = format!("13 01 {:02x} 00", b'1' ^ 0x5a);
        // Evaluate's trigger fields are further arguments.
        assert_eq!(
            code("Evaluate ( \"1\" ; [ Table::Input ; Table::Output ] )"),
            hex(&format!(
                "dc 0003 0000 {one} {} {}",
                input.replacen("00", "04", 1),
                output.replacen("00", "04", 1)
            ))
        );
        // GetSummary takes two field references, Lookup one.
        assert_eq!(
            code("GetSummary ( Input ; Output )"),
            hex(&format!("86 {} {}", as_ref(input), as_ref(output)))
        );
        assert_eq!(
            code("Lookup ( Input )"),
            hex(&format!("e0 0001 {}", as_ref(input)))
        );
        // Custom functions: tail calls, arguments last first, `g` and
        // `g ( )` alike, a trailing `;` accepted.
        let mut names = table();
        names.add_custom_function(20, "g", 0);
        let ctx = table_ctx(&names);
        let code = |src| polish::encode(&compile(src, &ctx).unwrap().expr);
        assert_eq!(code("f ( 1 ; 2 )")[..7], hex("1c 0002 14 02 01 40"));
        assert_eq!(code("1 + f ( 1 ; 2 )")[1 + 18], 0x1b);
        assert_eq!(code("g"), hex("1c 0000 14 02 01 14"));
        assert_eq!(code("g ( )"), hex("1c 0000 14 02 01 14"));
        assert_eq!(code("f ( 1 ; 2 ; )"), code("f ( 1 ; 2 )"));
        // `${...}` flags: fields 1, 5, 6, 7; symbols 1.
        let flags = |src| match &compile(src, &ctx).unwrap().tokens[0] {
            Token::Field(f) => f.flags,
            Token::Op(_) => match &compile(src, &ctx).unwrap().tokens[4] {
                Token::Symbol(s) => s.flag,
                t => panic!("{t:?}"),
            },
            t => panic!("{t:?}"),
        };
        assert_eq!(flags("${Input}"), 1);
        assert_eq!(flags("Table::${Input}"), 5);
        assert_eq!(flags("${Table}::Input"), 6);
        assert_eq!(flags("${Table}::${Input}"), 7);
        assert_eq!(flags("Let ( ${a b} = 1 ; ${a b} )"), 1);
        // `Self` with kSupportSelf.
        let with_self = Context {
            support_self: true,
            ..ctx
        };
        assert_eq!(
            polish::encode(&compile("Self", &with_self).unwrap().expr),
            [0xfa, 0x00]
        );
        // What may read Self, in any branch (#1023).
        for (src, reads) in [
            ("Self", true),
            ("If ( Get ( FoundCount ) > 1000 ; 1 ; Self = 0 )", true),
            ("Let ( [ a = 1 ] ; Case ( a ; 0 ; Self ) )", true),
            ("g", true),
            ("Evaluate ( \"1\" )", true),
            ("Get ( FoundCount ) > 1000", false),
            ("Let ( $$n = Input ; $$n > 0 )", false),
        ] {
            let expr = compile(src, &with_self).unwrap().expr;
            assert_eq!(expr.may_read_self(), reads, "{src}");
        }
        // A decimal comma locale.
        let comma = Context {
            decimal_comma: true,
            ..ctx
        };
        let compiled = compile("List ( 1,5 ; 2 )", &comma).unwrap();
        let Expr::Call { args, .. } = &compiled.expr else {
            panic!()
        };
        assert_eq!(args.len(), 2);
        assert_eq!(crate::print(&compiled.tokens, &comma), "List ( 1,5 ; 2 )");
    }

    /// Run `adversarial_nesting` with `mode` in a child process on a 1 MiB
    /// stack: a small UI thread's, rather than a test thread's, so excess
    /// recursion is not hidden, and exhausting the stack fails this test
    /// rather than aborting the test binary.
    fn on_small_stack(mode: &str) {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "parse::tests::adversarial_nesting",
                "--ignored",
                "--nocapture",
            ])
            .env("OM_FORMULA_ADVERSARIAL", mode)
            .env("RUST_MIN_STACK", (1 << 20).to_string())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{:?}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn deeply_nested_formulas_are_refused_without_exhausting_the_stack() {
        on_small_stack("hostile");
    }

    #[test]
    fn formulas_nest_up_to_the_budget() {
        on_small_stack("boundaries");
    }

    #[test]
    #[ignore = "run in a child process on a small stack by on_small_stack"]
    fn adversarial_nesting() {
        match std::env::var("OM_FORMULA_ADVERSARIAL").as_deref() {
            Ok("hostile") => refuse_hostile_nesting(),
            Ok("boundaries") => nest_up_to_the_budget(),
            _ => {}
        }
    }

    /// Every way one expression is read inside another: `before` and
    /// `after` one level of it, around `1`.
    const READERS: [(&str, &str, &str); 27] = [
        ("parentheses", "( ", " )"),
        ("binary right operand", "1 + ( ", " )"),
        ("fixed", "Abs ( ", " )"),
        ("counted", "List ( 0 ; ", " )"),
        ("case", "Case ( 1 ; ", " )"),
        ("choose", "Choose ( 0 ; ", " )"),
        ("if", "If ( 1 ; ", " ; 0 )"),
        ("swapped", "SetRecursion ( ", " ; 100 )"),
        ("evaluate", "Evaluate ( ", " )"),
        ("trigger list", "Evaluate ( 1 ; [ ", " ] )"),
        ("getfield", "GetField ( ", " )"),
        ("custom", "Twice ( ", " )"),
        ("plugin", "Plugin ( ", " )"),
        ("grouped", "Substitute ( ", " ; [ \"a\" ; \"b\" ] )"),
        ("group item", "Substitute ( \"x\" ; [ ", " ; \"y\" ] )"),
        (
            "group of three",
            "JSONSetElement ( \"{}\" ; [ \"a\" ; ",
            " ; 1 ] )",
        ),
        ("let", "Let ( a = 1 ; ", " )"),
        ("let binding", "Let ( a = ", " ; 1 )"),
        ("let binding list", "Let ( [ a = 1 ; b = ", " ] ; 1 )"),
        ("let repetition", "Let ( [ $a[ ", " ] = 1 ] ; 1 )"),
        ("while", "While ( [ ] ; 0 ; [ ] ; ", " )"),
        ("while binding", "While ( [ a = ", " ] ; 0 ; [ ] ; 1 )"),
        ("while condition", "While ( [ ] ; ", " ; [ ] ; 1 )"),
        ("while logic", "While ( [ ] ; 0 ; [ a = ", " ] ; 1 )"),
        (
            "while repetition",
            "While ( [ ] ; 0 ; [ $a[ ",
            " ] = 1 ] ; 1 )",
        ),
        ("variable repetition", "$a[ ", " ]"),
        ("field repetition", "Last Name[ ", " ]"),
    ];

    /// Operators before each level of a reader: none, one binary operator
    /// waiting for its right operand, one of each binary precedence level,
    /// prefixes.
    const OPERATORS: [&str; 4] = ["", "1 ^ ", "1 or 1 and 1 = 1 & 1 + 1 * 1 ^ ", "- not "];

    fn nested(before: &str, after: &str, k: usize) -> String {
        format!("{}1{}", before.repeat(k), after.repeat(k))
    }

    fn adversarial_names() -> NameTable {
        let mut names = contacts();
        names.add_plugin_function(
            PluginFunctionId {
                plugin: *b"TEST",
                function: 1,
            },
            "Plugin",
            1,
            Some(1),
        );
        names
    }

    fn refuse_hostile_nesting() {
        let names = adversarial_names();
        let ctx = Context {
            occurrence: Some(0xd00002),
            ..Context::new(&names)
        };
        let n = 100_000;
        let mut cases = vec![
            ("unary minus".to_owned(), "- ".repeat(n) + "1"),
            ("an operator chain".to_owned(), vec!["1"; n].join(" + ")),
            (
                "an operator ladder".to_owned(),
                vec!["1"; n].join(" ^ 1 * 1 + 1 & 1 = 1 and 1 or "),
            ),
        ];
        for (name, before, after) in READERS {
            for operators in OPERATORS {
                let before = format!("{operators}{before}");
                cases.push((format!("{operators}{name}"), nested(&before, after, n)));
            }
        }
        for (what, src) in &cases {
            let error = compile(src, &ctx).expect_err(what);
            assert_eq!(error.code, ErrorCode::NestingTooDeep, "{what}");
        }
        let mut tokens = vec![Token::LeftParen; n];
        tokens.extend(compile("1", &ctx).unwrap().tokens);
        tokens.extend(vec![Token::RightParen; n]);
        assert_eq!(
            compile_tokens(&tokens, &ctx).unwrap_err().code,
            ErrorCode::NestingTooDeep
        );
        // Stored code nested far past the decoder's bound, through each
        // kind of node header, even with a caller-supplied larger depth.
        for header in [
            "1e",
            "19 0001",
            "4c 0001",
            "4d 0000",
            "df 0000",
            "f800 0000",
            "dc 0001 0000",
            "dd 0000",
            "1b 0001 14 02 01 04",
            "1d 0001 15 54455354 0001",
            "87 0001",
            "4b",
            "40",
        ] {
            let mut deep = hex(header).repeat(n);
            deep.extend([Opcode::GET.0 as u8, 2]);
            assert_eq!(
                polish::decode_with(
                    &deep,
                    polish::Limits {
                        depth: usize::MAX,
                        nodes: usize::MAX
                    }
                )
                .unwrap_err()
                .kind,
                crate::DecodeErrorKind::Malformed("nesting depth"),
                "{header}"
            );
        }
    }

    /// The formatted stream of `before` `1` `after` nested `k` deep, made
    /// from one level's stream.
    fn nested_tokens(before: &str, after: &str, k: usize, ctx: &Context<'_>) -> Option<Vec<Token>> {
        let one = compile(&format!("{before}2{after}"), ctx).ok()?.tokens;
        let two = compile("2", ctx).unwrap().tokens;
        let i = one.iter().position(|t| *t == two[0]).unwrap();
        let mut tokens = Vec::new();
        (0..k).for_each(|_| tokens.extend_from_slice(&one[..i]));
        tokens.extend(compile("1", ctx).unwrap().tokens);
        (0..k).for_each(|_| tokens.extend_from_slice(&one[i + 1..]));
        Some(tokens)
    }

    fn nest_up_to_the_budget() {
        let names = adversarial_names();
        let ctx = Context {
            occurrence: Some(0xd00002),
            ..Context::new(&names)
        };
        let n = MAX_NESTING;
        let check = |source: &str, depth: usize| {
            let compiled = compile(source, &ctx).unwrap();
            assert_eq!(compiled.expr.depth(), depth);
            assert_eq!(
                compile_tokens(&compiled.tokens, &ctx).unwrap(),
                compiled.expr
            );
            assert_eq!(
                polish::decode(&polish::encode(&compiled.expr)).unwrap(),
                compiled.expr
            );
        };
        // Nodes per repetition, and of the rest, from one and two.
        let shape = |make: &dyn Fn(usize) -> String| {
            let one = compile(&make(1), &ctx).ok()?.expr.depth();
            let two = compile(&make(2), &ctx).ok()?.expr.depth();
            Some((two - one, 2 * one - two))
        };
        // Binary chains and prefixes grow the tree without recursion.
        let chains: [(&str, &dyn Fn(usize) -> String); 3] = [
            ("chain", &|k| vec!["1"; k + 1].join(" + ")),
            ("ladder", &|k| {
                vec!["1 ^ 1 * 1 + 1 & 1 = 1 and 1"; k].join(" or ")
            }),
            ("prefixes", &|k| "- not ".repeat(k) + "1"),
        ];
        for (what, make) in chains {
            let (per, base) = shape(make).unwrap();
            let longest = (n - base) / per;
            check(&make(longest), base + per * longest);
            assert_eq!(
                compile(&make(longest + 1), &ctx).unwrap_err().code,
                ErrorCode::NestingTooDeep,
                "{what}"
            );
        }
        // Each reader is one more level of recursion. Before the deepest
        // recursion the parser allows, the tree's bound may come first.
        for (name, before, after) in READERS {
            for operators in OPERATORS {
                let before = format!("{operators}{before}");
                let what = format!("{operators}{name}");
                eprintln!("boundary: {what}");
                let make = |k| nested(&before, after, k);
                let deepest = match shape(&make) {
                    Some((0, base)) => {
                        check(&make(n - 1), base);
                        n - 1
                    }
                    Some((per, base)) => {
                        let deepest = (n - 1).min((n - base) / per);
                        check(&make(deepest), base + per * deepest);
                        deepest
                    }
                    // A nested Evaluate is no trigger field: refused after
                    // the innermost level is read.
                    None => 0,
                };
                // One level more, and the deepest recursion and one more,
                // are refused alike from text and from stored tokens.
                for k in [deepest + 1, n - 1, n] {
                    if k <= deepest {
                        continue;
                    }
                    let error = compile(&make(k), &ctx).unwrap_err().code;
                    if deepest > 0 || k == n {
                        assert_eq!(error, ErrorCode::NestingTooDeep, "{what} {k}");
                    }
                    if let Some(tokens) = nested_tokens(&before, after, k, &ctx) {
                        assert_eq!(
                            compile_tokens(&tokens, &ctx).unwrap_err().code,
                            error,
                            "{what} {k}"
                        );
                    }
                }
            }
        }
    }
}
