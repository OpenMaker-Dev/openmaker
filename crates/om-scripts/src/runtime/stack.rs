//! The script stack's memory guard (`ScriptRuntimeModel::OutOfStackErrorCheck`,
//! script-runtime.md 2.6 and 13.18). FileMaker has no call-depth limit;
//! instead every 100th call made while the stack holds 100 or more frames
//! estimates the stack's memory, and a call above the limit fails with error
//! 2 "Memory error" before its frame exists. The script engine's runs (13.18)
//! settle what the estimate counts:
//!
//! - every frame on the stack, not the one being created: a script frame
//!   [`SCRIPT_FRAME`] bytes, a button's single-step frame [`STEP_FRAME`];
//! - a script frame's parameter, if it has one, and each of its `$`
//!   variables: [`DATA`] per value, [`VARIABLE`] more per variable, text
//!   (values and names) at 2 bytes per code point;
//! - not script results, `$$` variables or the frame's script;
//! - [`BASE`] once.
//!
//! The runs bound the constants (a frame 1,544-1,588 bytes, a value
//! 1,228-1,268, a variable 28-50 more than a value, the base 340-600); the
//! values here lie in those ranges and reproduce every boundary the engine
//! showed, to the character of a parameter or variable.

use om_calc::Value;

use super::{Frame, FrameKind};

/// FileMaker's limit: 1 GiB.
pub const STACK_MEMORY_LIMIT: u64 = 1 << 30;
/// The guard looks only once the stack holds this many frames ...
pub(super) const CHECK_DEPTH: usize = 100;
/// ... and then on every this-many-th call, starting with the first.
pub(super) const CHECK_INTERVAL: u64 = 100;
/// A script frame.
const SCRIPT_FRAME: u64 = 0x620;
/// A button's single-step frame (`ScriptStackElementStep`, 2.6).
const STEP_FRAME: u64 = 0x128;
/// The estimate's fixed part.
const BASE: u64 = 0x200;
/// A value: a parameter or a variable's value, beyond its characters.
const DATA: u64 = 0x4e0;
/// A `$` variable beyond its value and its name's characters.
const VARIABLE: u64 = 28;

/// The estimated memory of `frames`.
pub(super) fn estimate<'a>(frames: impl IntoIterator<Item = &'a Frame>) -> u64 {
    BASE + frames.into_iter().map(frame_size).sum::<u64>()
}

fn frame_size(frame: &Frame) -> u64 {
    match &frame.kind {
        FrameKind::Script {
            vars, parameter, ..
        } => {
            let variables: u64 = vars
                .iter()
                .map(|(name, _, value)| VARIABLE + text_size(name) + value_size(value))
                .sum();
            SCRIPT_FRAME + parameter.as_deref().map_or(0, value_size) + variables
        }
        FrameKind::Step { .. } => STEP_FRAME,
    }
}

/// 2 bytes per code point: what the engine counts for text (13.18: an
/// emoji counts once, `e` with a combining accent twice).
fn text_size(text: &str) -> u64 {
    2 * text.chars().count() as u64
}

/// A value: [`DATA`] and its text, or a container's reference text and
/// streams. Numbers, dates and times count [`DATA`] alone.
fn value_size(value: &Value) -> u64 {
    DATA + match value {
        Value::Text(text) | Value::NumberText(_, text) => text_size(text.as_str()),
        Value::Container(container) => {
            text_size(container.text.as_str())
                + container
                    .streams
                    .iter()
                    .map(|(_, data)| data.len() as u64)
                    .sum::<u64>()
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_counts_code_points() {
        assert_eq!(text_size("abc"), 6);
        assert_eq!(text_size("\u{1F600}"), 2);
        assert_eq!(text_size("e\u{301}"), 4);
        assert_eq!(value_size(&Value::number(123_456)), DATA);
        assert_eq!(value_size(&Value::text("ab")), DATA + 4);
    }
}
