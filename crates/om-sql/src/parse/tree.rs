//! Parsing a token list into a parse tree by walking the ATN, as ANTLR's
//! `ParserInterpreter` and FileMaker's generated `FQLSQLSemanticAnalyzer`
//! do, and finding the first syntax error the way they report it.
//!
//! Only the first error matters (it becomes FQL0001). It is the offending
//! token of the first failure: a `match` against the wrong token, an LL(1)
//! block with no alternative for the next token, or a failed prediction
//! ([`super::predict`]). Lexer errors count too, in the order ANTLR reports
//! them: when the lexer produced the bad character's token before the
//! parser failed (that is, the parser had looked that far), the lexer error
//! came first.
//!
//! The parser also fails where the statement gets deeper than
//! [`MAX_DEPTH`]: at the first token of a rule that would open too deep, at
//! an operator or join that would make its chain too deep, or at the token
//! after a rule whose subtree made its parent too deep.

use std::rc::Rc;

use super::atn::{Atn, Kind, Transition};
use super::lex::{self, Lexed, Pos, Token, TokenType, EOF};
use super::predict::{self, Ctx, Input, Predictor};
use super::MAX_DEPTH;

/// A node of the parse tree.
#[derive(Clone, Debug)]
pub enum Tree {
    /// A token, by index into the token list.
    Token(usize),
    Rule(Node),
}

/// A rule's subtree: the rule, the alternative of its top-level block
/// (1 when it has one), and its children in order.
#[derive(Clone, Debug)]
pub struct Node {
    pub rule: usize,
    pub alt: u16,
    pub children: Vec<Tree>,
}

/// The first syntax error: where FileMaker reports FQL0001.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub pos: Pos,
}

struct Frame {
    node: Node,
    /// The state parsing continues in after the rule.
    follow: usize,
    /// The rule's top-level decision state, if any.
    top_decision: Option<usize>,
    /// The height of the tallest finished child rule (0 without one).
    below: usize,
    /// The operators or joins of the chain this rule makes so far.
    links: usize,
}

impl Frame {
    fn new(rule: usize, follow: usize, top_decision: Option<usize>) -> Frame {
        Frame {
            node: Node {
                rule,
                alt: 1,
                children: Vec::new(),
            },
            follow,
            top_decision,
            below: 0,
            links: 0,
        }
    }

    /// The height of the rule's subtree so far (see [`MAX_DEPTH`]).
    fn height(&self) -> usize {
        1 + self.below + self.links
    }
}

/// The rules whose repetitions build a left-deep tree, each nesting the
/// statement one level further: every operator token of the three operator
/// chains, and every `table_join_reference` of a `table_reference`.
struct Chains {
    operators: [usize; 3],
    table_reference: usize,
    join: usize,
    interval: TokenType,
}

impl Chains {
    fn of(atn: &Atn) -> Chains {
        let rule = |name: &str| {
            atn.rule_names
                .iter()
                .position(|n| *n == name)
                .expect("a rule of the grammar")
        };
        Chains {
            operators: [
                rule("value_expression"),
                rule("value_term"),
                rule("value_pow"),
            ],
            table_reference: rule("table_reference"),
            join: rule("table_join_reference"),
            interval: lex::kw("INTERVAL"),
        }
    }

    /// Whether `token`, consumed by the rule `rule`, links a chain
    /// (`+ INTERVAL` is one link).
    fn links_token(&self, rule: usize, token: TokenType) -> bool {
        self.operators.contains(&rule) && token != self.interval
    }

    /// Whether `child`, invoked by the rule `rule`, links a chain.
    fn links_rule(&self, rule: usize, child: usize) -> bool {
        rule == self.table_reference && child == self.join
    }
}

/// Whether the innermost open rule, at its depth, makes the statement
/// deeper than [`MAX_DEPTH`]: the tree's height is at least the depth of a
/// rule plus the height of the rule's subtree. The rules above it were
/// checked when they last changed.
fn too_deep(stack: &[Frame]) -> bool {
    stack
        .last()
        .is_some_and(|top| stack.len() - 1 + top.height() > MAX_DEPTH)
}

struct Stream<'t> {
    tokens: &'t [Token],
    at: usize,
    /// The furthest token the parser has looked at.
    seen: usize,
}

impl Stream<'_> {
    fn la_at(&mut self, i: usize) -> TokenType {
        let i = i.min(self.tokens.len() - 1);
        self.seen = self.seen.max(i);
        self.tokens[i].ty
    }
}

struct Lookahead<'s, 't> {
    stream: &'s mut Stream<'t>,
}

impl Input for Lookahead<'_, '_> {
    fn la(&mut self, k: usize) -> TokenType {
        let at = self.stream.at;
        self.stream.la_at(at + k)
    }
}

/// Parses `lexed` from rule 0; the tree or the first error.
pub fn parse(atn: &Atn, predictor: &mut Predictor<'_>, lexed: &Lexed) -> Result<Node, SyntaxError> {
    let mut stream = Stream {
        tokens: &lexed.tokens,
        at: 0,
        seen: 0,
    };
    let result = run(atn, predictor, &mut stream);
    // The lexer reports its first error when it produces the token after
    // the bad character; it came first if the parser had asked for that
    // token by the time it failed (or never failed).
    let lexer_first = lexed.errors.first().filter(|e| match result {
        Ok(_) => true,
        Err(_) => e.token_index <= stream.seen,
    });
    match (lexer_first, result) {
        (Some(e), _) => Err(SyntaxError { pos: e.pos }),
        (None, Err(offending)) => Err(SyntaxError {
            pos: lexed.tokens[offending.min(lexed.tokens.len() - 1)].pos,
        }),
        (None, Ok(node)) => Ok(node),
    }
}

/// The parse; on failure the index of the offending token.
fn run(atn: &Atn, predictor: &mut Predictor<'_>, stream: &mut Stream<'_>) -> Result<Node, usize> {
    let chains = Chains::of(atn);
    let mut stack: Vec<Frame> = Vec::new();
    let mut state = atn.rule_start[0];
    stack.push(Frame::new(0, usize::MAX, top_decision(atn, 0)));
    stream.la_at(0);
    loop {
        let s = &atn.states[state];
        if s.kind == Kind::RuleStop {
            let frame = stack.pop().expect("a rule is open");
            match stack.last_mut() {
                None => return Ok(frame.node),
                Some(parent) => {
                    parent.below = parent.below.max(frame.height());
                    parent.node.children.push(Tree::Rule(frame.node));
                    if too_deep(&stack) {
                        return Err(stream.at);
                    }
                    state = frame.follow;
                    continue;
                }
            }
        }
        let alt = if s.transitions.len() > 1 {
            let decision = s.decision.expect("a state with alternatives is a decision");
            let alt = choose(atn, predictor, stream, decision, &stack)?;
            let frame = stack.last_mut().expect("a rule is open");
            if frame.top_decision == Some(state) {
                frame.node.alt = alt;
            }
            alt
        } else {
            1
        };
        match s.transitions[usize::from(alt) - 1] {
            Transition::Epsilon(target) => state = target,
            Transition::Atom(ty, target) => {
                if stream.la_at(stream.at) != ty {
                    return Err(stream.at);
                }
                consume(stream, &mut stack, &chains)?;
                state = target;
            }
            Transition::Set(set, target) => {
                let t = stream.la_at(stream.at);
                if !atn.sets[set].contains(t) {
                    return Err(stream.at);
                }
                consume(stream, &mut stack, &chains)?;
                state = target;
            }
            Transition::Rule {
                start,
                rule,
                follow,
            } => {
                let parent = stack.last_mut().expect("a rule is open");
                if chains.links_rule(parent.node.rule, rule) {
                    parent.links += 1;
                    if too_deep(&stack) {
                        return Err(stream.at);
                    }
                }
                stack.push(Frame::new(rule, follow, top_decision(atn, rule)));
                if too_deep(&stack) {
                    return Err(stream.at);
                }
                state = start;
            }
        }
    }
}

/// Consumes the next token into the innermost rule; fails at the token
/// when it links a chain that is then too deep.
fn consume(stream: &mut Stream<'_>, stack: &mut [Frame], chains: &Chains) -> Result<(), usize> {
    let at = stream.at;
    let top = stack.last_mut().expect("a rule is open");
    if chains.links_token(top.node.rule, stream.tokens[at].ty) {
        top.links += 1;
        if too_deep(stack) {
            return Err(at);
        }
    }
    stack
        .last_mut()
        .expect("a rule is open")
        .node
        .children
        .push(Tree::Token(at));
    if stream.tokens[at].ty != EOF {
        stream.at += 1;
    }
    // ANTLR's token stream fetches the next token when one is consumed.
    stream.la_at(stream.at);
    Ok(())
}

/// The decision state a rule's start leads to directly: its top-level
/// block of alternatives.
fn top_decision(atn: &Atn, rule: usize) -> Option<usize> {
    let start = &atn.states[atn.rule_start[rule]];
    let target = start.transitions.first()?.target();
    (atn.states[target].kind == Kind::BlockStart).then_some(target)
}

/// The alternative of `decision`: by the next token when the decision is
/// LL(1) and the token starts an alternative, else by adaptive prediction.
fn choose(
    atn: &Atn,
    predictor: &mut Predictor<'_>,
    stream: &mut Stream<'_>,
    decision: usize,
    stack: &[Frame],
) -> Result<u16, usize> {
    if let Some(sets) = &atn.ll1[decision] {
        let t = stream.la_at(stream.at);
        if let Some(i) = sets.iter().position(|s| s.contains(t)) {
            return Ok(i as u16 + 1);
        }
    }
    let at = stream.at;
    let outer = || outer_context(stack);
    let mut input = Lookahead { stream };
    predictor
        .predict(decision, &mut input, &outer)
        .map_err(|e| at + e.offset)
}

/// The parser's call stack as a prediction context (`fromRuleContext`):
/// the follow state of each open rule's invocation, innermost first.
fn outer_context(stack: &[Frame]) -> Rc<Ctx> {
    let mut ctx = predict::empty();
    for frame in stack.iter().skip(1) {
        ctx = Ctx::push(&ctx, frame.follow);
    }
    ctx
}
