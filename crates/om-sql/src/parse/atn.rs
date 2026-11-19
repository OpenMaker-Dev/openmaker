//! The grammar as an ANTLR 4 ATN (augmented transition network), built the
//! way ANTLR's `ParserATNFactory` builds it, so that prediction sees the
//! same states, decisions and alternatives as FileMaker's generated
//! recognizer:
//!
//! - a rule is `RuleStart -> block -> RuleStop`; every rule invocation adds
//!   an epsilon "follow link" from the invoked rule's stop state to the
//!   state after the invocation (the global FOLLOW SLL prediction uses);
//! - a block of several alternatives starts at a decision state whose
//!   transition `i` enters alternative `i + 1`;
//! - `x?` is a block whose last alternative bypasses it;
//! - `x*` is a loop entry (alternative 1 enters, 2 exits) with a loop back;
//! - `x+` is the block followed by a loop back (1 repeats, 2 exits).
//!
//! A decision is LL(1) when the tokens that can start each alternative,
//! following rule ends into every caller, are disjoint; ANTLR's generated
//! code tests the next token for those and predicts the rest adaptively.

use super::grammar::{self, Atom, Element, Suffix};
use super::lex::{self, TokenType};

/// A set of token types.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TokenSet(Vec<u64>);

impl TokenSet {
    pub fn new() -> TokenSet {
        TokenSet(vec![0; lex::token_type_count().div_ceil(64)])
    }

    pub fn of(tokens: &[TokenType]) -> TokenSet {
        let mut s = TokenSet::new();
        for t in tokens {
            s.insert(*t);
        }
        s
    }

    pub fn insert(&mut self, t: TokenType) {
        self.0[usize::from(t.0) / 64] |= 1 << (t.0 % 64);
    }

    pub fn contains(&self, t: TokenType) -> bool {
        self.0[usize::from(t.0) / 64] & (1 << (t.0 % 64)) != 0
    }

    pub fn union(&mut self, other: &TokenSet) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a |= b;
        }
    }

    pub fn intersects(&self, other: &TokenSet) -> bool {
        self.0.iter().zip(&other.0).any(|(a, b)| a & b != 0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|w| *w == 0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Basic,
    RuleStart,
    RuleStop,
    BlockStart,
    StarBlockStart,
    PlusBlockStart,
    BlockEnd,
    StarLoopEntry,
    StarLoopBack,
    PlusLoopBack,
    LoopEnd,
}

#[derive(Clone, Debug)]
pub enum Transition {
    Epsilon(usize),
    Atom(TokenType, usize),
    Set(usize, usize),
    /// A rule invocation: the rule's start state, the rule, and the state
    /// parsing continues in afterwards.
    Rule {
        start: usize,
        rule: usize,
        follow: usize,
    },
}

impl Transition {
    pub fn target(&self) -> usize {
        match *self {
            Transition::Epsilon(t) | Transition::Atom(_, t) | Transition::Set(_, t) => t,
            Transition::Rule { start, .. } => start,
        }
    }

    pub fn is_epsilon(&self) -> bool {
        matches!(self, Transition::Epsilon(_) | Transition::Rule { .. })
    }
}

#[derive(Clone, Debug)]
pub struct State {
    pub kind: Kind,
    pub rule: usize,
    pub transitions: Vec<Transition>,
    /// The decision number of a decision state.
    pub decision: Option<usize>,
}

impl State {
    /// ANTLR's `onlyHasEpsilonTransitions`: false for a state without
    /// transitions (the stop state of a rule nobody calls).
    pub fn only_epsilon(&self) -> bool {
        !self.transitions.is_empty() && self.transitions.iter().all(Transition::is_epsilon)
    }
}

pub struct Atn {
    pub states: Vec<State>,
    pub sets: Vec<TokenSet>,
    pub rule_names: Vec<&'static str>,
    pub rule_start: Vec<usize>,
    pub rule_stop: Vec<usize>,
    /// Decision number to its state.
    pub decisions: Vec<usize>,
    /// Per decision, the first tokens of each alternative when they are
    /// disjoint (an LL(1) decision).
    pub ll1: Vec<Option<Vec<TokenSet>>>,
}

impl Atn {
    pub fn build() -> Atn {
        let rules = grammar::rules();
        let mut b = Builder {
            atn: Atn {
                states: Vec::new(),
                sets: Vec::new(),
                rule_names: rules.iter().map(|r| r.name).collect(),
                rule_start: Vec::new(),
                rule_stop: Vec::new(),
                decisions: Vec::new(),
                ll1: Vec::new(),
            },
            rule: 0,
        };
        for r in 0..rules.len() {
            b.rule = r;
            let start = b.state(Kind::RuleStart);
            let stop = b.state(Kind::RuleStop);
            b.atn.rule_start.push(start);
            b.atn.rule_stop.push(stop);
        }
        for (r, rule) in rules.iter().enumerate() {
            b.rule = r;
            let (left, right) = b.block(&rule.alts, Suffix::One);
            let (start, stop) = (b.atn.rule_start[r], b.atn.rule_stop[r]);
            b.epsilon(start, left);
            b.epsilon(right, stop);
        }
        let mut atn = b.atn;
        // Follow links, in state order as ANTLR's deserializer adds them.
        let mut links = Vec::new();
        for state in &atn.states {
            for t in &state.transitions {
                if let Transition::Rule { rule, follow, .. } = *t {
                    links.push((atn.rule_stop[rule], follow));
                }
            }
        }
        for (stop, follow) in links {
            atn.states[stop]
                .transitions
                .push(Transition::Epsilon(follow));
        }
        atn.ll1 = (0..atn.decisions.len()).map(|d| atn.ll1_sets(d)).collect();
        atn
    }

    /// The tokens that can follow `state` (ANTLR's `LL1Analyzer._LOOK` from
    /// the empty context): rule ends continue into every caller.
    pub fn look(&self, state: usize) -> TokenSet {
        let mut out = TokenSet::new();
        let mut busy = std::collections::HashSet::new();
        let mut called = vec![false; self.rule_start.len()];
        self.look_into(state, &mut out, &mut busy, &mut called);
        out
    }

    fn look_into(
        &self,
        s: usize,
        out: &mut TokenSet,
        busy: &mut std::collections::HashSet<(usize, Vec<usize>)>,
        called: &mut Vec<bool>,
    ) {
        // The context is always empty or a stack of follow states pushed by
        // rule calls inside this lookahead.
        self.look_ctx(s, &mut Vec::new(), out, busy, called);
    }

    fn look_ctx(
        &self,
        s: usize,
        ctx: &mut Vec<usize>,
        out: &mut TokenSet,
        busy: &mut std::collections::HashSet<(usize, Vec<usize>)>,
        called: &mut Vec<bool>,
    ) {
        if !busy.insert((s, ctx.clone())) {
            return;
        }
        let state = &self.states[s];
        if state.kind == Kind::RuleStop {
            if let Some(follow) = ctx.pop() {
                let rule = state.rule;
                let was = called[rule];
                called[rule] = false;
                self.look_ctx(follow, ctx, out, busy, called);
                called[rule] = was;
                ctx.push(follow);
                return;
            }
        }
        for t in &state.transitions {
            match *t {
                Transition::Rule {
                    start,
                    rule,
                    follow,
                } => {
                    if called[rule] {
                        continue;
                    }
                    called[rule] = true;
                    ctx.push(follow);
                    self.look_ctx(start, ctx, out, busy, called);
                    ctx.pop();
                    called[rule] = false;
                }
                Transition::Epsilon(target) => self.look_ctx(target, ctx, out, busy, called),
                Transition::Atom(ty, _) => out.insert(ty),
                Transition::Set(set, _) => out.union(&self.sets[set]),
            }
        }
    }

    /// ANTLR's `ATN.nextTokens(s)`: the tokens that can follow `s` inside its
    /// rule, and whether the rule's end is reachable without a token.
    pub fn next_tokens_in_rule(&self, state: usize) -> (TokenSet, bool) {
        let mut out = TokenSet::new();
        let mut end = false;
        let mut busy = std::collections::HashSet::new();
        let mut called = vec![false; self.rule_start.len()];
        self.next_in_rule(
            state,
            &mut Vec::new(),
            &mut out,
            &mut end,
            &mut busy,
            &mut called,
        );
        (out, end)
    }

    fn next_in_rule(
        &self,
        s: usize,
        ctx: &mut Vec<usize>,
        out: &mut TokenSet,
        end: &mut bool,
        busy: &mut std::collections::HashSet<(usize, Vec<usize>)>,
        called: &mut Vec<bool>,
    ) {
        if !busy.insert((s, ctx.clone())) {
            return;
        }
        let state = &self.states[s];
        if state.kind == Kind::RuleStop {
            match ctx.pop() {
                None => {
                    *end = true;
                    return;
                }
                Some(follow) => {
                    let rule = state.rule;
                    let was = called[rule];
                    called[rule] = false;
                    self.next_in_rule(follow, ctx, out, end, busy, called);
                    called[rule] = was;
                    ctx.push(follow);
                    return;
                }
            }
        }
        for t in &state.transitions {
            match *t {
                Transition::Rule {
                    start,
                    rule,
                    follow,
                } => {
                    if called[rule] {
                        continue;
                    }
                    called[rule] = true;
                    ctx.push(follow);
                    self.next_in_rule(start, ctx, out, end, busy, called);
                    ctx.pop();
                    called[rule] = false;
                }
                Transition::Epsilon(target) => {
                    self.next_in_rule(target, ctx, out, end, busy, called)
                }
                Transition::Atom(ty, _) => out.insert(ty),
                Transition::Set(set, _) => out.union(&self.sets[set]),
            }
        }
    }

    fn ll1_sets(&self, decision: usize) -> Option<Vec<TokenSet>> {
        let state = &self.states[self.decisions[decision]];
        let mut sets: Vec<TokenSet> = Vec::new();
        let mut combined = TokenSet::new();
        for t in &state.transitions {
            let look = self.look(t.target());
            if look.is_empty() || look.intersects(&combined) {
                return None;
            }
            combined.union(&look);
            sets.push(look);
        }
        Some(sets)
    }
}

struct Builder {
    atn: Atn,
    rule: usize,
}

impl Builder {
    fn state(&mut self, kind: Kind) -> usize {
        self.atn.states.push(State {
            kind,
            rule: self.rule,
            transitions: Vec::new(),
            decision: None,
        });
        self.atn.states.len() - 1
    }

    fn decision(&mut self, state: usize) {
        self.atn.states[state].decision = Some(self.atn.decisions.len());
        self.atn.decisions.push(state);
    }

    fn epsilon(&mut self, from: usize, to: usize) {
        self.atn.states[from]
            .transitions
            .push(Transition::Epsilon(to));
    }

    /// A block of alternatives with a repetition: its entry and exit states.
    fn block(&mut self, alts: &[grammar::Alt], suffix: Suffix) -> (usize, usize) {
        if suffix == Suffix::One && alts.len() == 1 {
            return self.alt(&alts[0]);
        }
        let start_kind = match suffix {
            Suffix::One | Suffix::Optional => Kind::BlockStart,
            Suffix::Star => Kind::StarBlockStart,
            Suffix::Plus => Kind::PlusBlockStart,
        };
        let start = self.state(start_kind);
        if matches!(suffix, Suffix::One | Suffix::Optional) || alts.len() > 1 {
            self.decision(start);
        }
        let end = self.state(Kind::BlockEnd);
        for alt in alts {
            let (l, r) = self.alt(alt);
            self.epsilon(start, l);
            self.epsilon(r, end);
        }
        match suffix {
            Suffix::One => (start, end),
            Suffix::Optional => {
                self.epsilon(start, end);
                (start, end)
            }
            Suffix::Star => {
                let entry = self.state(Kind::StarLoopEntry);
                self.decision(entry);
                let loop_end = self.state(Kind::LoopEnd);
                let back = self.state(Kind::StarLoopBack);
                self.epsilon(entry, start);
                self.epsilon(entry, loop_end);
                self.epsilon(end, back);
                self.epsilon(back, entry);
                (entry, loop_end)
            }
            Suffix::Plus => {
                let back = self.state(Kind::PlusLoopBack);
                self.decision(back);
                let loop_end = self.state(Kind::LoopEnd);
                self.epsilon(end, back);
                self.epsilon(back, start);
                self.epsilon(back, loop_end);
                (start, loop_end)
            }
        }
    }

    fn alt(&mut self, elements: &[Element]) -> (usize, usize) {
        if elements.is_empty() {
            let l = self.state(Kind::Basic);
            let r = self.state(Kind::Basic);
            self.epsilon(l, r);
            return (l, r);
        }
        let handles: Vec<(usize, usize)> = elements.iter().map(|e| self.element(e)).collect();
        for w in handles.windows(2) {
            self.epsilon(w[0].1, w[1].0);
        }
        (handles[0].0, handles[handles.len() - 1].1)
    }

    fn element(&mut self, e: &Element) -> (usize, usize) {
        if e.suffix != Suffix::One {
            let alts = match &e.atom {
                Atom::Block(alts) => alts.clone(),
                atom => vec![vec![Element {
                    atom: atom.clone(),
                    suffix: Suffix::One,
                }]],
            };
            return self.block(&alts, e.suffix);
        }
        match &e.atom {
            Atom::Block(alts) => self.block(alts, Suffix::One),
            Atom::Token(ty) => {
                let l = self.state(Kind::Basic);
                let r = self.state(Kind::Basic);
                self.atn.states[l]
                    .transitions
                    .push(Transition::Atom(*ty, r));
                (l, r)
            }
            Atom::Set(tokens) => {
                let l = self.state(Kind::Basic);
                let r = self.state(Kind::Basic);
                self.atn.sets.push(TokenSet::of(tokens));
                let set = self.atn.sets.len() - 1;
                self.atn.states[l].transitions.push(Transition::Set(set, r));
                (l, r)
            }
            Atom::Rule(rule) => {
                let l = self.state(Kind::Basic);
                let r = self.state(Kind::Basic);
                let start = self.atn.rule_start[*rule];
                self.atn.states[l].transitions.push(Transition::Rule {
                    start,
                    rule: *rule,
                    follow: r,
                });
                (l, r)
            }
        }
    }
}
