//! Set Web Viewer (146) and Perform JavaScript in Web Viewer (175). The UI
//! owns the native views (`crate::web_viewer`), so a step names a web viewer
//! object of the window's layout, flushes the window the script left, and
//! waits for the UI's answer, serving the UI's requests meanwhile: the page
//! loads and the other windows keep drawing while the function runs.

use om_data::calc::Value;
use om_data::runtime::{ErrorCode, HostResult, Mode};

use super::WorkerHost;
use crate::scripting::{Answer, ScriptEvent, View};
use crate::web_viewer::{Command, INVALID, OBJECT_MISSING};

impl WorkerHost<'_> {
    /// The layout and object id of the web viewer `name` (exactly, as Go to
    /// Object finds objects) in the window's Browse layout; an empty name is
    /// the active object when it is a web viewer and `active` allows it.
    fn web_viewer_object(&mut self, name: &str, active: bool) -> HostResult<(u32, u64)> {
        let state = &self.s.file.window.state;
        if name.is_empty() && !active {
            return Err(ErrorCode(INVALID).into());
        }
        // Web viewers show pages only in Browse, in Form or List view.
        if state.mode != Mode::Browse || state.view == View::Table {
            return Err(ErrorCode(OBJECT_MISSING).into());
        }
        let current = state.object;
        let layout = state.layout.ok_or(ErrorCode(OBJECT_MISSING))?;
        let render = self.render(layout).ok_or(ErrorCode(OBJECT_MISSING))?;
        let objects = &render.layout.objects;
        let object = if name.is_empty() {
            current
                .and_then(|id| objects.get(id))
                .ok_or(ErrorCode(INVALID))?
        } else {
            objects
                .objects
                .iter()
                .find(|o| o.name.as_deref() == Some(name))
                .ok_or(ErrorCode(OBJECT_MISSING))?
        };
        if object.web_viewer().is_none() {
            return Err(ErrorCode(if name.is_empty() {
                INVALID
            } else {
                OBJECT_MISSING
            })
            .into());
        }
        Ok((layout, object.id))
    }

    /// Ask the UI to carry out `command` on the window's web viewer.
    fn web_viewer_command(&mut self, command: Command) -> HostResult {
        if self.connection_lost() {
            return Err(ErrorCode(OBJECT_MISSING).into());
        }
        self.reveal_window();
        self.flush();
        self.s.next_prompt += 1;
        let id = self.s.next_prompt;
        self.send(ScriptEvent::WebViewer { id, command });
        match self.pump(Some(id)) {
            Some(Answer::WebViewer(Ok(()))) => Ok(()),
            Some(Answer::WebViewer(Err(code))) => Err(ErrorCode(code).into()),
            _ => Err(ErrorCode(OBJECT_MISSING).into()),
        }
    }

    pub(super) fn set_named_web_viewer(
        &mut self,
        name: &str,
        action: u16,
        url: Option<&str>,
    ) -> HostResult {
        let (layout, object) = self.web_viewer_object(name, false)?;
        self.web_viewer_command(Command::Set {
            layout,
            object,
            action,
            url: url.map(str::to_owned),
        })
    }

    pub(super) fn perform_named_javascript(
        &mut self,
        name: &str,
        function: &str,
        parameters: &[Value],
    ) -> HostResult {
        let (layout, object) = self.web_viewer_object(name, true)?;
        if function.trim().is_empty() {
            return Err(ErrorCode(INVALID).into());
        }
        // The function takes text parameters.
        let parameters = parameters.iter().map(|value| self.text(value)).collect();
        self.web_viewer_command(Command::Perform {
            layout,
            object,
            function: function.to_owned(),
            parameters,
        })
    }
}
