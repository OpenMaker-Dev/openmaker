//! `$` and `$$` variables (calc-values.md 8.3, script-runtime.md 5): a map
//! from case-folded name and repetition to a whole value, kept by the host
//! (`DBVarMap`, one per script frame for `$`, one per file for `$$`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

use crate::{Number, Value};

/// A variable repetition: the index truncated to an integer of any size, so
/// `$r[100000000000000000000]` and `$r[100000000000000000001]` are distinct
/// values (calc-values.md 8.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Repetition(Key);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Small(i64),
    /// Beyond `i64`: the integer's locale-independent text, which is
    /// distinct for distinct integers.
    Big(String),
}

impl Repetition {
    /// The repetition `n` selects: its integer part toward zero; `None` for
    /// `?`.
    pub fn from_number(n: &Number) -> Option<Repetition> {
        if n.is_nan() {
            return None;
        }
        let int = n.int();
        Some(Repetition(match int.to_i64() {
            Some(small) => Key::Small(small),
            None => Key::Big(int.to_text_with('.')),
        }))
    }

    /// The repetition as an `i64`, when it fits.
    pub fn as_i64(&self) -> Option<i64> {
        match &self.0 {
            Key::Small(n) => Some(*n),
            Key::Big(_) => None,
        }
    }
}

impl From<i64> for Repetition {
    fn from(n: i64) -> Repetition {
        Repetition(Key::Small(n))
    }
}

/// Which map a variable lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VarScope {
    /// `$name`: the running script frame's variables (script level 0 outside
    /// a script, where they persist between evaluations).
    Local,
    /// `$$name`: the file's variables for the session.
    Global,
}

impl VarScope {
    /// The scope a name selects: `$$` global, `$` local.
    pub fn of(name: &str) -> VarScope {
        if name.starts_with("$$") {
            VarScope::Global
        } else {
            VarScope::Local
        }
    }
}

/// Variables by name and repetition. Names compare case-insensitively and
/// include their dollar signs; `$x` is `$x[1]`, and every repetition (0 and
/// negative ones included) is a value of its own. Values keep their type,
/// styles and precision. Setting an empty value removes the entry: an empty
/// variable and a missing one are the same (script-runtime.md 5.3).
#[derive(Debug, Default)]
pub struct Variables {
    map: HashMap<(String, Repetition), Value>,
    /// Clones share this identity until persistent values are assigned.
    revision: u64,
    /// Withheld assignments in the current evaluation, shared by nested
    /// hosts that reborrow these variables. These are not persistent values.
    pending: HashSet<(String, Repetition)>,
    evaluation_depth: usize,
    execution_pending: bool,
    /// Reads of withheld values, including reads made by nested solvers
    /// without another source request. Callers retain that dependency when
    /// a host reduces the nested result to a bool or SQL text.
    pending_reads: u64,
}

impl Clone for Variables {
    fn clone(&self) -> Self {
        // A snapshot starts a new evaluation rather than inheriting the
        // lifetime of an evaluator that happens to be using the original.
        Self {
            map: self.map.clone(),
            revision: self.revision,
            ..Self::default()
        }
    }
}

impl Variables {
    pub fn new() -> Variables {
        Variables::default()
    }

    /// Identity of this persistent-value snapshot, independent of temporary
    /// evaluation state. Repeated delivery of a clone does not change it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The value of `name[repetition]`, `None` when unset.
    pub fn get(&self, name: &str, repetition: impl Into<Repetition>) -> Option<&Value> {
        self.map.get(&(fold(name), repetition.into()))
    }

    /// Sets `name[repetition]`; an empty value removes it.
    pub fn set(&mut self, name: &str, repetition: impl Into<Repetition>, value: Value) {
        let key = (fold(name), repetition.into());
        self.pending.remove(&key);
        self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        if value.is_empty() {
            self.map.remove(&key);
        } else {
            self.map.insert(key, value);
        }
    }

    /// Removes every variable (a script frame ending, a file closing).
    pub fn clear(&mut self) {
        if !self.map.is_empty() {
            self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        }
        self.map.clear();
        self.pending.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Every set variable as (case-folded name, repetition, value).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Repetition, &Value)> {
        self.map
            .iter()
            .map(|((name, rep), value)| (name.as_str(), rep, value))
    }

    pub(super) fn mark_pending(&mut self, name: &str, repetition: Repetition) {
        self.pending.insert((fold(name), repetition));
    }

    pub(super) fn is_pending(&self, name: &str, repetition: &Repetition) -> bool {
        !self.pending.is_empty() && self.pending.contains(&(fold(name), repetition.clone()))
    }

    pub(super) fn begin_evaluation(&mut self) {
        if self.evaluation_depth == 0 {
            self.pending.clear();
            self.execution_pending = false;
            self.pending_reads = 0;
        }
        self.evaluation_depth += 1;
    }

    pub(super) fn end_evaluation(&mut self) {
        self.evaluation_depth -= 1;
        if self.evaluation_depth == 0 {
            self.pending.clear();
            self.execution_pending = false;
            self.pending_reads = 0;
        }
    }

    pub(super) fn suspend_execution(&mut self) {
        self.execution_pending = true;
    }

    pub(super) fn execution_pending(&self) -> bool {
        self.execution_pending
    }

    pub(super) fn read_pending_value(&mut self) {
        self.pending_reads = self.pending_reads.wrapping_add(1);
    }

    pub(super) fn pending_reads(&self) -> u64 {
        self.pending_reads
    }
}

fn fold(name: &str) -> String {
    name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fold_case_and_repetitions_are_separate() {
        let mut vars = Variables::new();
        vars.set("$Case1", 1, Value::number(5));
        assert!(vars.get("$case1", 1).is_some());
        assert!(vars.get("$case1", 2).is_none());
        vars.set("$case1", -1, Value::text("n"));
        assert_eq!(vars.iter().count(), 2);
        vars.set("$CASE1", 1, Value::empty_text());
        assert!(vars.get("$case1", 1).is_none());
        assert_eq!(VarScope::of("$$g"), VarScope::Global);
        let big = |s: &str| {
            Repetition::from_number(&crate::number::Number::parse_text(s, '.', ',')).unwrap()
        };
        vars.set("$r", big("100000000000000000000"), Value::number(1));
        vars.set("$r", big("100000000000000000001"), Value::number(2));
        assert!(vars.get("$r", big("100000000000000000000")).is_some());
        assert_eq!(vars.iter().filter(|(n, _, _)| *n == "$r").count(), 2);
        assert_eq!(VarScope::of("$l"), VarScope::Local);
    }
}
