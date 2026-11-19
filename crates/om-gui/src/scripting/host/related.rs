//! Related IDs are read in the source's hydrated schema; layouts and found
//! sets belong to the selected destination file's independent session.
use super::{data_error, WorkerHost};
use om_data::calc::Ctx;
use om_data::file_refs::hosted_key;
use om_data::found::FoundSetChange;
use om_data::runtime::*;

use super::super::WindowStyle;

/// The schema with a related occurrence bound, and the account the step
/// typed for its file (its sign-in name), if it asked for one.
type Bound = (
    std::sync::Arc<om_data::host::CalcSchema>,
    Option<(String, crate::scripting::SignIn)>,
);

impl WorkerHost<'_> {
    /// The script's schema with occurrence `related` bound to its table:
    /// a related file not opened yet opens now (on demand, #983), and one
    /// that refused the account it was sent, or was sent none, asks for an
    /// account (the sign-in Open File uses, #991), as a layout of it asks
    /// in the window. The step then goes on in the schema that has the
    /// file, and an account typed opens the file's window too, with no
    /// second prompt. 106 when it has no table even so; Cancel at the
    /// prompt gives 1, or after a refused answer 212.
    fn related_schema(&mut self, related: u64) -> HostResult<Bound> {
        let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
        if schema.table_of(related).is_some() {
            return Ok((schema, None));
        }
        if schema.is_pending(related) {
            // Whether it opened or is refused, the schema below says.
            let _ = self.p().calc_data(&[om_data::host::CalcNeed::Occurrence {
                occurrence: related,
            }]);
            self.reread_schema();
            let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
            if schema.table_of(related).is_some() {
                return Ok((schema, None));
            }
        }
        let Some(file) = self
            .p()
            .related_sign_ins()
            .into_iter()
            .find(|sign_in| sign_in.occurrences.contains(&related))
            .map(|sign_in| sign_in.file)
        else {
            return Err(ErrorCode(106).into());
        };
        let unbound = om_data::DataError::with_code(
            106,
            format!("“{file}” has not admitted an account of this file's yet."),
        );
        let mut typed = None;
        let signed_in = self.ask_for_account(&file, unbound, |host, account, password| {
            host.p().sign_in_related(&file, account, password)?;
            typed = Some(crate::scripting::SignIn {
                account: account.to_owned(),
                password: zeroize::Zeroizing::new(password.to_owned()),
            });
            Ok(())
        })?;
        if let Err(error) = signed_in {
            return Err(self.open_failed(error));
        }
        self.reread_schema();
        let schema = self.schema().cloned().ok_or(HostError::Unsupported)?;
        if schema.table_of(related).is_none() {
            return Err(ErrorCode(106).into());
        }
        Ok((schema, typed.map(|sign_in| (file, sign_in))))
    }

    pub(super) fn go_to_related(&mut self, request: &RelatedRecords<'_>) -> HostResult {
        let related = request.occurrence.ok_or(ErrorCode::INVALID)?;
        let (schema, typed) = self.related_schema(related)?;
        let occurrence = self.s.file.occurrence.ok_or(HostError::Unsupported)?;
        let table = schema.table_of(related).ok_or(ErrorCode(106))?.id;
        let records = {
            let (window, provider) = self.wp();
            if request.match_found_set {
                window.found_ids(provider).map_err(data_error)?
            } else {
                window
                    .record_id(provider)
                    .map_err(data_error)?
                    .into_iter()
                    .collect()
            }
        };
        let row_record = if request.match_found_set {
            None
        } else {
            self.check_row_order(related)?;
            self.active_row_record(related)
        };
        let mut ids = Vec::new();
        for record in records {
            let Ok(record) = u64::try_from(record) else {
                continue;
            };
            let found = self
                .file(|f| f.related_records(occurrence, record, related))
                .ok_or(HostError::Unsupported)?
                .map_err(|_| HostError::Error(ErrorCode(103)))?;
            for id in found {
                let id = i64::try_from(id).map_err(|_| ErrorCode::RECORD_MISSING)?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        // From a filtering portal's row, the related records it shows
        // (#192).
        if request.show_only_related && !request.match_found_set {
            if let Some(shown) = self.filtered_rows(related)? {
                ids.retain(|id| u64::try_from(*id).is_ok_and(|id| shown.contains(&id)));
            }
        }
        if ids.is_empty() {
            return Err(ErrorCode::RECORD_MISSING.into());
        }
        let first = row_record
            .and_then(|r| i64::try_from(r).ok())
            .filter(|r| ids.contains(r))
            .unwrap_or(ids[0]);

        // Evaluate geometry in the source's locale, before switching files.
        let name = request.window[0].as_ref().map(|v| self.text(v));
        let bound = |slot: usize| {
            let number = request.window[slot]
                .as_ref()?
                .to_number(&Ctx::new(&schema.locale, self.today));
            if number.is_nan() || number.is_empty() {
                return None;
            }
            number.to_text_with('.').parse().ok()
        };
        let bounds = Bounds {
            height: bound(1),
            width: bound(2),
            top: bound(3),
            left: bound(4),
        };

        let from = self.s.file.id;
        let (destination, destination_table) = if request.external_layout {
            let source = schema.data_source(related).ok_or(ErrorCode(106))?;
            let owner = schema.table_context(table);
            // The imported context stores a stem, including a literal
            // ".fmp12" when the actual file has a doubled extension.
            let owner_name = owner.map_or_else(
                || self.s.file.context.file.clone(),
                |f| format!("{}.fmp12", f.file_name),
            );
            let native_table = owner.map_or(table, |f| f.file_id(table));
            // Opening the file queues its opening triggers (without a new
            // window): a file refused below runs none of them.
            let queued = self.triggers.len();
            let destination = self.resolve_related_source(
                source,
                request.new_window,
                typed
                    .as_ref()
                    .map(|(file, sign_in)| (file.as_str(), sign_in)),
            )?;
            // The ID list and the destination must be the same file, even
            // if an earlier path starts working after schema hydration, or
            // a later one stands in for one that fails now: one name on
            // two hosts, or on two ports of a host, is two files
            // (`FileIdentity`). An import or a file that does not say which
            // file it is is matched by name.
            // A table of this file has no imported context: this file is
            // its owner.
            let owner_identity = match owner {
                Some(f) => f.identity.clone(),
                None => self.files.identity(&self.s.file, from),
            };
            let matches = match (
                owner_identity,
                self.files.identity(&self.s.file, destination),
            ) {
                (Some(owner), Some(there)) => owner == there,
                _ => {
                    self.switch_file(destination)?;
                    let same = hosted_key(&self.s.file.context.file) == hosted_key(&owner_name);
                    self.switch_file(from)?;
                    same
                }
            };
            if !matches {
                self.triggers.truncate(queued);
                return Err(ErrorCode::UNAVAILABLE.into());
            }
            (destination, native_table)
        } else {
            (from, table)
        };

        // Validate only in the destination's catalog; a colliding root key
        // must never substitute for a missing peer layout. Fail before saves,
        // new windows or found-set changes, and restore the caller on error.
        self.switch_file(destination)?;
        let target = self.related_layout(request.layout.as_ref(), destination_table);
        // The destination already shows a window, not one opened hidden for
        // this step.
        let on_screen = !self.s.file.window.state.hidden;
        self.switch_file(from)?;
        let target = target?;
        // Another file's records on that file's current layout, only the
        // related ones, without a new window or the caller's found set, in a
        // window it already shows: FileMaker Pro 26.0.3 keeps the caller's
        // script and window; that window keeps its layout and shows the
        // related records (script-runtime.md, Go to Related Record). Other
        // forms are unmeasured and keep going to the destination.
        let in_background = request.external_layout
            && destination != from
            && matches!(request.layout, Some(LayoutTarget::Original))
            && request.show_only_related
            && !request.match_found_set
            && !request.new_window
            && on_screen;
        // The new window's style, checked in the caller's windows. A card
        // or dialog over another file's window is not supported.
        let style = if request.new_window {
            let style = self.new_window_style(request.window_options(), bounds)?;
            if destination != from && style != WindowStyle::Document {
                return Err(HostError::Unsupported);
            }
            style
        } else {
            WindowStyle::Document
        };
        self.commit_window()?;
        if destination != from {
            self.flush_background();
            self.switch_file(destination)?;
        }
        let prepared = if request.new_window {
            self.open_window(name, style)
        } else {
            self.commit_window()
        };
        if let Err(error) = prepared {
            self.switch_file(from)?;
            return Err(error);
        }
        let shown = self.show_related(request, target, ids, first, row_record.is_some());
        if in_background {
            // The caller goes on in its own window whatever the destination
            // showed. Its layout did not change: no layout trigger runs
            // there [I].
            self.flush_background();
            self.switch_file(from)?;
            return shown.map(|_| ());
        }
        let visible = shown?;
        if request.new_window {
            self.move_resize_window(&WindowTarget::Current, bounds, false)?;
        }
        // A hidden layout is never entered: no layout trigger runs [I].
        if visible {
            self.after_layout_entered();
        }
        Ok(())
    }

    /// Show `ids`, the related records, in the current window on layout
    /// `target`, `first` (or, when no active row chose it, the first one
    /// shown of another file) current. Whether the layout is one the
    /// account sees.
    fn show_related(
        &mut self,
        request: &RelatedRecords<'_>,
        target: u32,
        ids: Vec<i64>,
        first: i64,
        from_row: bool,
    ) -> HostResult<bool> {
        // Reused windows and new windows cloned from them may be in Find
        // mode. Position the found records in Browse, not the find requests.
        self.s.file.window.set_mode(Mode::Browse);
        self.reveal_window();
        self.show_layout(target);
        let visible = self.layout_shown(target);
        // FileMaker Pro 26.0.3 names a hidden layout Go to Related Record
        // went to in its toolbar (#696's reading).
        self.s.file.window.state.hidden_named = !visible;
        self.sync_layout(false);
        if request.show_only_related {
            self.change(FoundSetChange::Records(ids))?;
        } else {
            let kept = {
                let (window, provider) = self.wp();
                window.found_ids(provider).map_err(data_error)?
            };
            if !kept.contains(&first) {
                self.change(FoundSetChange::ShowAll)?;
            }
        }
        // External GTRR does not retain a relationship/destination sort.
        if request.external_layout {
            self.change(FoundSetChange::Unsort)?;
        }
        let (window, provider) = self.wp();
        let shown = window.found_ids(provider).map_err(data_error)?;
        let first = if request.external_layout && request.show_only_related && !from_row {
            shown.first().copied().unwrap_or(first)
        } else {
            first
        };
        if let Some(i) = shown.iter().position(|id| *id == first) {
            window.go_to(i as u64);
        }
        Ok(visible)
    }

    fn related_layout(&mut self, layout: Option<&LayoutTarget>, table: u64) -> HostResult<u32> {
        let layouts = self.layouts();
        let target = match layout {
            Some(LayoutTarget::Key(key)) => layouts
                .iter()
                .find(|l| u64::from(l.id) == *key)
                .map(|l| l.id),
            Some(LayoutTarget::Name(name)) => layouts
                .iter()
                .find(|l| l.name.eq_ignore_ascii_case(name))
                .map(|l| l.id),
            Some(LayoutTarget::Number(n)) => n
                .checked_sub(1)
                .and_then(|n| usize::try_from(n).ok())
                .and_then(|i| layouts.get(i))
                .map(|l| l.id),
            Some(LayoutTarget::Original) => self.s.file.window.state.layout,
            None => layouts
                .iter()
                .find(|l| self.layout_table(l.id) == Some(table))
                .map(|l| l.id),
        }
        .ok_or(ErrorCode::LAYOUT_MISSING)?;
        // A layout the account may not see is gone to as well, with 0
        // (#696's reading): the window shows `<No Access>` there.
        if self.layout_table(target) != Some(table) {
            return Err(ErrorCode(106).into());
        }
        Ok(target)
    }
}
