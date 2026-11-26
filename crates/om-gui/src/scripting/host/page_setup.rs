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
    /// it is one OpenMaker cannot read, which is kept as it is. A hosted
    /// file keeps the setup for the session, as in FileMaker Pro: the
    /// provider sends nothing. After a failure the setup is the session's
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
        if let Some(bytes) = &stored {
            match Settings::restored(bytes) {
                Ok(settings) if same_paper(&settings.paper, paper) => return,
                Ok(_) => {}
                Err(reason) => {
                    log::warn!(
                        "The file's page setup was kept: OpenMaker cannot read it ({reason})."
                    );
                    return;
                }
            }
        }
        let value = paper.common_format();
        match self.p().store_page_format(&value) {
            Ok(true) => self.s.file.page_format_saved = Some(value.to_vec()),
            Ok(false) => {}
            Err(error) => log::warn!(
                "The page setup was kept for this session, not saved in the file: {}",
                error.message
            ),
        }
    }
}

/// Whether a stored setup, read back from its f32s, is `paper`: the same
/// orientation and scale (as f32), and the sheet to a hundredth of a point.
/// FileMaker Pro's A4 (595 x 842) is not OpenMaker's (595.28 x 841.89).
fn same_paper(stored: &PaperSettings, paper: &PaperSettings) -> bool {
    stored.orientation == paper.orientation
        && stored.scale as f32 == paper.scale as f32
        && (stored.width - paper.width).abs() < 0.01
        && (stored.height - paper.height).abs() < 0.01
}
