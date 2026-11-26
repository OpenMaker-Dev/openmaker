//! Print Setup (42); preferences change atomically, without a record commit.
use super::{forget_failure, note_failure, WorkerHost};
use crate::{
    page_setup::Settings,
    scripting::{Answer, Prompt, ScriptEvent},
};
use om_data::{
    flag,
    runtime::{ErrorCode, HostError, HostResult, StepCall},
};
use om_layout::{pagination::PageSize, paper::PaperSettings, Layout};

impl WorkerHost<'_> {
    pub(super) fn report_paper(&self, layout: &Layout) -> Result<PageSize, String> {
        self.page_setup().as_ref().map_or_else(
            || PageSize::for_layout(layout),
            |settings| settings.paper.for_layout(layout),
        )
    }

    /// The file session's page setup: the one accepted in it, else the one
    /// the file stores (`[2, 87]`) when its common format reads; `None`
    /// before either, for the layout's own defaults.
    fn page_setup(&self) -> Option<Settings> {
        self.s.file.page_setup.clone().or_else(|| {
            let bytes = self.schema()?.page_format.as_deref()?;
            Settings::restored(bytes).ok()
        })
    }

    fn page_setup_error(&mut self, message: String) -> HostError {
        note_failure(message);
        HostError::Error(ErrorCode(3))
    }

    pub(super) fn print_setup(&mut self, call: &StepCall<'_>) -> HostResult {
        forget_failure();
        let restored = if call.step.has_flag(flag::RESTORE) {
            let node = call.step.data_leaf(0x87).ok_or_else(|| {
                self.page_setup_error("Restored Page Setup has no page format".into())
            })?;
            // Check declared and actual sizes before the common value decoder
            // allocates its reassembled buffer.
            let declared = node
                .children
                .first()
                .and_then(|n| n.value.as_deref())
                .and_then(|v| v.get(..4))
                .map(|v| u32::from_be_bytes(v.try_into().unwrap()) as usize);
            // The pieces after the chunk header (child 0) are the value.
            let actual = node.value.as_ref().map_or_else(
                || {
                    node.children
                        .iter()
                        .skip(1)
                        .filter_map(|n| n.value.as_ref())
                        .map(Vec::len)
                        .sum()
                },
                Vec::len,
            );
            if actual > 1024 * 1024 || declared.is_some_and(|n| n > 1024 * 1024) {
                return Err(self.page_setup_error("Page format exceeds 1 MiB".into()));
            }
            let bytes = om_data::scripts::node_bytes(node)
                .ok_or_else(|| self.page_setup_error("Malformed page format storage".into()))?;
            Some(Settings::restored(&bytes).map_err(|e| self.page_setup_error(e))?)
        } else {
            None
        };
        // Restore off + dialog off is a no-op, even with stale stored options.
        if restored.is_none() && call.step.has_flag(flag::NO_INTERACT) {
            return Ok(());
        }
        let mut candidate = match restored.or_else(|| self.page_setup()) {
            Some(settings) => settings,
            None => {
                let id = self
                    .s
                    .file
                    .window
                    .state
                    .layout
                    .ok_or(ErrorCode::RECORD_MISSING)?;
                let document = self.render(id).ok_or(HostError::Unsupported)?;
                Settings::new(
                    PaperSettings::initial(&document.layout)
                        .map_err(|e| self.page_setup_error(e))?,
                )
            }
        };
        let dialog = !call.step.has_flag(flag::NO_INTERACT);
        if dialog {
            let Some(Answer::PageSetup(Some(settings))) = self.ask(Prompt::PageSetup(candidate))
            else {
                return Err(ErrorCode::CANCELED.into());
            };
            candidate = settings;
        }
        candidate
            .paper
            .validate()
            .map_err(|e| self.page_setup_error(e))?;
        // Against the paper the file session prints on now, the stored
        // setup's before any is accepted: only a change of it starts the
        // pages over.
        let paper_changed = self
            .page_setup()
            .is_none_or(|current| current.paper != candidate.paper);
        // The windows are told of the first setup accepted even when it is
        // the stored one, so none is left on another paper; a window
        // already on it keeps its page (`Session::set_paper`).
        let first = self.s.file.page_setup.is_none();
        let paper = candidate.paper.clone();
        self.s.file.page_setup = Some(candidate);
        // File > Page Setup and the toolbar's button run this step with
        // the dialog and without Restore. Its OK is the one FileMaker Pro
        // was seen to save in the file; a restored setup is the
        // session's, as whether Pro saves one is unmeasured.
        if dialog && !call.step.has_flag(flag::RESTORE) {
            self.save_page_setup(&paper);
        }
        if paper_changed {
            // Physical page positions are independent per window; a new paper
            // plan starts each at page one without changing Browse state.
            self.s.file.window.state.preview_page = 0;
            for window in &mut self.s.file.others {
                window.state.preview_page = 0;
            }
        }
        if paper_changed || first {
            self.send(ScriptEvent::PaperSettings {
                file: self.s.file.id,
                paper,
            });
        }
        Ok(())
    }

    /// Keep `paper`, accepted in Page Setup's dialog, in a file on this
    /// computer, as FileMaker Pro does at the OK (file-format.md 3.8): as
    /// one `Comm` chunk, which Pro reads whole, adding its own records at
    /// its next OK. Pro's platform records go, since its sheet would show
    /// them in place of this paper. Nothing is written when the file's
    /// setup already reads as `paper`, which keeps Pro's records, or when
    /// it is one OpenMaker cannot read, which is kept as it is; the
    /// provider also keeps a setup that is not what was read here, or that
    /// holds records Pro does not make again. For a hosted file the
    /// provider sends nothing, as FileMaker Pro does, and the setup is the
    /// session's. After a failure the setup is the session's
    /// too, and the failure is logged.
    fn save_page_setup(&mut self, paper: &PaperSettings) {
        // What the file stores: what this session saved, else what it
        // stored when it opened. Without its schema that is not known, and
        // nothing is written over it.
        let stored = match (&self.s.file.page_format_saved, self.schema()) {
            (Some(saved), _) => Some(saved.clone()),
            (None, Some(schema)) => schema.page_format.clone(),
            (None, None) => {
                log::warn!("The page setup was kept for this session: the file's is not known.");
                return;
            }
        };
        let value = match plan_save(stored.as_deref(), paper) {
            Save::Unchanged => return,
            Save::Keep(reason) => {
                log::warn!("The file's page setup was kept: OpenMaker cannot read it ({reason}).");
                return;
            }
            Save::Write(value) => value,
        };
        match self.p().store_page_format(stored.as_deref(), &value) {
            Ok(true) => self.s.file.page_format_saved = Some(value.to_vec()),
            Ok(false) => {}
            Err(error) => log::warn!(
                "The page setup was kept for this session, not saved in the file: {}",
                error.message
            ),
        }
    }
}

/// What saving `paper` over `stored`, what the file stores, comes to.
enum Save {
    /// It would change nothing, and Pro's records stay.
    Unchanged,
    /// The file's setup does not read, and is kept: why.
    Keep(String),
    /// The `Comm` chunk to write.
    Write([u8; 37]),
}

/// A save changes nothing when the file already stores every byte `paper`
/// encodes to (a setup this session saved), or when `paper` is the file's
/// own setup, read and not edited, equal to it. Otherwise any byte the
/// encoder writes may differ, even the printable size alone, which an
/// edit below the sheet's f32 step changes, and it is written. A setup
/// read back from its f32s is not compared with a new encoding of it: that
/// is what tells a small edit apart. FileMaker Pro's A4 (595 x 842) is not
/// OpenMaker's (595.28 x 841.89).
fn plan_save(stored: Option<&[u8]>, paper: &PaperSettings) -> Save {
    let value = paper.common_format();
    if stored == Some(&value[..]) {
        return Save::Unchanged;
    }
    if let Some(bytes) = stored {
        match Settings::restored(bytes) {
            Ok(settings) if settings.paper == *paper => return Save::Unchanged,
            Ok(_) => {}
            Err(reason) => return Save::Keep(reason),
        }
    }
    Save::Write(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_layout::paper::Orientation;

    /// Every byte a save writes tells two setups apart, and a setup that
    /// would change nothing is not written again.
    #[test]
    fn a_page_setup_is_written_when_any_byte_it_encodes_to_differs() {
        let written = |stored: Option<&[u8]>, paper: &PaperSettings| match plan_save(stored, paper)
        {
            Save::Write(value) => Some(value),
            Save::Unchanged => None,
            Save::Keep(reason) => panic!("kept: {reason}"),
        };
        let letter = PaperSettings::default();
        let saved = letter.common_format();
        // US Legal differs from US Letter only in the sheet's bottom edge.
        let legal = PaperSettings {
            height: 1008.0,
            ..letter.clone()
        };
        assert_eq!(written(Some(&saved), &legal), Some(legal.common_format()));
        // A width edit too small to move the sheet's right edge in f32
        // (512 either way) still moves the printable width (494 against
        // 494.0000305), and is written.
        let narrow = PaperSettings {
            width: 530.0,
            ..letter.clone()
        };
        let edited = PaperSettings {
            width: 530.00002,
            ..letter.clone()
        };
        let (before, after) = (narrow.common_format(), edited.common_format());
        assert_eq!(before[..29], after[..29]);
        assert_ne!(before, after);
        assert_eq!(written(Some(&before), &edited), Some(after));
        // Unchanged: a setup this session saved, accepted again; and the
        // file's own setup read and not edited, FileMaker Pro's A4 (#876's
        // run L), whose 41-point bottom margin OpenMaker would not encode.
        let a4 = PaperSettings {
            width: 595.2756,
            height: 841.8898,
            orientation: Orientation::Landscape,
            scale: 0.5,
        };
        assert_eq!(written(Some(&a4.common_format()), &a4), None);
        let pro: &[u8] = b"Comm\x00\x00\x00\x1d\x00\x3f\x80\x00\x00\xc1\x90\x00\x00\xc1\x90\x00\x00\x44\x10\x40\x00\x44\x4e\x00\x00\x44\x0b\xc0\x00\x44\x43\xc0\x00";
        let read = Settings::restored(pro).unwrap().paper;
        assert_ne!(read.common_format()[..], *pro);
        assert_eq!(written(Some(pro), &read), None);
        // OpenMaker's A4 is not Pro's.
        let pro_a4 = PaperSettings {
            width: 595.0,
            height: 842.0,
            ..a4.clone()
        };
        assert!(written(Some(&pro_a4.common_format()), &a4).is_some());
    }
}
