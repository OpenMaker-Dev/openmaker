//! Find requests in a server script session: the requests stored in Enter
//! Find Mode, Perform Find, Constrain and Extend Found Set (script-steps.md
//! 11.2, variables already expanded by the runtime, 11.2.3), and how a set
//! of requests combines (11.2.4).
//!
//! Criteria are read with FileMaker's find operators (`om_query::criteria`,
//! the desktop client's too) and solved by the finders `om_query::find`
//! plans; operators the finders cannot express exactly (wildcards, quoted
//! phrases, `?`, `~`, partial dates) are tested on the candidates' stored
//! values, and criteria on unstored calculations on each candidate's
//! computed value (script-runtime.md 13.19). Criteria on fields of a
//! related table are not supported (3). A criterion on a repetition matches
//! the field's value index, which holds every repetition. Criteria on
//! global and summary fields are not criteria (11.2.1, 13.19) and are left
//! out.

use om_data::found::Findable;
use om_hbam::{OwnedNode, RidList};
use om_scripts::runtime::{ErrorCode, HostResult};
use om_scripts::specs;

use super::find::{FindError, Request};
use super::ServerHost;

impl ServerHost {
    /// The requests stored in a step's `06` node, as find requests on the
    /// window's table.
    pub(super) fn stored_requests(&self, node: &OwnedNode) -> HostResult<Vec<Request>> {
        let stored = specs::requests(node).ok_or(ErrorCode::INVALID)?;
        let window = self.window()?;
        let mut requests = Vec::with_capacity(stored.len());
        for stored in stored {
            let mut request = Request {
                omit: stored.omit,
                criteria: Vec::new(),
            };
            for (field, text) in stored.criteria {
                let occurrence = field.occurrence.unwrap_or(window.occurrence);
                if occurrence != window.occurrence
                    && self.schema.table_of(occurrence) != Some(window.table)
                {
                    // A related field's criterion (a join, 11.2.5).
                    return Err(ErrorCode(3).into());
                }
                let (_, def) = self
                    .schema
                    .field(window.occurrence, field.field)
                    .ok_or(ErrorCode::FIELD_MISSING)?;
                if om_data::found::findable(def) != Findable::Ignored {
                    request.set(field.field, text);
                }
            }
            requests.push(request);
        }
        Ok(requests)
    }

    /// The records `requests` find within `master` (11.2.4): the first
    /// request's matches within the master list (or the master list without
    /// them for an omit request); then each find request adds its matches
    /// within the master list and each omit request removes its matches
    /// from what was found so far.
    pub(super) fn find_within(
        &mut self,
        requests: &[Request],
        master: &RidList,
    ) -> Result<RidList, FindError> {
        if super::find::all_empty(requests) {
            return Err(FindError::Empty);
        }
        let mut found = RidList::new();
        for (i, request) in requests.iter().enumerate() {
            let matched = self.matches(request)?;
            found = match (i, request.omit) {
                (0, true) => master.and_not(&matched),
                (_, true) => found.and_not(&matched),
                (_, false) => found.or(&matched.and(master)),
            };
        }
        Ok(found)
    }
}
