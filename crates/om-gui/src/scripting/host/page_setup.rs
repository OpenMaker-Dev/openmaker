//! Print Setup (42); preferences change atomically, without a record commit.
use super::WorkerHost;
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
        self.s.failure = Some(message);
        HostError::Error(ErrorCode(3))
    }

    pub(super) fn print_setup(&mut self, call: &StepCall<'_>) -> HostResult {
        self.s.failure = None;
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
            let actual = node.value.as_ref().map_or_else(
                || {
                    node.children
                        .iter()
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
        if !call.step.has_flag(flag::NO_INTERACT) {
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
        let paper_changed = self
            .page_setup()
            .is_none_or(|current| current.paper != candidate.paper);
        let paper = candidate.paper.clone();
        self.s.file.page_setup = Some(candidate);
        if paper_changed {
            // Physical page positions are independent per window; a new paper
            // plan starts each at page one without changing Browse state.
            self.s.file.window.state.preview_page = 0;
            for window in &mut self.s.file.others {
                window.state.preview_page = 0;
            }
            self.send(ScriptEvent::PaperSettings {
                file: self.s.file.id,
                paper,
            });
        }
        Ok(())
    }
}
