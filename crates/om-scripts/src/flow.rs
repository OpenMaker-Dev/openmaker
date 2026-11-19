//! Block structure and the step indexes block steps cache
//! (script-runtime.md 3.1, script-steps.md 3.1).
//!
//! Only enabled steps take part: a disabled If or End If is not a block
//! boundary, and disabled block steps keep whatever index they stored
//! (stale values, 0 or their own index + 1, in the corpus).

use crate::step::ScriptStep;
use crate::ScriptError;

pub const IF: u16 = 68;
pub const ELSE: u16 = 69;
pub const END_IF: u16 = 70;
pub const LOOP: u16 = 71;
pub const EXIT_LOOP_IF: u16 = 72;
pub const END_LOOP: u16 = 73;
pub const ELSE_IF: u16 = 125;
pub const OPEN_TRANSACTION: u16 = 205;
pub const COMMIT_TRANSACTION: u16 = 206;
pub const GO_TO_RECORD: u16 = 16;
pub const GO_TO_PORTAL_ROW: u16 = 99;

/// Whether an enabled step of this id caches a step index in `mStepValue`.
pub fn caches_index(id: u16) -> bool {
    matches!(
        id,
        IF | ELSE
            | END_IF
            | LOOP
            | EXIT_LOOP_IF
            | END_LOOP
            | ELSE_IF
            | OPEN_TRANSACTION
            | COMMIT_TRANSACTION
    )
}

/// What an enabled step must cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cached {
    /// The step value: the partner's index.
    pub value: Option<u16>,
    /// Numeric data `[00 0, 03]`: Else If's End If index, or the index of
    /// the End Loop enclosing a Go to Record / Go to Portal Row step (-1
    /// outside a loop). The latter is written whether or not "Exit after
    /// last" is set (corpus: 270/270 steps).
    pub numeric: Option<i32>,
}

enum Block {
    If {
        last: usize,
        after_else: bool,
        else_ifs: Vec<usize>,
    },
    Loop {
        start: usize,
        exits: Vec<usize>,
    },
    Transaction {
        start: usize,
    },
}

impl Block {
    fn name(&self) -> &'static str {
        match self {
            Block::If { .. } => "If",
            Block::Loop { .. } => "Loop",
            Block::Transaction { .. } => "Open Transaction",
        }
    }
}

fn index(i: usize) -> Result<u16, ScriptError> {
    u16::try_from(i)
        .map_err(|_| ScriptError(format!("Step {} is beyond the 16-bit step index.", i + 1)))
}

/// Check that the enabled steps nest properly and compute the index every
/// enabled block step caches. Errors name the offending step (1-based).
pub fn analyze(steps: &[ScriptStep]) -> Result<Vec<Cached>, ScriptError> {
    let mut out = vec![Cached::default(); steps.len()];
    let mut stack: Vec<(usize, Block)> = Vec::new();
    // Go to Record/Portal Row steps inside each open Loop.
    let mut exit_after_last: Vec<Vec<usize>> = Vec::new();
    let error = |i: usize, what: &str| {
        Err(ScriptError(format!(
            "Step {} ({}) {what}.",
            i + 1,
            steps[i].name()
        )))
    };
    for (i, step) in steps.iter().enumerate() {
        if !step.is_enabled() {
            continue;
        }
        match step.id() {
            IF => stack.push((
                i,
                Block::If {
                    last: i,
                    after_else: false,
                    else_ifs: Vec::new(),
                },
            )),
            id @ (ELSE_IF | ELSE) => match stack.last_mut() {
                Some((
                    _,
                    Block::If {
                        last,
                        after_else,
                        else_ifs,
                    },
                )) if !*after_else => {
                    out[*last].value = Some(index(i)?);
                    *last = i;
                    if id == ELSE {
                        *after_else = true;
                    } else {
                        else_ifs.push(i);
                    }
                }
                Some((_, Block::If { .. })) => return error(i, "follows an Else"),
                Some((open, block)) => {
                    return error(
                        i,
                        &format!("is inside the {} at step {}", block.name(), *open + 1),
                    )
                }
                None => return error(i, "has no matching If"),
            },
            END_IF => match stack.pop() {
                Some((_, Block::If { last, else_ifs, .. })) => {
                    out[last].value = Some(index(i)?);
                    out[i].value = Some(0);
                    for e in else_ifs {
                        out[e].numeric = Some(i32::from(index(i)?));
                    }
                }
                Some((open, block)) => {
                    return error(
                        i,
                        &format!("closes the {} at step {}", block.name(), open + 1),
                    )
                }
                None => return error(i, "has no matching If"),
            },
            LOOP => {
                stack.push((
                    i,
                    Block::Loop {
                        start: i,
                        exits: Vec::new(),
                    },
                ));
                exit_after_last.push(Vec::new());
            }
            EXIT_LOOP_IF => match stack.iter_mut().rev().find_map(|(_, b)| match b {
                Block::Loop { exits, .. } => Some(exits),
                _ => None,
            }) {
                Some(exits) => exits.push(i),
                None => return error(i, "is outside a Loop"),
            },
            END_LOOP => match stack.pop() {
                Some((_, Block::Loop { start, exits })) => {
                    let end = index(i)?;
                    out[start].value = Some(end);
                    out[i].value = Some(index(start)?);
                    for e in exits {
                        out[e].value = Some(end);
                    }
                    for g in exit_after_last.pop().unwrap_or_default() {
                        out[g].numeric = Some(i32::from(end));
                    }
                }
                Some((open, block)) => {
                    return error(
                        i,
                        &format!("closes the {} at step {}", block.name(), open + 1),
                    )
                }
                None => return error(i, "has no matching Loop"),
            },
            OPEN_TRANSACTION => stack.push((i, Block::Transaction { start: i })),
            COMMIT_TRANSACTION => match stack.pop() {
                Some((_, Block::Transaction { start })) => {
                    out[start].value = Some(index(i)?);
                    out[i].value = Some(index(i)?);
                }
                Some((open, block)) => {
                    return error(
                        i,
                        &format!("closes the {} at step {}", block.name(), open + 1),
                    )
                }
                None => return error(i, "has no matching Open Transaction"),
            },
            GO_TO_RECORD | GO_TO_PORTAL_ROW => match exit_after_last.last_mut() {
                Some(pending) => pending.push(i),
                None => out[i].numeric = Some(-1),
            },
            _ => {}
        }
    }
    if let Some((open, block)) = stack.pop() {
        return error(
            open,
            &format!("has no matching end of its {} block", block.name()),
        );
    }
    Ok(out)
}

/// Write the cached indexes of every enabled step. The step value is always
/// written; numeric data is written for Else If, and for Go to Record / Go to
/// Portal Row when it is stored already or not -1 (FileMaker writes -1 into
/// every new step, which [`crate::Script::insert_steps`] does too).
pub fn update(steps: &mut [ScriptStep]) -> Result<(), ScriptError> {
    let cached = analyze(steps)?;
    for (step, cached) in steps.iter_mut().zip(cached) {
        if let Some(value) = cached.value {
            step.value = value;
        }
        if let Some(n) = cached.numeric {
            let stored = step.number(0);
            let write = step.id() == ELSE_IF || stored.is_some() || n != -1;
            if write && stored != Some(i64::from(n)) {
                step.set_number(0, Some(n));
            }
        }
    }
    Ok(())
}
