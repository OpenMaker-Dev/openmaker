//! Script source: the text form a script is fetched and edited in
//! (`om_scripts::display`, the Script Workspace's copy and paste), and
//! compiling edited text back into the script it came from.
//!
//! Each step is one record of the text, indented with a tab per block
//! level; `Stored …` options carry what the visible text leaves out, so the
//! text reads back to the same steps. Compiling lines the edited records
//! up with the script's own step texts (a longest common subsequence): a
//! record that matches one of the script's steps keeps that step exactly
//! as stored (its data key, data and item UUIDs); any other record is read
//! as a new step with the file's names. Text that this file's own steps
//! print as may carry their `Stored …` ids; other text may only name
//! objects ([`Origin::Unknown`]), so a step copied from another file never
//! binds to whatever has the same ids here.

use std::collections::HashSet;

use om_hbam::OwnedNode;
use om_scripts::display::{
    numbered_records, parse_record, paste_limits, script_lines, step_text_fidelity, Fidelity,
    Origin, StepNames, TEXT_INDENT,
};
use om_scripts::{Script, StepSource};
use serde::Serialize;

/// Past this many cells the middle of two texts is not aligned: its
/// records are all read as new steps.
const MAX_ALIGN_CELLS: usize = 4_000_000;

/// A problem with edited text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// The line the record starts on (from 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// The step the record is (from 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<usize>,
    /// The record's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub message: String,
}

impl Diagnostic {
    fn general(message: impl Into<String>) -> Self {
        Self {
            line: None,
            step: None,
            text: None,
            message: message.into(),
        }
    }
}

/// Edited text compiled against its script.
#[derive(Clone, Debug)]
pub struct Compiled {
    /// The script with the new steps.
    pub script: Script,
    /// The new definition: the stored one with only its step list and step
    /// data replaced.
    pub definition: OwnedNode,
    /// Steps kept as stored, new steps, and stored steps left out.
    pub kept: usize,
    pub new: usize,
    pub deleted: usize,
}

/// One step of a script as text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StepText {
    /// The step's place (from 1).
    pub index: usize,
    /// The line of the source it starts on (from 1).
    pub line: usize,
    /// Its block depth (the tabs before it).
    pub depth: usize,
    pub step_id: u16,
    pub name: String,
    pub enabled: bool,
    /// Its text, without indentation.
    pub text: String,
    /// `display`: the visible text is the whole step; `extras`: `Stored …`
    /// options carry the parts listed in `stored`; `raw`: the step is
    /// carried whole as `Stored step`, for the reasons listed.
    pub fidelity: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stored: Vec<String>,
}

/// A script's steps as text: the source and each step's place in it.
pub fn script_text(
    steps: &[om_scripts::ScriptStep],
    names: &dyn StepNames,
) -> (String, Vec<StepText>) {
    let mut source = String::new();
    let mut listing = Vec::new();
    let mut line = 1;
    for (i, (step, display)) in steps.iter().zip(script_lines(steps, names)).enumerate() {
        let (text, fidelity) = step_text_fidelity(step, names);
        source.push_str(&TEXT_INDENT.repeat(display.depth));
        source.push_str(&text);
        source.push('\n');
        let (fidelity, stored) = match fidelity {
            Fidelity::Display => ("display", Vec::new()),
            Fidelity::Extras(parts) => ("extras", parts),
            Fidelity::Raw(reasons) => ("raw", reasons),
        };
        let lines = text.split('\n').count();
        listing.push(StepText {
            index: i + 1,
            line,
            depth: display.depth,
            step_id: step.id(),
            name: step.name(),
            enabled: step.is_enabled(),
            text,
            fidelity,
            stored,
        });
        line += lines;
    }
    (source, listing)
}

/// Compile `source` against `base`, whose stored definition is
/// `definition`, with the file's `names`. Fails with every record that
/// does not read, or with why the steps do not nest.
pub fn compile(
    base: &Script,
    definition: &OwnedNode,
    names: &dyn StepNames,
    source: &str,
) -> Result<Compiled, Vec<Diagnostic>> {
    paste_limits(source, 0).map_err(|e| vec![Diagnostic::general(e)])?;
    if source.trim_start().starts_with('<') {
        return Err(vec![Diagnostic::general(
            "Step XML is not read here; edit the text form get_script returns.",
        )]);
    }
    let records = numbered_records(source);
    paste_limits("", records.len()).map_err(|e| vec![Diagnostic::general(e)])?;
    let ours: Vec<String> = base
        .steps
        .iter()
        .map(|s| step_text_fidelity(s, names).0)
        .collect();
    let ours_refs: Vec<&str> = ours.iter().map(String::as_str).collect();
    let known: HashSet<&str> = ours_refs.iter().copied().collect();
    // Accept the parser's spelling on retries too, retaining the stored
    // step/data keys after a normalized edit. Only normalize foreign text
    // after the normal file-reference checks have accepted it.
    let keys: Vec<String> = records
        .iter()
        .map(|(_, record)| {
            let text = record.trim_start_matches([' ', '\t']);
            if known.contains(text) {
                text.to_owned()
            } else {
                parse_record(record, names, Origin::Unknown)
                    .map(|step| step_text_fidelity(&step, names).0)
                    .unwrap_or_else(|_| text.to_owned())
            }
        })
        .collect();
    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let aligned = align(&ours_refs, &key_refs);
    let mut steps = Vec::new();
    let mut diagnostics = Vec::new();
    for (i, ((line, record), at)) in records.iter().zip(&aligned).enumerate() {
        if let Some(at) = at {
            steps.push(StepSource::Kept(*at));
            continue;
        }
        let origin = if known.contains(record.trim_start_matches([' ', '\t'])) {
            Origin::ThisFile
        } else {
            Origin::Unknown
        };
        match parse_record(record, names, origin) {
            Ok(step) => steps.push(StepSource::New(step)),
            Err(message) => diagnostics.push(Diagnostic {
                line: Some(*line),
                step: Some(i + 1),
                text: Some(record.clone()),
                message,
            }),
        }
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    let kept = aligned.iter().filter(|a| a.is_some()).count();
    let new = steps.len() - kept;
    let mut script = base.clone();
    if let Err(e) = script.replace_steps(steps) {
        // Block errors name the step: "Step 4 (Else) follows an Else."
        let step =
            e.0.strip_prefix("Step ")
                .and_then(|rest| rest.split(' ').next())
                .and_then(|n| n.parse::<usize>().ok());
        let record = step.and_then(|n| records.get(n.checked_sub(1)?));
        return Err(vec![Diagnostic {
            line: record.map(|(line, _)| *line),
            step,
            text: record.map(|(_, text)| text.clone()),
            message: e.0,
        }]);
    }
    let definition = script.with_steps_in(definition);
    Ok(Compiled {
        script,
        definition,
        kept,
        new,
        deleted: base.steps.len() - kept,
    })
}

/// For each of `b`, the index in `a` it is aligned with, on a longest
/// common subsequence: common ends first, then the middle unless it is too
/// large to compare.
fn align(a: &[&str], b: &[&str]) -> Vec<Option<usize>> {
    let mut out = vec![None; b.len()];
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    for (i, slot) in out.iter_mut().enumerate().take(prefix) {
        *slot = Some(i);
    }
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    for k in 0..suffix {
        out[b.len() - 1 - k] = Some(a.len() - 1 - k);
    }
    let a_mid = &a[prefix..a.len() - suffix];
    let b_mid = &b[prefix..b.len() - suffix];
    let (n, m) = (a_mid.len(), b_mid.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > MAX_ALIGN_CELLS {
        return out;
    }
    // lengths[i][j]: the longest common subsequence of a_mid[i..], b_mid[j..].
    let width = m + 1;
    let mut lengths = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lengths[i * width + j] = if a_mid[i] == b_mid[j] {
                lengths[(i + 1) * width + j + 1] + 1
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a_mid[i] == b_mid[j] {
            out[prefix + j] = Some(prefix + i);
            i += 1;
            j += 1;
        } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// One line of a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Same(usize, usize),
    Removed(usize),
    Added(usize),
}

/// A unified diff (3 lines of context) of `old` to `new`, empty when they
/// are the same.
pub fn unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut ops = Vec::new();
    let mut i = 0;
    let mut added = Vec::new();
    for (j, at) in align(&a, &b).into_iter().enumerate() {
        match at {
            Some(at) => {
                ops.extend((i..at).map(Op::Removed));
                ops.append(&mut added);
                ops.push(Op::Same(at, j));
                i = at + 1;
            }
            None => added.push(Op::Added(j)),
        }
    }
    ops.extend((i..a.len()).map(Op::Removed));
    ops.append(&mut added);
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, Op::Same(..)))
        .map(|(k, _)| k)
        .collect();
    if changed.is_empty() {
        return String::new();
    }
    const CONTEXT: usize = 3;
    let mut out = format!("--- {old_label}\n+++ {new_label}\n");
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(CONTEXT);
        let mut end = changed[k];
        while k + 1 < changed.len() && changed[k + 1] <= end + 2 * CONTEXT + 1 {
            k += 1;
            end = changed[k];
        }
        let end = (end + CONTEXT + 1).min(ops.len());
        k += 1;
        // Lines before the hunk, on each side.
        let before = |side: fn(&Op) -> bool| ops[..start].iter().filter(|op| side(op)).count();
        let old_side = |op: &Op| !matches!(op, Op::Added(_));
        let new_side = |op: &Op| !matches!(op, Op::Removed(_));
        let hunk = &ops[start..end];
        let old_len = hunk.iter().filter(|op| old_side(op)).count();
        let new_len = hunk.iter().filter(|op| new_side(op)).count();
        let first = |before: usize, len: usize| if len == 0 { before } else { before + 1 };
        out.push_str(&format!(
            "@@ -{},{old_len} +{},{new_len} @@\n",
            first(before(old_side), old_len),
            first(before(new_side), new_len)
        ));
        for op in hunk {
            let (mark, text) = match *op {
                Op::Same(i, _) => (' ', a[i]),
                Op::Removed(i) => ('-', a[i]),
                Op::Added(j) => ('+', b[j]),
            };
            out.push(mark);
            out.push_str(text);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_keeps_common_records_in_order() {
        let a = ["If", "x", "End If", "y"];
        let b = ["If", "z", "x", "End If"];
        assert_eq!(align(&a, &b), [Some(0), None, Some(1), Some(2)]);
        // Repeated lines align once each, in order.
        let a = ["#", "x", "#"];
        let b = ["#", "#"];
        assert_eq!(align(&a, &b), [Some(0), Some(2)]);
        let a = ["a", "#", "b", "#", "c"];
        let b = ["#", "c", "#"];
        assert_eq!(align(&a, &b), [Some(1), Some(4), None]);
        assert_eq!(align(&[], &["a"]), [None]);
    }

    #[test]
    fn diffs_show_removed_lines_before_added_ones() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nc\nd\ne\n";
        assert_eq!(
            unified_diff(old, new, "old", "new"),
            "--- old\n+++ new\n@@ -1,4 +1,5 @@\n a\n-b\n+B\n c\n d\n+e\n"
        );
        assert_eq!(unified_diff(old, old, "old", "new"), "");
        let far: String = (0..20).map(|n| format!("{n}\n")).collect();
        let changed: String = (0..20)
            .map(|n| match n {
                2 => "two\n".to_owned(),
                17 => "seventeen\n".to_owned(),
                n => format!("{n}\n"),
            })
            .collect();
        let diff = unified_diff(&far, &changed, "old", "new");
        assert_eq!(diff.matches("@@ -").count(), 2, "{diff}");
        assert!(diff.contains("@@ -1,6 +1,6 @@\n"), "{diff}");
        assert!(diff.contains("@@ -15,6 +15,6 @@\n"), "{diff}");
    }
}
