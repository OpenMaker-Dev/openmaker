//! Adaptive prediction, ANTLR 4's `ParserATNSimulator` (ALL(*)): which
//! alternative of a decision the coming tokens select.
//!
//! Prediction first runs SLL: every alternative is simulated from the
//! decision with an empty outer context, so a rule end continues into every
//! caller. It stops at a token that leaves one alternative, or at an SLL
//! conflict; a conflict is retried with the parser's real call stack (full
//! LL), and a remaining ambiguity resolves to the lowest alternative. When
//! every alternative dies, the alternative of a configuration that already
//! left the decision's rule is taken (so the parser goes on and fails
//! later, at its own `match`); without one prediction fails at the token
//! that killed the last alternative. FileMaker's syntax error positions
//! (execute-sql.md 3.3) are these failures.
//!
//! Contexts are ANTLR's graph-structured stacks with its merge rules, since
//! the conflict tests group configurations by (state, context). SLL results
//! are cached per decision as a DFA, as ANTLR does.
//!
//! An SLL context holds at most [`MAX_DEPTH`] rule invocations below the
//! decision: SLL prediction that would enter a rule below that fails at the
//! token it was looking past, and caches nothing for it. Without the bound,
//! SLL lookahead over deeply nested text cached DFA states for every level
//! of the text before the parser reached its own limit. Full LL contexts
//! are not bounded, since they grow with the length of flat statements as
//! well: a bare subquery can take the following `UNION` parts into itself,
//! so for a subquery in a statement of many `UNION` parts, full LL follows
//! an alternative nested one level deeper for each part (FileMaker answers
//! statements of 52 such parts, execute-sql.md q13). Merging, comparing
//! and dropping contexts do not use the thread's stack in proportion to
//! their depth.

use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::rc::Rc;

use super::atn::{Atn, Kind, Transition};
use super::lex::{TokenType, EOF};
use super::MAX_DEPTH;

const EMPTY_RETURN: u32 = u32::MAX;

/// A fast hasher for the predictor's small keys (state numbers, alternatives,
/// cached context hashes): the multiply-rotate hash of rustc's `FxHasher`.
#[derive(Default, Clone, Copy)]
struct FastHasher(u64);

impl Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }

    fn write_u64(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_usize(&mut self, word: usize) {
        self.write_u64(word as u64);
    }

    fn write_u32(&mut self, word: u32) {
        self.write_u64(u64::from(word));
    }

    fn write_u16(&mut self, word: u16) {
        self.write_u64(u64::from(word));
    }
}

type HashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FastHasher>>;
type HashSet<K> = std::collections::HashSet<K, BuildHasherDefault<FastHasher>>;

/// A prediction context: return states (sorted, the empty path last as
/// [`EMPTY_RETURN`]) with their parents.
#[derive(Debug)]
pub struct Ctx {
    returns: Vec<u32>,
    parents: Vec<Option<Rc<Ctx>>>,
    hash: u64,
    /// The return states on its longest path (0 for the empty context).
    depth: usize,
}

impl PartialEq for Ctx {
    fn eq(&self, other: &Ctx) -> bool {
        std::ptr::eq(self, other)
            || (self.hash == other.hash
                && self.returns == other.returns
                && crate::stack::guard(|| self.parents == other.parents))
    }
}

impl Drop for Ctx {
    /// Drops the parents only this context holds one after another, rather
    /// than recursively: full LL contexts can be thousands deep.
    fn drop(&mut self) {
        let mut unique = Vec::new();
        detach(&mut self.parents, &mut unique);
        while let Some(parent) = unique.pop() {
            if let Ok(mut parent) = Rc::try_unwrap(parent) {
                detach(&mut parent.parents, &mut unique);
            }
        }
    }
}

/// Moves the parents that no other context holds into `unique` and drops
/// the others' references.
fn detach(parents: &mut Vec<Option<Rc<Ctx>>>, unique: &mut Vec<Rc<Ctx>>) {
    for parent in parents.drain(..).flatten() {
        if Rc::strong_count(&parent) == 1 {
            unique.push(parent);
        }
    }
}

impl Eq for Ctx {}

impl Hash for Ctx {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

fn make(returns: Vec<u32>, parents: Vec<Option<Rc<Ctx>>>) -> Rc<Ctx> {
    let mut h = FastHasher::default();
    returns.hash(&mut h);
    for p in &parents {
        p.as_ref().map(|p| p.hash).hash(&mut h);
    }
    let depth = returns
        .iter()
        .zip(&parents)
        .filter(|(r, _)| **r != EMPTY_RETURN)
        .map(|(_, p)| 1 + p.as_ref().map_or(0, |p| p.depth))
        .max()
        .unwrap_or(0);
    Rc::new(Ctx {
        returns,
        parents,
        hash: h.finish(),
        depth,
    })
}

thread_local! {
    static EMPTY: Rc<Ctx> = make(vec![EMPTY_RETURN], vec![None]);
}

pub fn empty() -> Rc<Ctx> {
    EMPTY.with(Rc::clone)
}

impl Ctx {
    pub fn is_empty(&self) -> bool {
        self.returns.len() == 1 && self.returns[0] == EMPTY_RETURN
    }

    fn has_empty_path(&self) -> bool {
        self.returns.last() == Some(&EMPTY_RETURN)
    }

    fn singleton(parent: Option<Rc<Ctx>>, ret: u32) -> Rc<Ctx> {
        if ret == EMPTY_RETURN && parent.is_none() {
            return empty();
        }
        make(vec![ret], vec![parent])
    }

    /// `parent` with `ret` pushed.
    pub fn push(parent: &Rc<Ctx>, ret: usize) -> Rc<Ctx> {
        Ctx::singleton(Some(parent.clone()), ret as u32)
    }
}

fn same(a: &Option<Rc<Ctx>>, b: &Option<Rc<Ctx>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Merged contexts by their operands, kept alive with them so that their
/// addresses stay theirs (ANTLR's `mergeCache`, emptied after each
/// prediction).
type MergeCache = HashMap<(*const Ctx, *const Ctx, bool), (Rc<Ctx>, Rc<Ctx>, Rc<Ctx>)>;

thread_local! {
    static MERGES: std::cell::RefCell<MergeCache> = std::cell::RefCell::new(HashMap::default());
}

fn clear_merge_cache() {
    MERGES.with(|m| m.borrow_mut().clear());
}

/// ANTLR's `PredictionContext.merge`.
fn merge(a: &Rc<Ctx>, b: &Rc<Ctx>, wildcard: bool) -> Rc<Ctx> {
    if Rc::ptr_eq(a, b) {
        return a.clone();
    }
    let key = (Rc::as_ptr(a), Rc::as_ptr(b), wildcard);
    if let Some(hit) = MERGES.with(|m| m.borrow().get(&key).map(|e| e.2.clone())) {
        return hit;
    }
    // Merging recurses through both contexts' parents.
    let merged = crate::stack::guard(|| merge_uncached(a, b, wildcard));
    MERGES.with(|m| {
        m.borrow_mut()
            .insert(key, (a.clone(), b.clone(), merged.clone()))
    });
    merged
}

fn merge_uncached(a: &Rc<Ctx>, b: &Rc<Ctx>, wildcard: bool) -> Rc<Ctx> {
    if a == b {
        return a.clone();
    }
    if a.returns.len() == 1 && b.returns.len() == 1 {
        return merge_singletons(a, b, wildcard);
    }
    if wildcard {
        if a.is_empty() {
            return a.clone();
        }
        if b.is_empty() {
            return b.clone();
        }
    }
    merge_arrays(a, b, wildcard)
}

fn merge_root(a: &Rc<Ctx>, b: &Rc<Ctx>, wildcard: bool) -> Option<Rc<Ctx>> {
    if wildcard {
        if a.is_empty() || b.is_empty() {
            return Some(empty());
        }
    } else {
        if a.is_empty() && b.is_empty() {
            return Some(empty());
        }
        if a.is_empty() {
            return Some(make(
                vec![b.returns[0], EMPTY_RETURN],
                vec![b.parents[0].clone(), None],
            ));
        }
        if b.is_empty() {
            return Some(make(
                vec![a.returns[0], EMPTY_RETURN],
                vec![a.parents[0].clone(), None],
            ));
        }
    }
    None
}

fn merge_parents(a: &Option<Rc<Ctx>>, b: &Option<Rc<Ctx>>, wildcard: bool) -> Option<Rc<Ctx>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(merge(a, b, wildcard)),
        (Some(a), None) | (None, Some(a)) => Some(a.clone()),
        (None, None) => None,
    }
}

fn merge_singletons(a: &Rc<Ctx>, b: &Rc<Ctx>, wildcard: bool) -> Rc<Ctx> {
    if let Some(root) = merge_root(a, b, wildcard) {
        return root;
    }
    let (ra, rb) = (a.returns[0], b.returns[0]);
    let (pa, pb) = (&a.parents[0], &b.parents[0]);
    if ra == rb {
        let parent = merge_parents(pa, pb, wildcard);
        if same(&parent, pa) {
            return a.clone();
        }
        if same(&parent, pb) {
            return b.clone();
        }
        return Ctx::singleton(parent, ra);
    }
    if pa.is_some() && same(pa, pb) {
        let (lo, hi) = (ra.min(rb), ra.max(rb));
        return make(vec![lo, hi], vec![pa.clone(), pa.clone()]);
    }
    if ra > rb {
        make(vec![rb, ra], vec![pb.clone(), pa.clone()])
    } else {
        make(vec![ra, rb], vec![pa.clone(), pb.clone()])
    }
}

fn merge_arrays(a: &Rc<Ctx>, b: &Rc<Ctx>, wildcard: bool) -> Rc<Ctx> {
    let (mut i, mut j) = (0, 0);
    let mut returns = Vec::with_capacity(a.returns.len() + b.returns.len());
    let mut parents = Vec::with_capacity(returns.capacity());
    while i < a.returns.len() && j < b.returns.len() {
        let (pa, pb) = (&a.parents[i], &b.parents[j]);
        if a.returns[i] == b.returns[j] {
            let payload = a.returns[i];
            let both_empty = payload == EMPTY_RETURN && pa.is_none() && pb.is_none();
            let same_parent = pa.is_some() && pb.is_some() && same(pa, pb);
            if both_empty || same_parent {
                parents.push(pa.clone());
            } else {
                parents.push(merge_parents(pa, pb, wildcard));
            }
            returns.push(payload);
            i += 1;
            j += 1;
        } else if a.returns[i] < b.returns[j] {
            parents.push(pa.clone());
            returns.push(a.returns[i]);
            i += 1;
        } else {
            parents.push(pb.clone());
            returns.push(b.returns[j]);
            j += 1;
        }
    }
    while i < a.returns.len() {
        parents.push(a.parents[i].clone());
        returns.push(a.returns[i]);
        i += 1;
    }
    while j < b.returns.len() {
        parents.push(b.parents[j].clone());
        returns.push(b.returns[j]);
        j += 1;
    }
    if returns.len() == 1 {
        return Ctx::singleton(parents.pop().flatten(), returns[0]);
    }
    let m = make(returns, parents);
    if m == *a {
        return a.clone();
    }
    if m == *b {
        return b.clone();
    }
    m
}

/// An ATN configuration: a state reached while predicting `alt`, with the
/// context it returns through.
#[derive(Clone, Debug)]
struct Config {
    state: usize,
    alt: u16,
    ctx: Rc<Ctx>,
    /// How far the configuration left the decision's rule (ANTLR's
    /// `reachesIntoOuterContext`).
    outer: u32,
}

type ConfigKey = (usize, u16, Rc<Ctx>);

impl Config {
    fn key(&self) -> ConfigKey {
        (self.state, self.alt, self.ctx.clone())
    }
}

/// ANTLR's `ATNConfigSet`: configurations merged by (state, alt).
#[derive(Clone, Debug, Default)]
struct Configs {
    items: Vec<Config>,
    index: HashMap<(usize, u16), usize>,
    full: bool,
}

impl Configs {
    fn new(full: bool) -> Configs {
        Configs {
            full,
            ..Configs::default()
        }
    }

    fn add(&mut self, c: Config) {
        match self.index.get(&(c.state, c.alt)) {
            Some(&i) => {
                let existing = &mut self.items[i];
                existing.outer = existing.outer.max(c.outer);
                existing.ctx = merge(&existing.ctx, &c.ctx, !self.full);
            }
            None => {
                self.index.insert((c.state, c.alt), self.items.len());
                self.items.push(c);
            }
        }
    }

    fn unique_alt(&self) -> Option<u16> {
        let first = self.items.first()?.alt;
        self.items.iter().all(|c| c.alt == first).then_some(first)
    }

    /// The alternative sets of configurations grouped by (state, context).
    fn conflicting_subsets(&self) -> Vec<Vec<u16>> {
        if self.items.len() <= 32 {
            // Few configurations (full LL): no hashing.
            let mut groups: Vec<(usize, &Rc<Ctx>, Vec<u16>)> = Vec::new();
            for c in &self.items {
                match groups.iter_mut().find(|g| g.0 == c.state && *g.1 == c.ctx) {
                    Some(g) => {
                        if !g.2.contains(&c.alt) {
                            g.2.push(c.alt);
                        }
                    }
                    None => groups.push((c.state, &c.ctx, vec![c.alt])),
                }
            }
            return groups.into_iter().map(|g| g.2).collect();
        }
        let mut groups: HashMap<(usize, Rc<Ctx>), Vec<u16>> = HashMap::default();
        for c in &self.items {
            let alts = groups.entry((c.state, c.ctx.clone())).or_default();
            if !alts.contains(&c.alt) {
                alts.push(c.alt);
            }
        }
        groups.into_values().collect()
    }

    fn state_has_one_alt(&self) -> bool {
        let mut by_state: HashMap<usize, Vec<u16>> = HashMap::default();
        for c in &self.items {
            let alts = by_state.entry(c.state).or_default();
            if !alts.contains(&c.alt) {
                alts.push(c.alt);
            }
        }
        by_state.values().any(|alts| alts.len() == 1)
    }

    fn list_key(&self) -> Vec<ConfigKey> {
        self.items.iter().map(Config::key).collect()
    }
}

/// `PredictionMode.getSingleViableAlt`.
fn single_viable_alt(subsets: &[Vec<u16>]) -> Option<u16> {
    let mut viable: Option<u16> = None;
    for alts in subsets {
        let min = *alts.iter().min()?;
        match viable {
            None => viable = Some(min),
            Some(v) if v != min => return None,
            _ => {}
        }
    }
    viable
}

#[derive(Clone, Copy, Debug)]
enum Edge {
    Error,
    To(usize),
}

#[derive(Debug)]
struct DfaState {
    configs: Configs,
    edges: HashMap<TokenType, Edge>,
    prediction: Option<u16>,
    requires_full: bool,
}

#[derive(Debug, Default)]
struct Dfa {
    states: Vec<DfaState>,
    by_configs: HashMap<Vec<ConfigKey>, usize>,
    s0: Option<usize>,
}

/// What prediction reads: the tokens from the decision on.
pub trait Input {
    /// The type of the token `k` tokens ahead of the decision (0 is the
    /// first), marking it as seen.
    fn la(&mut self, k: usize) -> TokenType;
}

/// Prediction failed at the token `k` tokens ahead of the decision: no
/// alternative is viable, or following them would nest the statement
/// deeper than [`MAX_DEPTH`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoViableAlt {
    pub offset: usize,
}

/// An SLL closure would have entered a rule below [`MAX_DEPTH`].
#[derive(Clone, Copy, Debug)]
struct TooDeep;

pub struct Predictor<'a> {
    atn: &'a Atn,
    dfas: Vec<Dfa>,
    /// The configurations the DFA states hold, and one for each state:
    /// what the cache's memory grows with.
    cached: usize,
    /// The closure's work list, kept between calls.
    stack: std::cell::RefCell<Vec<Step>>,
}

impl<'a> Predictor<'a> {
    pub fn new(atn: &'a Atn) -> Predictor<'a> {
        Predictor {
            atn,
            dfas: (0..atn.decisions.len()).map(|_| Dfa::default()).collect(),
            cached: 0,
            stack: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// How much the cache holds: its DFA states and their configurations.
    pub fn cached(&self) -> usize {
        self.cached
    }

    /// The alternative (from 1) of `decision` the input predicts; `outer`
    /// is the parser's call stack as a context (the follow states of the
    /// rule invocations, innermost first).
    pub fn predict(
        &mut self,
        decision: usize,
        input: &mut dyn Input,
        outer: &dyn Fn() -> Rc<Ctx>,
    ) -> Result<u16, NoViableAlt> {
        clear_merge_cache();
        let start = self.atn.decisions[decision];
        // A closure that is too deep fails at the token after the one it
        // follows (the decision's first token for a start state).
        let too_deep = |k: usize| move |_: TooDeep| NoViableAlt { offset: k };
        let s0 = match self.dfas[decision].s0 {
            Some(s0) => s0,
            None => {
                let configs = self
                    .start_state(start, empty(), false)
                    .map_err(too_deep(0))?;
                let s0 = self.add_dfa_state(decision, configs);
                self.dfas[decision].s0 = Some(s0);
                s0
            }
        };
        let mut previous = s0;
        let mut k = 0;
        loop {
            let t = input.la(k);
            let next = match self.dfas[decision].states[previous].edges.get(&t) {
                Some(&edge) => edge,
                None => self
                    .compute_target(decision, previous, t)
                    .map_err(too_deep(k + 1))?,
            };
            let d = match next {
                Edge::Error => {
                    let configs = &self.dfas[decision].states[previous].configs;
                    return finished_alt(self.atn, configs).ok_or(NoViableAlt { offset: k });
                }
                Edge::To(d) => d,
            };
            let state = &self.dfas[decision].states[d];
            if state.requires_full {
                let configs = self
                    .start_state(start, outer(), true)
                    .map_err(too_deep(0))?;
                return self.full_context(configs, input);
            }
            if let Some(alt) = state.prediction {
                return Ok(alt);
            }
            previous = d;
            if t != EOF {
                k += 1;
            }
        }
    }

    fn add_dfa_state(&mut self, decision: usize, configs: Configs) -> usize {
        let key = configs.list_key();
        let dfa = &mut self.dfas[decision];
        if let Some(&existing) = dfa.by_configs.get(&key) {
            return existing;
        }
        let mut state = DfaState {
            configs,
            edges: HashMap::default(),
            prediction: None,
            requires_full: false,
        };
        if let Some(alt) = state.configs.unique_alt() {
            state.prediction = Some(alt);
        } else if sll_conflict_ends_prediction(self.atn, &state.configs) {
            state.requires_full = true;
        }
        self.cached += state.configs.items.len() + 1;
        dfa.states.push(state);
        dfa.by_configs.insert(key, dfa.states.len() - 1);
        dfa.states.len() - 1
    }

    /// The DFA edge from `previous` on `t`, computed and cached; nothing
    /// is cached when the closure is too deep.
    fn compute_target(
        &mut self,
        decision: usize,
        previous: usize,
        t: TokenType,
    ) -> Result<Edge, TooDeep> {
        let configs = self.dfas[decision].states[previous].configs.clone();
        let edge = match self.reach(&configs, t, false)? {
            None => Edge::Error,
            Some(reach) => Edge::To(self.add_dfa_state(decision, reach)),
        };
        self.dfas[decision].states[previous].edges.insert(t, edge);
        Ok(edge)
    }

    fn full_context(&self, s0: Configs, input: &mut dyn Input) -> Result<u16, NoViableAlt> {
        let mut previous = s0;
        let mut k = 0;
        loop {
            let t = input.la(k);
            let reach = self
                .reach(&previous, t, true)
                .map_err(|TooDeep| NoViableAlt { offset: k + 1 })?;
            let Some(reach) = reach else {
                return finished_alt(self.atn, &previous).ok_or(NoViableAlt { offset: k });
            };
            if let Some(alt) = reach.unique_alt() {
                return Ok(alt);
            }
            if let Some(alt) = single_viable_alt(&reach.conflicting_subsets()) {
                return Ok(alt);
            }
            previous = reach;
            if t != EOF {
                k += 1;
            }
        }
    }

    /// `computeStartState`.
    fn start_state(
        &self,
        decision_state: usize,
        ctx: Rc<Ctx>,
        full: bool,
    ) -> Result<Configs, TooDeep> {
        let mut configs = Configs::new(full);
        for (i, t) in self.atn.states[decision_state]
            .transitions
            .iter()
            .enumerate()
        {
            let c = Config {
                state: t.target(),
                alt: i as u16 + 1,
                ctx: ctx.clone(),
                outer: 0,
            };
            let mut busy = HashSet::default();
            self.closure(c, &mut configs, &mut busy, full, false, 0)?;
        }
        Ok(configs)
    }

    /// `computeReachSet`.
    fn reach(
        &self,
        closure: &Configs,
        t: TokenType,
        full: bool,
    ) -> Result<Option<Configs>, TooDeep> {
        let mut intermediate = Configs::new(full);
        let mut skipped_stops = Vec::new();
        for c in &closure.items {
            let state = &self.atn.states[c.state];
            if state.kind == Kind::RuleStop {
                if full || t == EOF {
                    skipped_stops.push(c.clone());
                }
                continue;
            }
            for tr in &state.transitions {
                let target = match *tr {
                    Transition::Atom(ty, target) if ty == t => target,
                    Transition::Set(set, target) if self.atn.sets[set].contains(t) => target,
                    _ => continue,
                };
                intermediate.add(Config {
                    state: target,
                    ..c.clone()
                });
            }
        }
        let mut reach = None;
        if skipped_stops.is_empty()
            && t != EOF
            && (intermediate.items.len() == 1 || intermediate.unique_alt().is_some())
        {
            reach = Some(intermediate.clone());
        }
        let trivially = reach.is_some();
        let mut reach = match reach {
            Some(reach) => reach,
            None => {
                let mut out = Configs::new(full);
                let mut busy = HashSet::default();
                for c in &intermediate.items {
                    self.closure(c.clone(), &mut out, &mut busy, full, t == EOF, 0)?;
                }
                out
            }
        };
        if t == EOF {
            reach = self.only_rule_stops(reach, trivially);
        }
        if !skipped_stops.is_empty()
            && (!full
                || !reach
                    .items
                    .iter()
                    .any(|c| self.atn.states[c.state].kind == Kind::RuleStop))
        {
            for c in skipped_stops {
                reach.add(c);
            }
        }
        Ok((!reach.items.is_empty()).then_some(reach))
    }

    /// `removeAllConfigsNotInRuleStopState`.
    fn only_rule_stops(&self, configs: Configs, look_to_end: bool) -> Configs {
        let is_stop = |c: &Config| self.atn.states[c.state].kind == Kind::RuleStop;
        if configs.items.iter().all(is_stop) {
            return configs;
        }
        let mut out = Configs::new(configs.full);
        for c in &configs.items {
            if is_stop(c) {
                out.add(c.clone());
            } else if look_to_end && self.atn.states[c.state].only_epsilon() {
                let (_, end) = self.atn.next_tokens_in_rule(c.state);
                if end {
                    let stop = self.atn.rule_stop[self.atn.states[c.state].rule];
                    out.add(Config {
                        state: stop,
                        ..c.clone()
                    });
                }
            }
        }
        out
    }

    /// ANTLR's `closure` (`closureCheckingStopState` and `closure_`): every
    /// configuration reachable from `c` without a token. ANTLR recurses;
    /// this walks the same depth-first order with an explicit stack (the
    /// epsilon paths through rule ends can be hundreds of states long), so
    /// configurations are added, and busy checks made, in ANTLR's order.
    fn closure(
        &self,
        c: Config,
        configs: &mut Configs,
        busy: &mut HashSet<ConfigKey>,
        full: bool,
        eof_as_epsilon: bool,
        depth: i32,
    ) -> Result<(), TooDeep> {
        let mut stack = std::mem::take(&mut *self.stack.borrow_mut());
        stack.push(Step::Check(c, depth));
        let result = loop {
            let Some(step) = stack.pop() else {
                break Ok(());
            };
            match step {
                Step::Add(c) => configs.add(c),
                Step::Busy(c, depth) => {
                    if busy.insert(c.key()) {
                        stack.push(Step::Check(c, depth));
                    }
                }
                Step::Check(c, depth) => {
                    if self.atn.states[c.state].kind == Kind::RuleStop {
                        if !c.ctx.is_empty() {
                            // Pushed last first, so they run in order.
                            for i in (0..c.ctx.returns.len()).rev() {
                                let ret = c.ctx.returns[i];
                                if ret == EMPTY_RETURN {
                                    if full {
                                        stack.push(Step::Add(Config {
                                            ctx: empty(),
                                            ..c.clone()
                                        }));
                                    } else {
                                        stack.push(Step::Inner(c.clone(), depth));
                                    }
                                    continue;
                                }
                                let parent = c.ctx.parents[i].clone().unwrap_or_else(empty);
                                stack.push(Step::Check(
                                    Config {
                                        state: ret as usize,
                                        alt: c.alt,
                                        ctx: parent,
                                        outer: c.outer,
                                    },
                                    depth - 1,
                                ));
                            }
                            continue;
                        } else if full {
                            configs.add(c);
                            continue;
                        }
                    }
                    stack.push(Step::Inner(c, depth));
                }
                Step::Inner(c, depth) => {
                    if let Err(e) =
                        self.closure_inner(c, configs, &mut stack, eof_as_epsilon, depth)
                    {
                        break Err(e);
                    }
                }
            }
        };
        stack.clear();
        *self.stack.borrow_mut() = stack;
        result
    }

    /// `closure_`: adds `c` unless its state only has epsilon transitions,
    /// and queues the configurations its epsilon transitions reach.
    fn closure_inner(
        &self,
        c: Config,
        configs: &mut Configs,
        stack: &mut Vec<Step>,
        eof_as_epsilon: bool,
        depth: i32,
    ) -> Result<(), TooDeep> {
        let state = &self.atn.states[c.state];
        if !state.only_epsilon() {
            configs.add(c.clone());
        }
        // Pushed last first, so they run in order.
        for t in state.transitions.iter().rev() {
            let mut n = match *t {
                Transition::Rule { start, follow, .. } => {
                    let ctx = Ctx::push(&c.ctx, follow);
                    if !configs.full && ctx.depth > MAX_DEPTH {
                        return Err(TooDeep);
                    }
                    Config {
                        state: start,
                        ctx,
                        ..c.clone()
                    }
                }
                Transition::Epsilon(target) => Config {
                    state: target,
                    ..c.clone()
                },
                Transition::Atom(ty, target) if eof_as_epsilon && ty == EOF => Config {
                    state: target,
                    ..c.clone()
                },
                Transition::Set(set, target)
                    if eof_as_epsilon && self.atn.sets[set].contains(EOF) =>
                {
                    Config {
                        state: target,
                        ..c.clone()
                    }
                }
                _ => continue,
            };
            if state.kind == Kind::RuleStop {
                // Fell off the end of the decision's rule.
                n.outer += 1;
                stack.push(Step::Busy(n, depth - 1));
            } else if !t.is_epsilon() {
                stack.push(Step::Busy(n, depth));
            } else if matches!(t, Transition::Rule { .. }) && depth >= 0 {
                stack.push(Step::Check(n, depth + 1));
            } else {
                stack.push(Step::Check(n, depth));
            }
        }
        Ok(())
    }
}

/// A pending step of [`Predictor::closure`].
enum Step {
    /// `closureCheckingStopState`.
    Check(Config, i32),
    /// `closure_`.
    Inner(Config, i32),
    /// The busy check, then `closureCheckingStopState`.
    Busy(Config, i32),
    Add(Config),
}

/// `hasSLLConflictTerminatingPrediction` (prediction mode LL).
fn sll_conflict_ends_prediction(atn: &Atn, configs: &Configs) -> bool {
    if configs
        .items
        .iter()
        .all(|c| atn.states[c.state].kind == Kind::RuleStop)
    {
        return true;
    }
    configs
        .conflicting_subsets()
        .iter()
        .any(|alts| alts.len() > 1)
        && !configs.state_has_one_alt()
}

/// `getAltThatFinishedDecisionEntryRule`: the lowest alternative of a
/// configuration that left the decision's rule.
fn finished_alt(atn: &Atn, configs: &Configs) -> Option<u16> {
    configs
        .items
        .iter()
        .filter(|c| {
            c.outer > 0 || (atn.states[c.state].kind == Kind::RuleStop && c.ctx.has_empty_path())
        })
        .map(|c| c.alt)
        .min()
}
