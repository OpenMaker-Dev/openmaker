//! Save Records as PDF (144): generic runtime call, portable report backend.

use om_data::calc::eval::{evaluate, CalcHost, Context};
use om_data::calc::{Ctx, Value, ValueType};
use om_data::formula::{Expr, Field};
use om_data::runtime::{Arg, ErrorCode, FieldRef, HostError, HostResult, Mode, StepCall};
use om_data::scripts::save_records::{self, Records};
use om_data::{flag, ScriptStep};
use om_layout::{LayoutObject, ObjectKind};

use super::WorkerHost;
use crate::report::{self, ReportError};
use crate::scripting::{files, Answer, PdfDialog, Prompt};

/// The restored options' view (slot 8), after refusing requested options
/// whose semantics this slice cannot implement. Restore-off option slots
/// are stale native storage and are not requests.
fn pdf_options(step: &ScriptStep) -> Result<report::PdfView, ReportError> {
    if step.has_flag(flag::OPTION) {
        return Err(ReportError::unsupported(
            "Appending to an existing PDF is unsupported",
        ));
    }
    if step.has_flag(flag::CREATE_EMAIL) {
        return Err(ReportError::unsupported(
            "Creating an email with a PDF attachment is unsupported",
        ));
    }
    if !matches!(step.list_value(11), None | Some(1)) || step.has_flag(flag::FIELD_SPEC) {
        return Err(ReportError::unsupported(
            "PDF output to a target or currently open PDF is unsupported",
        ));
    }
    if !matches!(step.value, 0..=2) {
        return Err(ReportError::unsupported(
            "Blank-record PDF output is unsupported",
        ));
    }
    if !step.has_flag(flag::RESTORE) {
        return Ok(report::PdfView::default());
    }
    if step.data_leaf(0x83).is_some() {
        return Err(ReportError::unsupported(
            "Legacy PDF option blobs are unsupported",
        ));
    }
    // The page-from calculation kept beside the print permission while All
    // pages is chosen: the runtime does not evaluate it, and it is accepted
    // when it is stored as a calculation.
    let unused_page_from = |slot: u16| save_records::unused_pdf_calculation(step, slot);
    if let Some(data) = &step.data {
        for (slot, value) in data.slots().filter(|(slot, _)| *slot <= 10) {
            let unexpected = value.child_keys().any(|key| {
                key != 3 && !(slot == 0 && key == 0x82) && !(key == 5 && unused_page_from(slot))
            });
            let malformed = unused_page_from(slot)
                && value
                    .child(5)
                    .is_some_and(|node| !well_formed_calculation(node));
            if unexpected
                || malformed
                || (value.child(3).is_some() && step.number(slot).is_none())
                || value.value().is_some()
                || !value.other().is_empty()
            {
                return Err(ReportError::unsupported(
                    "Unsupported or malformed restored PDF option storage",
                ));
            }
        }
    }
    for slot in 0..=10 {
        if step.calculation(slot).is_some() && !unused_page_from(slot) {
            return Err(ReportError::unsupported(
                "PDF metadata, passwords and page-range calculations are unsupported",
            ));
        }
        if let Some(n) = step.number(slot) {
            let supported = match slot {
                0 => n == 2,
                1 => n == 1,
                2 | 3 => n == 0,
                4 => n == 2,
                5 => n == 4,
                6 | 7 => n == 1,
                // The initial view (script-steps.md 12.5): 0 Page Only,
                // 1 Bookmarks Panel and Page, 2 Pages Panel and Page.
                8 => (0..=2).contains(&n),
                // Page layout and magnification: 0 Default only.
                9 | 10 => n == 0,
                _ => false,
            };
            if !supported {
                return Err(ReportError::unsupported(format!("Unsupported PDF option in slot {slot}: security, page ranges, and page layouts and magnifications other than Default are not implemented")));
            }
        }
    }
    Ok(match step.number(8) {
        Some(1) => report::PdfView::Bookmarks,
        Some(2) => report::PdfView::Pages,
        _ => report::PdfView::PageOnly,
    })
}

/// A stored calculation as FileMaker writes one: only its context,
/// formatted and Polish streams, with a Polish stream that decodes. It is
/// not evaluated.
fn well_formed_calculation(node: &om_data::runtime::OwnedNode) -> bool {
    node.children
        .iter()
        .all(|child| matches!(child.segment.as_slice(), [4] | [5] | [6]))
        && om_data::Calculation::decode(node)
            .and_then(|calculation| calculation.polish)
            .is_some_and(|polish| om_data::formula::polish::decode(&polish).is_ok())
}

impl WorkerHost<'_> {
    pub(super) fn report_error(&mut self, error: ReportError) -> HostError {
        self.s.failure = Some(error.message);
        HostError::Error(ErrorCode(error.code))
    }

    pub(super) fn save_records_pdf(&mut self, call: &StepCall<'_>) -> HostResult {
        self.s.failure = None;
        let view = pdf_options(call.step).map_err(|e| self.report_error(e))?;
        if self.s.file.window.state.mode == Mode::Find {
            return Err(self.report_error(ReportError::unsupported(
                "Save Records as PDF is unavailable in Find mode",
            )));
        }
        let mut paths = match call.arg("File paths") {
            Some(Arg::Text(text)) => text.lines().map(str::to_owned).collect(),
            _ => Vec::new(),
        };
        let mut current = Records::of(call.step) == Records::Current;
        if !call.step.has_flag(flag::NO_INTERACT) || paths.is_empty() {
            // The panel opens on the step's own output file, or
            // Untitled.pdf in the Documents folder.
            let documents = self.s.file.data_files.folders.documents.clone();
            let stored = files::candidates(&paths, &documents)
                .ok()
                .and_then(|found| found.into_iter().next())
                .and_then(|path| Some((path.parent()?.to_owned(), path.file_name()?.to_owned())));
            let (folder, name) = stored.unwrap_or_else(|| (documents, "Untitled.pdf".into()));
            let dialog = PdfDialog {
                chooser: crate::open_file::SaveChooser::new(
                    folder,
                    name.to_string_lossy().into_owned(),
                    "pdf",
                ),
                path: String::new(),
                current_record: current,
            };
            let Some(Answer::Pdf(Some(dialog))) = self.ask(Prompt::Pdf(dialog)) else {
                return Err(ErrorCode::CANCELED.into());
            };
            // The native path chosen, in FileMaker's form (`/private/tmp`
            // on macOS is `/Macintosh HD/private/tmp`, script-runtime.md
            // Files), as Export Records' dialog passes it.
            paths = vec![files::chosen_path(&dialog.path)];
            current = dialog.current_record;
        }
        // No output is opened before the report has been validated.
        self.commit_window()?;
        let report = self.build_report(current, None, view)?;
        let path = self.export_path(&paths, call.step.has_flag(flag::OPTION2))?;
        report::save_pdf(&path, &report.bytes).map_err(|e| {
            self.report_error(ReportError {
                code: 800,
                message: format!("The PDF could not be saved: {e}"),
            })
        })?;
        if call.step.has_flag(flag::OPEN_AUTOMATICALLY) {
            super::transfer::open_with_system(&path);
        }
        Ok(())
    }

    /// One owned vector report builder for PDF export and desktop Print.
    pub(super) fn build_report(
        &mut self,
        current: bool,
        override_paper: Option<&om_layout::paper::PaperSettings>,
        view: report::PdfView,
    ) -> HostResult<report::PdfReport> {
        let layout_id = self
            .s
            .file
            .window
            .state
            .layout
            .ok_or(ErrorCode::RECORD_MISSING)?;
        let document = self.render(layout_id).ok_or(HostError::Unsupported)?;
        let occurrence = self.occurrence().ok_or(ErrorCode::RECORD_MISSING)?;
        let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
        // A layout whose records the set gives no access is not printed or
        // saved as PDF (3) [I: unmeasured in FileMaker Pro, #707].
        let masked = schema
            .authenticated_account
            .set_access
            .as_ref()
            .is_some_and(|access| {
                access.layout_records(u64::from(layout_id))
                    == om_data::host::privilege_set::AccessLevel::No
            });
        if masked {
            return Err(HostError::Unsupported);
        }
        let (found, selected) = {
            let (window, provider) = self.wp();
            let count = window.found_count(provider).map_err(super::data_error)?;
            let found = window.found_ids(provider).map_err(super::data_error)?;
            if found.len() as u64 != count || found.iter().any(|id| *id <= 0) {
                return Err(self.report_error(ReportError::unsupported(
                    "The provider returned an incomplete report found set",
                )));
            }
            let selected = if current {
                window
                    .record_id(provider)
                    .map_err(super::data_error)?
                    .into_iter()
                    .collect()
            } else {
                found.clone()
            };
            (found, selected)
        };
        if selected.is_empty() {
            return Err(ErrorCode(401).into());
        }
        let paper = override_paper
            .map_or_else(
                || self.report_paper(&document.layout),
                |paper| paper.for_layout(&document.layout),
            )
            .map_err(|e| self.report_error(ReportError::unsupported(e)))?;
        let selected_ids: Vec<u64> = selected.iter().map(|id| *id as u64).collect();
        let breaks = self
            .report_groups(&document.layout, occurrence, &schema, &selected_ids)
            .map_err(|e| self.report_error(e))?;
        // Claris: Get ( WindowMode ) is 3 while printing is in progress; a
        // PDF is printed output too (inferred). Layout calculations such as
        // a field's `Get ( WindowMode ) = 0` format read it.
        let mut info = self.session_info();
        info.window_mode = om_data::host::WindowMode::Printing;
        self.report_session = Some(info);
        let result = report::render_pdf_viewed(
            &document.layout,
            selected.len(),
            paper,
            &breaks,
            &mut HostContent {
                panels: (self.s.file.window.state.panels.iter())
                    .filter(|((layout, _), _)| *layout == u64::from(layout_id))
                    .map(|((_, control), panel)| (*control, *panel))
                    .collect(),
                host: self,
                found: &found,
                selected: &selected,
                occurrence,
                schema: &schema,
            },
            view,
        );
        self.report_record = None;
        self.report_session = None;
        result.map_err(|e| self.report_error(e))
    }

    /// One report object's text for `selected[context.record]` on its
    /// physical page, with Get ( RecordNumber ) at its found-set position:
    /// what output and Preview page movement read alike. The caller sets
    /// `report_session` around the report and clears it afterwards.
    pub(super) fn report_content(
        &mut self,
        found: &[i64],
        selected: &[i64],
        occurrence: u64,
        schema: &om_data::host::CalcSchema,
        object: &LayoutObject,
        context: report::ContentContext,
    ) -> Result<String, ReportError> {
        let record = self.report_at(found, selected, context);
        let (text, _) = self.pdf_text(object, occurrence, record, context, schema)?;
        Ok(text)
    }

    /// The conditional formats of `object` that hold for
    /// `selected[context.record]` on its physical page, as
    /// [`Self::report_content`] reads it. `Self` is a field object's value,
    /// otherwise the object's text, as in Browse.
    fn report_conditions(
        &mut self,
        found: &[i64],
        selected: &[i64],
        occurrence: u64,
        schema: &om_data::host::CalcSchema,
        object: &LayoutObject,
        context: report::ContentContext,
    ) -> Result<Vec<usize>, ReportError> {
        if object.calcs.conditions.is_empty() {
            return Ok(Vec::new());
        }
        let record = self.report_at(found, selected, context);
        let (text, own) = self.pdf_text(object, occurrence, record, context, schema)?;
        let context = report::in_row(Context::record(occurrence, record), context.portal)
            .with_self(own.unwrap_or_else(|| Value::text(text)));
        // `LayoutObjectUUID` is the object's, as in Browse.
        self.report_object = (object.uuid.as_ref()).map(om_data::host::layout_objects::uuid_text);
        let held = om_data::host::layout::conditions(self, object, &context);
        self.report_object = None;
        Ok(held)
    }

    /// The records of `portal`'s occurrence related to
    /// `selected[context.record]`, in the relationship's order.
    fn report_portal_records(
        &mut self,
        found: &[i64],
        selected: &[i64],
        occurrence: u64,
        portal: &LayoutObject,
        context: report::ContentContext,
    ) -> Result<Vec<u64>, ReportError> {
        let record = self.report_at(found, selected, context);
        let related = (portal.portal.as_ref())
            .and_then(|p| om_layout::occurrence_id(p.table_alias_key))
            .ok_or_else(|| ReportError::unsupported("A report portal's table is invalid"))?;
        CalcHost::related_records(self, occurrence, record, related).map_err(|e| ReportError {
            code: if e.code() == 0 { 3 } else { e.code() },
            message: "A report portal's related records could not be read".into(),
        })
    }

    /// Make `selected[context.record]` the report's record, at its found-set
    /// position, on its physical page; its id.
    fn report_at(
        &mut self,
        found: &[i64],
        selected: &[i64],
        context: report::ContentContext,
    ) -> u64 {
        let index = context.record;
        let record = selected[index] as u64;
        self.report_record = Some((
            record,
            found
                .iter()
                .position(|r| *r == selected[index])
                .unwrap_or(index),
            context.page,
        ));
        if let Some(info) = &mut self.report_session {
            info.report_page_number = Some(context.page as u64 + 1);
        }
        record
    }

    /// The sorted groups of the layout's sub-summary parts for `selected`
    /// (record ids in output order); empty, without reading any data, for
    /// a layout without them. Shared by output and Preview page counts.
    pub(super) fn report_groups(
        &mut self,
        layout: &om_layout::Layout,
        occurrence: u64,
        schema: &om_data::host::CalcSchema,
        selected: &[u64],
    ) -> Result<Vec<om_layout::pagination::BreakGroups>, ReportError> {
        let breaks = report::report_breaks(layout, schema, occurrence)?;
        if breaks.is_empty() {
            return Ok(Vec::new());
        }
        let count = {
            let (window, provider) = self.wp();
            window
                .found_count(provider)
                .map_err(|e| ReportError::unsupported(e.message))?
        };
        if count > report::MAX_GROUPED_RECORDS {
            return Err(ReportError::unsupported(format!(
                "Grouped reports support at most {} records",
                report::MAX_GROUPED_RECORDS
            )));
        }
        let fields: Vec<u64> = breaks.iter().map(|b| b.field).collect();
        let found = self
            .file(|f| f.found_groups(occurrence, &fields))
            .ok_or_else(|| {
                ReportError::unsupported("This provider has no report calculation context")
            })?
            .map_err(report::group_error)?;
        report::selected_groups(&breaks, &found, selected)
    }

    #[allow(clippy::too_many_arguments)]
    fn pdf_field(
        &mut self,
        origin: u64,
        record: u64,
        group: Option<om_layout::BreakField>,
        row: Option<report::PortalRowContext>,
        target: FieldRef,
        object: &LayoutObject,
        schema: &om_data::host::CalcSchema,
    ) -> Result<(Value, String), ReportError> {
        let FieldRef {
            occurrence,
            field,
            repetition,
        } = target;
        let def = report::field_def(schema, target, object)?;
        // A container field object prints its data ([`Self::report_container`]).
        if def.value_type == ValueType::Container {
            return Ok((Value::text(String::new()), String::new()));
        }
        let expr = Expr::Field(Field::new(occurrence, field, true));
        let context = report::in_row(
            Context {
                repetition,
                ..Context::record(origin, record)
            },
            row,
        );
        let incomplete = |code: i32| ReportError {
            code: if code == 0 { 3 } else { code },
            message: "A report field could not be calculated completely".into(),
        };
        let value = match report::group_summary(def, target, origin, group)? {
            Some((summary, by)) => CalcHost::summary(self, &context, summary, by)
                .map_err(|e| incomplete(e.code()))
                .and_then(|v| match v.is_invalid() {
                    true => Err(incomplete(3)),
                    false => Ok(v),
                })?,
            None => {
                let evaluated = evaluate(&expr, self, &context);
                if evaluated.pending_execution
                    || evaluated.error.is_some()
                    || evaluated.value.is_invalid()
                {
                    return Err(incomplete(evaluated.code()));
                }
                evaluated.value
            }
        };
        let text = self.text(&value);
        let cx = Ctx::new(self.locale_for(&context), self.today);
        let text = crate::document::data_format::display(
            &text,
            def.value_type,
            object.data_format.as_ref(),
            &cx,
        )
        .into_owned();
        Ok((value, text))
    }

    /// The data a container field object shows for `selected[context.record]`,
    /// its streams read from the library; `None` for other objects and an
    /// empty field.
    #[allow(clippy::too_many_arguments)]
    fn report_container(
        &mut self,
        found: &[i64],
        selected: &[i64],
        origin: u64,
        schema: &om_data::host::CalcSchema,
        object: &LayoutObject,
        context: report::ContentContext,
    ) -> Result<Option<std::sync::Arc<om_data::calc::eval::StoredContainer>>, ReportError> {
        let Some(spec) = object.field_spec() else {
            return Ok(None);
        };
        let (Some(occurrence), Some(field)) =
            (om_layout::occurrence_id(spec.table_key), spec.field_id())
        else {
            return Ok(None);
        };
        if schema
            .field(occurrence, field)
            .is_none_or(|def| def.value_type != ValueType::Container)
        {
            return Ok(None);
        }
        let record = selected[context.record] as u64;
        self.report_record = Some((
            record,
            found
                .iter()
                .position(|r| *r == selected[context.record])
                .unwrap_or(context.record),
            context.page,
        ));
        // In a portal row, the row's record.
        let calc_context = report::in_row(
            Context {
                repetition: spec.repetition.max(1) as u32,
                ..Context::record(origin, record)
            },
            context.portal,
        );
        let incomplete = |code: i32| ReportError {
            code: if code == 0 { 3 } else { code },
            message: "A report container could not be read".into(),
        };
        let evaluated = evaluate(
            &Expr::Field(Field::new(occurrence, field, true)),
            self,
            &calc_context,
        );
        if evaluated.pending_execution || evaluated.error.is_some() {
            return Err(incomplete(evaluated.code()));
        }
        let om_data::calc::Value::Container(data) = evaluated.value else {
            return Ok(None);
        };
        if data.streams.is_empty() && data.library.is_none() {
            return Ok(None);
        }
        if !data.is_unloaded() {
            return Ok(Some(std::sync::Arc::new(
                om_data::calc::eval::StoredContainer {
                    data,
                    intact: true,
                    external: false,
                },
            )));
        }
        let mut stored = CalcHost::container_data(self, &data, &calc_context)
            .map_err(|e| incomplete(e.code()))?;
        stored.data.library = data.library;
        Ok(Some(std::sync::Arc::new(stored)))
    }

    /// The object's text and, for a field object, its field's value, in
    /// the portal row `context` names, if any.
    fn pdf_text(
        &mut self,
        object: &LayoutObject,
        origin: u64,
        record: u64,
        context: report::ContentContext,
        schema: &om_data::host::CalcSchema,
    ) -> Result<(String, Option<Value>), ReportError> {
        let mut own = None;
        let (group, row) = (context.group, context.portal);
        let text = report::object_text(object, origin, schema, |field| {
            let (value, text) =
                self.pdf_field(origin, record, group, row, field, object, schema)?;
            if object.kind() == ObjectKind::Field {
                own = Some(value);
            }
            Ok(text)
        })?;
        Ok((text, own))
    }
}

/// Save Records as PDF's report content: the host's text, container data
/// and conditional formatting for the selected records.
pub(super) struct HostContent<'h, 'a, 'r> {
    pub host: &'h mut WorkerHost<'a>,
    /// The panel each tab control shows in the window, by control.
    pub panels: std::collections::BTreeMap<u64, u64>,
    pub found: &'r [i64],
    pub selected: &'r [i64],
    pub occurrence: u64,
    pub schema: &'r om_data::host::CalcSchema,
}

impl report::ReportContent for HostContent<'_, '_, '_> {
    fn text(
        &mut self,
        object: &LayoutObject,
        context: report::ContentContext,
    ) -> Result<String, ReportError> {
        self.host.report_content(
            self.found,
            self.selected,
            self.occurrence,
            self.schema,
            object,
            context,
        )
    }

    fn container(
        &mut self,
        object: &LayoutObject,
        context: report::ContentContext,
    ) -> Result<Option<std::sync::Arc<om_data::calc::eval::StoredContainer>>, ReportError> {
        self.host.report_container(
            self.found,
            self.selected,
            self.occurrence,
            self.schema,
            object,
            context,
        )
    }

    fn conditions(
        &mut self,
        object: &LayoutObject,
        context: report::ContentContext,
    ) -> Result<Option<Vec<usize>>, ReportError> {
        self.host
            .report_conditions(
                self.found,
                self.selected,
                self.occurrence,
                self.schema,
                object,
                context,
            )
            .map(Some)
    }

    fn front_panel(&mut self, control: &LayoutObject) -> Option<u64> {
        self.panels.get(&control.id).copied()
    }

    fn portal_records(
        &mut self,
        portal: &LayoutObject,
        context: report::ContentContext,
    ) -> Result<Option<Vec<u64>>, ReportError> {
        self.host
            .report_portal_records(self.found, self.selected, self.occurrence, portal, context)
            .map(Some)
    }
}
