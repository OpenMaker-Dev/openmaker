//! Print (43): accept choices, pass owning-window commit/veto, then spool the
//! same validated vector report as PDF export. Paper overrides are job-local.
use std::io::Write;
use std::sync::atomic::Ordering;

use super::{Act, WorkerHost};
use crate::{
    page_setup::Settings,
    printing,
    report::ReportError,
    scripting::{Answer, Prompt},
};
use om_data::{
    flag,
    runtime::{ErrorCode, HostError, HostResult, Mode, StepCall},
    ScriptStep,
};
use om_layout::paper::PaperSettings;

#[derive(Clone, Debug, PartialEq)]
struct Owner {
    file: u64,
    window: u32,
    layout: Option<u32>,
    record: Option<i64>,
    index: u64,
    mode: Mode,
}
#[derive(Clone, Debug)]
pub(crate) struct Request {
    options: printing::Options,
    paper: Option<PaperSettings>,
    owner: Owner,
}

fn stored(step: &ScriptStep, key: u8) -> Result<std::borrow::Cow<'_, [u8]>, ReportError> {
    let node = step
        .data_leaf(key)
        .ok_or_else(|| ReportError::unsupported("Restored Print has no print settings"))?;
    printing::stored_record(node)
}
impl WorkerHost<'_> {
    fn print_owner(&self) -> Owner {
        let state = &self.s.file.window.state;
        Owner {
            file: self.s.file.id,
            window: state.id,
            layout: state.layout,
            record: state.record,
            index: state.index,
            mode: state.mode,
        }
    }
    pub(super) fn print_records(&mut self, call: &StepCall<'_>) -> HostResult {
        self.s.failure = None;
        if !matches!(self.s.file.window.state.mode, Mode::Browse | Mode::Preview) {
            return Err(self.report_error(ReportError::unsupported(
                "Print is available in Browse and Preview modes",
            )));
        }
        if !matches!(call.step.value, 0..=2) {
            return Err(self.report_error(ReportError::unsupported(
                "Blank-record print variants are unsupported",
            )));
        }
        // The stored records apply only with "Specify print options": with
        // them off, FileMaker Pro 26 opens its Print dialog on the records
        // being browsed though the step keeps Current record (#843; that it
        // also prints them is inferred from the dialog).
        let current_record = call.step.has_flag(flag::RESTORE) && call.step.value == 2;
        let (mut options, paper) = if call.step.has_flag(flag::RESTORE) {
            let bytes = stored(call.step, 0x88).map_err(|e| self.report_error(e))?;
            let options = printing::Options::restored(&bytes, current_record)
                .map_err(|e| self.report_error(e))?;
            let paper = if call.step.data_leaf(0x87).is_some() {
                let bytes = stored(call.step, 0x87).map_err(|e| self.report_error(e))?;
                printing::common_chunk(&bytes).map_err(|e| self.report_error(e))?;
                Some(
                    Settings::restored(&bytes)
                        .map_err(|e| self.report_error(ReportError::unsupported(e)))?
                        .paper,
                )
            } else {
                None
            };
            (options, paper)
        } else {
            (
                printing::Options {
                    current_record,
                    ..Default::default()
                },
                None,
            )
        };
        let owner = self.print_owner();
        let printers = self
            .s
            .print_dispatcher
            .printers()
            .map_err(|e| self.report_error(e))?;
        if !call.step.has_flag(flag::NO_INTERACT) {
            let Some(Answer::Print(Some(accepted))) = self.ask(Prompt::Print(
                printing::Dialog::new(options, printers.clone()),
            )) else {
                return Err(ErrorCode::CANCELED.into());
            };
            options = accepted;
        }
        options.validate().map_err(|e| self.report_error(e))?;
        if options
            .printer
            .as_ref()
            .is_some_and(|p| !printers.contains(p))
        {
            return Err(self.report_error(ReportError {
                code: 600,
                message: "The selected printer is unavailable; no other printer was used".into(),
            }));
        }
        self.run_act(
            Act::Print(Request {
                options,
                paper,
                owner,
            }),
            0,
        )
    }

    pub(super) fn dispatch_print(&mut self, request: Request) -> HostResult {
        if self.print_owner() != request.owner {
            return Err(self.report_error(ReportError {
                code: 20,
                message: "The print window or record context changed before submission".into(),
            }));
        }
        if self.s.abort.load(Ordering::SeqCst) {
            return Err(ErrorCode::CANCELED.into());
        }
        self.commit_window()?;
        let report = self
            .build_report(
                request.options.current_record,
                request.paper.as_ref(),
                crate::report::PdfView::default(),
            )
            .map_err(|e| {
                if matches!(e, HostError::Error(ErrorCode(401))) {
                    HostError::Error(ErrorCode(203))
                } else {
                    e
                }
            })?;
        // Selection is applied only after complete pagination and calculation.
        // Never slice records or renumber page-dependent report expressions.
        request
            .options
            .pages
            .validate_for_pages(report.plan.pages.len())
            .map_err(|e| self.report_error(e))?;
        // The entire report exists before any temporary file or printer job.
        let mut temporary = tempfile::Builder::new()
            .prefix("openmaker-print-")
            .suffix(".pdf")
            .tempfile()
            .map_err(|e| {
                self.report_error(ReportError {
                    code: 801,
                    message: format!("Could not create print report: {e}"),
                })
            })?;
        temporary
            .write_all(&report.bytes)
            .and_then(|()| temporary.flush())
            .map_err(|e| {
                self.report_error(ReportError {
                    code: 801,
                    message: format!("Could not write print report: {e}"),
                })
            })?;
        if self.s.abort.load(Ordering::SeqCst) {
            return Err(ErrorCode::CANCELED.into());
        }
        let title = format!("OpenMaker - {}", self.s.file.context.file);
        self.s
            .print_dispatcher
            .submit(
                temporary.path(),
                &request.options,
                &report.plan.paper,
                &title,
            )
            .map_err(|e| self.report_error(e))
        // NamedTempFile remains live through synchronous submission and drops
        // on both success and failure; the spooler owns its own copy afterward.
    }
}
