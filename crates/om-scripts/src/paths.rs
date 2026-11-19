//! A step's file path list `[00 0, 82]` (`KPathString`, script-steps.md
//! 11.0): the output or source files of Export Records, Save Records as
//! PDF and Excel, Insert File, the data file steps and the others whose
//! "File paths" parameter it is.
//!
//! The list is a path stream (`UniversalPathListToStream`): a u32 count,
//! then three stream strings per path: its kind (`file`, `filemac`, ...),
//! a volume and the path. FileMaker's XML shows each path as one line
//! `kind:path`, and its XML importer stores such a line with an empty
//! volume (script-steps.md, "Implementation notes", the oracle runs); a
//! list written here is stored the same way. [`ScriptStep::path_list`]
//! reads either form, and paths with a volume as `kind:/volume path`.

use crate::step::{flag, ScriptStep, StepData};
use crate::value::bytes_node;

/// The longest path list text the Script Workspace writes, in bytes.
pub const MAX_PATH_LIST: usize = 30_000;

/// The longest string a path list holds here, in masked SCSU bytes: the
/// two-byte count's limit, the longest the runtime's reader takes.
const MAX_STRING: usize = 0x3fff;

/// A stream string (script-steps.md 11.0, `InternalSCSUCompress`): its
/// byte count, then the masked SCSU bytes; at most [`MAX_STRING`] bytes,
/// which [`parse`] checks.
fn push_string(out: &mut Vec<u8>, text: &str) {
    let bytes = om_scsu::encode_scsu(text);
    let n = bytes.len().min(MAX_STRING);
    if n < 0x80 {
        out.push(n as u8);
    } else {
        out.extend((0x8000 | n as u16).to_be_bytes());
    }
    out.extend(&bytes[..n]);
}

/// The paths of a path list text, one per line, as `(kind, path)`: each
/// line `kind:path`, the kind a word of ASCII letters, kept as typed, and
/// the path every character after its colon (spaces at either end
/// included, as the reader gives them back). Lines of only spaces are
/// skipped. A line without a kind is refused: how FileMaker stores one (a
/// lone `$variable` is allowed in its dialog) is not established. So is a
/// path longer than the stream's two-byte count holds.
pub fn parse(text: &str) -> Result<Vec<(String, String)>, String> {
    if text.len() > MAX_PATH_LIST {
        return Err(format!(
            "A path list holds at most {MAX_PATH_LIST} bytes here."
        ));
    }
    let mut paths = Vec::new();
    for line in text.split(['\r', '\n']) {
        if line.trim().is_empty() {
            continue;
        }
        let kind = line
            .split_once(':')
            .map(|(kind, _)| kind)
            .filter(|kind| !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_alphabetic()));
        let Some(kind) = kind else {
            return Err(format!(
                "“{line}” has no kind: start each path with one, such as file:, filemac:, \
                 filewin: or filelinux: (file:$path for a path in a variable)."
            ));
        };
        let path = &line[kind.len() + 1..];
        if path.is_empty() {
            return Err(format!("“{line}” names no file."));
        }
        if [kind, path]
            .iter()
            .any(|part| om_scsu::encode_scsu(part).len() > MAX_STRING)
        {
            return Err(format!(
                "A path holds at most {MAX_STRING} bytes here; “{kind}:…” is longer."
            ));
        }
        paths.push((kind.to_owned(), path.to_owned()));
    }
    Ok(paths)
}

/// The path stream of `paths` (`(kind, path)`, each with an empty volume).
pub fn encode(paths: &[(String, String)]) -> Vec<u8> {
    let mut out = (paths.len() as u32).to_be_bytes().to_vec();
    for (kind, path) in paths {
        push_string(&mut out, kind);
        push_string(&mut out, "");
        push_string(&mut out, path);
    }
    out
}

impl ScriptStep {
    /// The step's file path list as text, one `kind:path` per line
    /// (returns between them), `None` when it has none.
    pub fn path_list(&self) -> Option<String> {
        crate::runtime::path_list(self)
    }

    /// Store the path list `text` (one `kind:path` per line) at
    /// `[00 0, 82]` as FileMaker's XML importer stores one, with
    /// `kFilePath`; an empty list removes it and the flag. Refused, the
    /// step unchanged, for a line [`parse`] refuses.
    pub fn set_path_list(&mut self, text: &str) -> Result<(), String> {
        let paths = parse(text)?;
        if paths.is_empty() {
            self.set_slot_child(0, StepData::PATH_STRING, None);
            self.set_flag(flag::FILE_PATH, false);
        } else {
            let stream = encode(&paths);
            self.set_slot_child(
                0,
                StepData::PATH_STRING,
                Some(bytes_node(&[StepData::PATH_STRING], &stream)),
            );
            self.set_flag(flag::FILE_PATH, true);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_list_reads_back_as_written() {
        let mut step = ScriptStep::new(144);
        let text = "file:Invoices/$name.pdf\rfilemac:/Macintosh HD/Users/Größe.pdf";
        step.set_path_list(&format!("{text}\n\n")).unwrap();
        assert!(step.has_flag(flag::FILE_PATH));
        assert_eq!(step.path_list().as_deref(), Some(text));
        // Kind, empty volume, path: as FileMaker's XML importer stores one.
        let stored = step
            .data
            .as_ref()
            .and_then(|d| d.slot(0))
            .and_then(|s| s.child(StepData::PATH_STRING))
            .and_then(crate::node_bytes)
            .unwrap();
        assert_eq!(&stored[..4], &[0, 0, 0, 2]);
        assert_eq!(&stored[4..10], &[4, 0x3c, 0x33, 0x36, 0x3f, 0]);
        step.set_path_list("").unwrap();
        assert!(!step.has_flag(flag::FILE_PATH));
        assert_eq!(step.path_list(), None);
    }

    #[test]
    fn a_line_without_a_kind_is_refused_and_the_step_kept() {
        let mut step = ScriptStep::new(36);
        step.set_path_list("file:Export.csv").unwrap();
        let long = format!("file:{}", "x".repeat(MAX_STRING + 1));
        let long_kind = format!("{}:x", "a".repeat(MAX_STRING + 1));
        for bad in [
            "$path",
            "Export.csv",
            ":x",
            "file:",
            " file:x",
            &long,
            &long_kind,
        ] {
            assert!(step.set_path_list(bad).is_err(), "{bad}");
        }
        assert_eq!(step.path_list().as_deref(), Some("file:Export.csv"));
    }

    #[test]
    fn every_path_reads_back_exactly_spaces_and_kinds_included() {
        let mut step = ScriptStep::new(36);
        let longest = format!("file:{}", "y".repeat(MAX_STRING));
        let text = format!("filemac:/Users/a/report \rFILE:other.pdf\r{longest}");
        step.set_path_list(&text).unwrap();
        assert_eq!(step.path_list().as_deref(), Some(text.as_str()));
    }
}
