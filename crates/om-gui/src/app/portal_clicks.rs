//! Layout buttons, in portal rows too (layout-model.md 5.3; script-runtime.md
//! 13.24 and the desktop notes "Portals"). A button runs its single step
//! (Perform Script included) with its parameter in the script runtime, as
//! every layout button does.
//!
//! A button in a portal row runs in the window's context: its layout, its
//! record (the portal's parent) and its file stay the script's. The row
//! the button is in becomes the active portal row, identified by its
//! related record rather than by its position: `Get (
//! ActivePortalRowNumber )`, the parameter calculation and the related
//! field steps the runtime supports read and write that record
//! (`scripting::host::portals`, "a click in a portal row").
//!
//! A click that has to wait (the focused field's exit triggers, the save
//! of the user's typing, the OnRecordLoad of the row's record in List
//! view, or a row shown while its portal's records are read again, whose
//! record may have left the portal) finds its row again by that record
//! when its button paints next (`DocumentState::wait_portal_click`). Rows reordered, filtered or
//! removed meanwhile, another record, layout, mode or window, or a portal
//! that replaced the clicked one never give the click to another row: the
//! click goes. The script worker checks the row's record once more before
//! the step or its parameter runs and refuses a row that is no longer the
//! portal's.

use super::{Action, Mode, OpenMakerApp, PortalClick, UnsavedRow};
use crate::scripting::{PortalIdentity, PortalRow, Start};
use crate::session::RenderState;

impl OpenMakerApp {
    /// Run layout button `object_id` on record `record_index` (List view:
    /// that record becomes current first), in portal row `row` if any.
    pub(super) fn run_button(
        &mut self,
        ctx: &egui::Context,
        object_id: u64,
        record_index: u64,
        row: Option<PortalRow>,
    ) {
        let step = self
            .w
            .session
            .as_ref()
            .and_then(|session| match &session.render {
                RenderState::Loaded(layout) => {
                    layout.layout.objects.get(object_id)?.script_step().cloned()
                }
                _ => None,
            });
        let Some(step) = step else {
            return;
        };
        // A parameter calculation that did not decode cannot run.
        let unsupported = self.w.session.as_ref().is_some_and(|s| match &s.render {
            RenderState::Loaded(layout) => {
                layout.layout.objects.get(object_id).is_some_and(|object| {
                    object.script_parameter_unsupported && object.calcs.script_parameter.is_none()
                })
            }
            _ => false,
        });
        if unsupported {
            self.error = Some(
                "This button uses a script parameter calculation that OpenMaker \
                 cannot evaluate yet."
                    .into(),
            );
            return;
        }
        self.apply(ctx, Action::GoToRecord(record_index));
        let object = self.w.session.as_ref().and_then(|s| match &s.render {
            RenderState::Loaded(layout) => layout.layout.objects.get(object_id).cloned(),
            _ => None,
        });
        let literal = object
            .as_ref()
            .and_then(|o| o.script_parameter.clone())
            .unwrap_or_default();
        let formula = object.and_then(|o| o.calcs.script_parameter);
        let Some(step) = om_data::ScriptStep::from_layout(&step) else {
            self.error = Some("This button's script step is not valid.".into());
            return;
        };
        // The parameter calculation runs on the button's record, now the
        // current one (and in the button's portal row).
        self.start_script(
            ctx,
            Start::Button {
                step: Box::new(step),
                formula: formula.map(Box::new),
                literal,
                portal: row,
            },
        );
    }

    /// The user clicked button `object_id` in a portal row (`click`) of
    /// record `record_index`. A click read from this frame's rows runs at
    /// once; one that waited first finds its row again.
    pub(super) fn portal_button(
        &mut self,
        ctx: &egui::Context,
        object_id: u64,
        record_index: u64,
        click: PortalClick,
    ) {
        if !click.current {
            self.w.document.wait_portal_click(object_id, click, false);
            return;
        }
        if !self.portal_click_is_current(object_id, record_index, &click) {
            self.w.document.cancel_pending_focus();
            return;
        }
        if self
            .w
            .session
            .as_ref()
            .is_some_and(|session| session.current != record_index)
        {
            // List view: the row's record becomes current first, with its
            // triggers; the click then finds its row on that record, and
            // goes if the record did not become current.
            self.apply(ctx, Action::GoToRecord(record_index));
            self.w.document.wait_portal_click(object_id, click, true);
            return;
        }
        let row = PortalRow {
            object: click.portal,
            occurrence: click.occurrence,
            row: click.row,
            identity: self.clicked_identity(&click),
            clicked: false,
            field: None,
            repetition: 1,
        };
        self.run_button(ctx, object_id, record_index, Some(row));
    }

    /// The worker's identity of the clicked row: its related record, the
    /// creation row, or the record the user began in that row, by its
    /// place among what the window's record keeps uncommitted (`None` when
    /// it keeps no such record any more).
    fn clicked_identity(&self, click: &PortalClick) -> Option<PortalIdentity> {
        self.row_identity(click.record, click.unsaved)
    }

    /// [`Self::clicked_identity`] of a row showing `record`, or the row
    /// without a record `unsaved` names.
    pub(super) fn row_identity(
        &self,
        record: u64,
        unsaved: Option<UnsavedRow>,
    ) -> Option<PortalIdentity> {
        match unsaved {
            None => Some(PortalIdentity::Record(record)),
            Some(UnsavedRow::Creation) => Some(PortalIdentity::Creation),
            Some(UnsavedRow::Begun(id)) => self
                .uncommitted()?
                .related
                .iter()
                .position(|d| d.begun == Some(id))
                .map(PortalIdentity::New),
        }
    }

    /// Whether `click` is a click on button `object_id`, still inside the
    /// clicked portal, in Browse mode on the window's record `record_index`
    /// and layout.
    fn portal_click_is_current(
        &self,
        object_id: u64,
        record_index: u64,
        click: &PortalClick,
    ) -> bool {
        let Some(session) = &self.w.session else {
            return false;
        };
        let RenderState::Loaded(render) = &session.render else {
            return false;
        };
        let layout = &render.layout;
        let occurrence = layout
            .objects
            .get(click.portal)
            .and_then(|portal| portal.portal.as_ref())
            .and_then(|portal| om_layout::occurrence_id(portal.table_alias_key));
        self.w.mode == Mode::Browse
            && session.layout.map(u64::from) == Some(click.layout)
            && layout.id == click.layout
            && session
                .record_at(record_index)
                .and_then(|(_, record)| u64::try_from(record.id).ok())
                == Some(click.parent)
            && occurrence == Some(click.occurrence)
            && crate::document::enclosing_portal(layout, object_id) == Some(click.portal)
    }
}
