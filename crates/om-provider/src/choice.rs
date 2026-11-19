//! A field control's value-list items read over this connection, for
//! `om-author`'s `list_choices` (`om_data::choice`, issue #178): through
//! the calculation host over this connection's session, as the provider
//! computes a page's calculations, so the items are those the desktop's
//! controls offer, with the account's privileges.

use om_data::choice::{self, ChoiceError, ChoiceTarget, Choices, Unavailable};
use om_data::host::FileCalc;
use om_data::meta::ValueList;

use super::*;
use crate::calc::NativeSource;

/// A control's items and the list they come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedChoices {
    pub list: ValueList,
    pub choices: Choices,
}

/// One reading of a value list: the list or why it could not be listed,
/// and what its calculations asked for.
type Pass = (Result<ListedChoices, ChoiceError>, crate::calc::SchemaNeeds);

impl NativeProvider {
    /// The items field control `target.object` of layout `target.layout`
    /// offers on record `target.record` of the layout's table: every one,
    /// or an error (`om_data::choice::choices`).
    ///
    /// Refused before anything is read: a missing layout, object or record,
    /// an object that is not a field control with a value list, a related
    /// field (which portals show), and a control showing several
    /// repetitions. A table the account cannot read is an error, never a
    /// shorter list.
    pub fn value_list_choices(
        &mut self,
        target: ChoiceTarget,
    ) -> Result<ListedChoices, ChoiceError> {
        // A calculation that read the objects of a layout listed from the
        // catalog's index only (#1019), or a related file not open yet
        // (#1125): its member or its file, then the list again.
        let mut asked = std::collections::BTreeSet::new();
        loop {
            let (listed, needs) = self.value_list_choices_once(target)?;
            if !self
                .grow_for(needs, &mut asked)
                .map_err(|error| ChoiceError::Failed(error.code.unwrap_or(3)))?
            {
                return listed;
            }
        }
    }

    /// [`Self::value_list_choices`] once: the list, or why it could not be
    /// listed, with what its calculations asked for, which come back also
    /// when listing failed: a value its calculation read as missing
    /// before its file was open fails the list, and the list is read again
    /// once the file is open before that counts (#1125).
    fn value_list_choices_once(&mut self, target: ChoiceTarget) -> Result<Pass, ChoiceError> {
        let layout_id = u32::try_from(target.layout).map_err(|_| ChoiceError::NoControl)?;
        let layout = self
            .decoded_layout(layout_id)
            .map_err(|_| ChoiceError::NoControl)?;
        let object = layout
            .objects
            .objects
            .iter()
            .find(|object| object.id == target.object && object.field_obj.is_some())
            .ok_or(ChoiceError::NoControl)?;
        let alias = layout.def.as_ref().map_or(0, |def| def.table_alias_key);
        choice::supported(object, alias, false)?;
        let schema = self
            .calc_schema_cached()
            .map_err(|_| ChoiceError::Unavailable(Unavailable::NoCalcHost))?;
        let list = choice::value_list(&schema, object)?.clone();
        let occurrence = om_layout::occurrence_id(alias).ok_or(ChoiceError::NoControl)?;
        let table = schema
            .table_of(occurrence)
            .map(|table| table.id)
            .ok_or(ChoiceError::NoControl)?;
        if self
            .read_record(table, target.record)
            .map_err(|error| ChoiceError::Failed(error.code.unwrap_or(3)))?
            .is_none()
        {
            return Err(ChoiceError::TargetGone);
        }
        let session_info = self.session_info();
        let today = om_data::host::session::today(session_info.utc_offset);
        let globals = self.desktop_globals(&schema);
        let session = self
            .session
            .as_mut()
            .ok_or(ChoiceError::Unavailable(Unavailable::NoCalcHost))?;
        let source = NativeSource::new(session, &schema, today)
            .with_calculation_session(session_info.clone())
            .with_session_globals(globals)
            .with_library(&self.calc.library)
            .with_fonts(&self.calc.fonts)
            .with_external(&self.external);
        // An unstored calculation the list reads evaluates with this
        // connection's session (`Get ( AccountName )` and the like).
        let file = FileCalc::new(&schema, &source, today).with_session(&session_info);
        let context = choice::context(&schema, &list, occurrence, target.record, None);
        let choices = choice::choices(&file, &list, &context);
        // A table this account cannot read reads as missing; its values
        // would be left out of the list, so the list is an error instead.
        if let Some(failure) = source.take_failure() {
            return Err(ChoiceError::Failed(failure.code.unwrap_or(3)));
        }
        let listed = choices.map(|choices| ListedChoices { list, choices });
        Ok((listed, source.take_needs()))
    }
}
