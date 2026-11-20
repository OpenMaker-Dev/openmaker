//! Targeted refreshes use the same schema, provider cache lifetime and layout
//! identities as ordinary calculations. They never leave/commit a field.

use om_data::runtime::{ErrorCode, HostError, HostResult, Mode};
use om_layout::ObjectKind;

use super::{data_error, WorkerHost};
use crate::scripting::ObjectRefresh;

impl WorkerHost<'_> {
    pub(super) fn refresh_named_object(
        &mut self,
        name: &str,
        repetition: u32,
        portal_step: bool,
    ) -> HostResult {
        // Those screens use field columns/requests rather than the layout's
        // object evaluation path. Do not acknowledge an invisible refresh.
        if self.s.file.window.state.mode != Mode::Browse
            || self.s.file.window.state.view == crate::scripting::View::Table
        {
            self.s.failure = Some(
                "Targeted refresh currently requires a Browse layout in Form or List view.".into(),
            );
            return Err(HostError::Unsupported);
        }
        let missing = || HostError::Error(ErrorCode(116));
        let layout = self.s.file.window.state.layout.ok_or_else(missing)?;
        let render = self.render(layout).ok_or_else(missing)?;
        let state = &self.s.file.window.state;
        let object = if !name.is_empty() {
            render
                .layout
                .objects
                .objects
                .iter()
                .find(|o| o.name.as_deref() == Some(name))
        } else if portal_step {
            let active = state
                .portal
                .map(|p| p.field.unwrap_or(p.object))
                .or(state.object);
            active
                .and_then(|id| render.layout.objects.get(id))
                .or_else(|| {
                    let id = super::super::field_object(&render, state.focus.as_deref()?)?;
                    render.layout.objects.get(id)
                })
        } else {
            return Err(ErrorCode::INVALID.into());
        }
        .ok_or_else(missing)?;
        if matches!(object.kind(), ObjectKind::External | ObjectKind::Unknown(_)) {
            self.s.failure = Some(
                "Refreshing external or unknown layout object contents is unsupported.".into(),
            );
            return Err(HostError::Unsupported);
        }
        let id = object.id;
        let field_target = object.field_spec().is_some();
        if let Some(spec) = object.field_spec() {
            let occurrence =
                om_layout::occurrence_id(spec.table_key).ok_or(HostError::Unsupported)?;
            let field = spec.field_id().ok_or(HostError::Unsupported)?;
            let def = self
                .schema()
                .and_then(|s| s.field(occurrence, field))
                .ok_or(HostError::Unsupported)?;
            if repetition > def.repetitions.max(1) {
                return Err(ErrorCode::FIELD_MISSING.into());
            }
        }
        let portal = if portal_step && object.kind() == ObjectKind::Portal {
            Some(self.portal_by_id(id).ok_or(HostError::Unsupported)?)
        } else {
            None
        };
        let mut objects = vec![id];
        for child in &render.layout.objects.objects {
            let mut parent = child.parent;
            while let Some(at) = parent {
                if at == id {
                    objects.push(child.id);
                    break;
                }
                parent = render.layout.objects.get(at).and_then(|o| o.parent);
            }
        }
        if objects
            .iter()
            .filter_map(|id| render.layout.objects.get(*id))
            .any(|object| {
                !object.calcs.undecodable.is_empty()
                    || matches!(object.kind(), ObjectKind::External | ObjectKind::Unknown(_))
                    || object
                        .field_obj
                        .as_ref()
                        .is_some_and(|field| field.num_of_reps > 1)
            })
        {
            self.s.failure = Some("Target refresh requires decoded layout calculations and supported contents; external objects and multi-repetition grids are unsupported.".into());
            return Err(HostError::Unsupported);
        }
        if !self.s.file.window.can_refresh_object() {
            return Err(ErrorCode::MEMORY.into());
        }
        // Preserve draft originals before expiring inputs, as Refresh Window
        // does, but retain the page and worker record itself.
        let record = {
            let (window, provider) = self.wp();
            window.record_id(provider).map_err(data_error)?
        };
        let active = self
            .s
            .file
            .window
            .state
            .portal
            .filter(|r| portal.is_some_and(|p| p.object == r.object));
        // Unordered rows have stable record/draft identities. The existing
        // sorted/filtered-row refusal remains; never reuse its old row index.
        let previous = active.and_then(|row| row.identity);
        let join = portal.and_then(|p| {
            Some((
                self.s.file.occurrence?,
                u64::try_from(record?).ok()?,
                p.occurrence,
            ))
        });
        let generation = self.p().refresh_object(join).map_err(data_error)?;
        self.s.file.cache.borrow_mut().refresh_object(join);
        self.s.file.cache.borrow_mut().generation = generation;
        let mut remap_error = None;
        if let (Some(p), Some(mut row)) = (portal, active) {
            let number = match previous {
                Some(previous) if !p.ordered => match self.number_of(&p, previous) {
                    Ok(number) => number,
                    Err(error) => {
                        remap_error = Some(error);
                        None
                    }
                },
                _ => None,
            };
            row.row = number.unwrap_or(0);
            if row.row == 0 {
                row.identity = None;
                row.field = None;
                row.repetition = 1;
                self.s.file.window.state.select = false;
            }
            self.s.file.window.state.portal = Some(row);
            self.s.file.window.row_record = None;
        }
        self.s.file.window.refresh_object(ObjectRefresh {
            layout,
            objects,
            repetition: if field_target { repetition } else { 0 },
            join,
            generation,
        });
        match remap_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
