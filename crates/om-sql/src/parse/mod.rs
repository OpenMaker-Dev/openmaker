//! Lexing and parsing FQL (execute-sql.md 3): tokens, the grammar as an
//! ATN, ANTLR-compatible prediction, the parse tree and the statement it
//! describes.
//!
//! ```text
//! text --lex--> tokens --tree::parse (ATN + predict)--> parse tree --ast--> Statement
//! ```
//!
//! A statement may nest [`MAX_DEPTH`] levels; a deeper one is a syntax
//! error. Everything that walks a statement's tree, here and in the planner
//! and executor, recurses: the limit bounds that recursion, and the large
//! frames continue on the heap when the thread's stack runs low
//! (`crate::stack`), so hostile text cannot overflow the thread that runs
//! it.

pub mod ast;
mod atn;
mod build;
mod grammar;
pub mod lex;
mod predict;
mod tree;

use std::cell::RefCell;
use std::sync::OnceLock;

pub use lex::{Pos, Token, TokenType};
pub use tree::SyntaxError;

use atn::Atn;
use predict::Predictor;

/// How deep a statement may nest: OpenMaker's limit, far above any real
/// statement (FileMaker's own is unmeasured; execute-sql.md, "Implementation
/// notes (om-sql)").
///
/// The depth is the height of the statement's parse tree, counting one
/// level for each grammar rule on the way down from `statement` to a token,
/// and one more for each operator of a chain of `+`, `-`, `||`, `*`, `/` or
/// `^` and for each join, since the statement's tree nests one level per
/// operator or join of a chain (the [`ast`] of a chain is left-deep). A plain
/// query is about 15 levels deep. A pair of parentheses costs 4 levels
/// around a condition (`search_term`, `boolean_term`, `boolean_factor`,
/// `boolean_primary`), 5 around a value (`value_expression` to
/// `value_primary`) and about 11 around a subquery.
///
/// The parser checks the depth as the tree grows, and SLL prediction holds
/// its contexts (stacks of rule invocations below the decision) to the
/// same depth; a statement that passes it is a syntax error (FQL0001) at
/// the token where the parser or prediction passed it.
pub const MAX_DEPTH: usize = 500;

fn atn() -> &'static Atn {
    static ATN: OnceLock<Atn> = OnceLock::new();
    ATN.get_or_init(Atn::build)
}

thread_local! {
    /// The SLL prediction cache (ANTLR keeps one per recognizer class too).
    static PREDICTOR: RefCell<Predictor<'static>> = RefCell::new(Predictor::new(atn()));
}

/// The rule names of the grammar, for diagnostics.
pub fn rule_name(rule: usize) -> &'static str {
    atn().rule_names[rule]
}

/// Lexes and parses `sql` into a parse tree.
pub(crate) fn parse_tree(sql: &str) -> Result<(lex::Lexed, tree::Node), SyntaxError> {
    let lexed = lex::lex(sql);
    let node = PREDICTOR.with(|p| tree::parse(atn(), &mut p.borrow_mut(), &lexed))?;
    Ok((lexed, node))
}

/// Parses a statement. A syntax error is FQL0001 at its position.
pub fn parse(sql: &str) -> Result<ast::Statement, SyntaxError> {
    crate::stack::guard(|| {
        let (lexed, node) = parse_tree(sql)?;
        Ok(build::statement(&lexed.tokens, &node))
    })
}
