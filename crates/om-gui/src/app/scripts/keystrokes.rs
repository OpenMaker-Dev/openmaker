//! OnObjectKeystroke and OnLayoutKeystroke: the keys the user types into a
//! window in Browse mode go to its keystroke triggers before the field
//! editor or the layout gets them.
//!
//! The Claris reference (OnObjectKeystroke, OnLayoutKeystroke): FileMaker's
//! keyboard shortcuts come first and do not activate the triggers (the
//! Command key on macOS; Alt and Ctrl on Windows). Then the active object's
//! OnObjectKeystroke processes the characters, then the layout's
//! OnLayoutKeystroke, also when no object is active. Either trigger stops
//! the keystroke by returning False. Otherwise navigational keys navigate,
//! and other keys go to the active object. `Get ( TriggerKeystroke )` is the
//! characters; navigational keys are the control characters of the Code
//! function's table. `Get ( TriggerModifierKeys )` is the modifier keys as
//! they were when the key was pressed, valued as for `Get (
//! ActiveModifierKeys )`.
//!
//! egui reads Tab, Escape and the arrow keys for keyboard focus as each
//! pass begins, before any of the window's code runs. So [`KeyGate`], an
//! egui plugin, takes the keys out of the window's raw input. The keys the
//! triggers let through go back in, as they were, at the start of the
//! window's next pass, and the editor applies them as it applies any other
//! key. Keys go one at a time: one typed while another waits goes after
//! it. A key goes only to the window, file, mode, layout, record and editor
//! it was typed into. When any of these changed while its triggers ran, it
//! is dropped and its remaining trigger does not run.

use std::collections::{HashMap, VecDeque};

use egui::{Event, Key, Modifiers, ViewportId};
use om_data::runtime::{TriggerEvent, TriggerInfo};

use super::Held;
use crate::app::{Mode, OpenMakerApp};

/// A key the user typed that the window's keystroke triggers can see.
#[derive(Clone, Debug)]
pub(in crate::app) struct Keystroke {
    /// The event as egui delivered it, given back when the triggers let it.
    event: Event,
    /// `Get ( TriggerKeystroke )`: `None` for keys without a code in the
    /// Claris reference, which reach the editor without a trigger.
    text: Option<String>,
    /// `Get ( TriggerModifierKeys )`.
    modifiers: i64,
}

impl Keystroke {
    fn is_escape(&self) -> bool {
        matches!(
            self.event,
            Event::Key {
                key: Key::Escape,
                ..
            }
        )
    }
}

/// The characters of a navigational key: the Code function's table in the
/// Claris reference. Backspace 8, Tab and Shift-Tab 9, Return 13, Escape
/// 27, left arrow 28, up arrow 29, right arrow 30, down arrow 31, forward
/// delete 127. egui has one Enter key, Return's 13; the numeric keypad's
/// Enter (10) is not told apart.
fn navigation(key: Key) -> Option<char> {
    Some(match key {
        Key::Backspace => '\u{8}',
        Key::Tab => '\t',
        Key::Enter => '\r',
        Key::Escape => '\u{1b}',
        Key::ArrowLeft => '\u{1c}',
        Key::ArrowUp => '\u{1d}',
        Key::ArrowRight => '\u{1e}',
        Key::ArrowDown => '\u{1f}',
        Key::Delete => '\u{7f}',
        _ => return None,
    })
}

/// Editing keys the reference gives no code for. Typed while earlier keys
/// wait, they wait their turn too, and then go to the editor without a
/// trigger.
fn untriggered(key: Key) -> bool {
    matches!(key, Key::Home | Key::End | Key::PageUp | Key::PageDown)
}

/// The keystroke `event` is, when the window's keystroke triggers see it:
/// typed characters and the navigational keys, without a keyboard
/// shortcut's modifiers. These are Command on macOS, and Ctrl or Alt
/// elsewhere (Windows', also used on Linux, which FileMaker does not run
/// on). Ctrl-Up and Ctrl-Down are FileMaker's previous and next record
/// shortcuts on every platform. With `all`, the editing keys without a code
/// are taken too.
pub(in crate::app) fn keystroke(
    event: &Event,
    modifiers: Modifiers,
    mac: bool,
    all: bool,
) -> Option<Keystroke> {
    let (text, modifiers) = match event {
        Event::Text(text) if !text.is_empty() => (Some(text.clone()), modifiers),
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            let shortcut = modifiers.command
                || modifiers.mac_cmd
                || (!mac && (modifiers.ctrl || modifiers.alt))
                || (modifiers.ctrl && matches!(key, Key::ArrowUp | Key::ArrowDown));
            if shortcut {
                return None;
            }
            match navigation(*key) {
                Some(c) => (Some(c.to_string()), *modifiers),
                None if all && untriggered(*key) => (None, *modifiers),
                None => return None,
            }
        }
        _ => return None,
    };
    Some(Keystroke {
        event: event.clone(),
        text,
        modifiers: super::modifier_keys(modifiers),
    })
}

/// Takes the keys of a window whose keystroke triggers decide them out of
/// its viewport's raw input, and gives back those they let through, before
/// egui begins the viewport's next pass.
#[derive(Default)]
pub(in crate::app) struct KeyGate {
    /// Root passes begun. What one pass left for a viewport is stale once
    /// a root pass went by without the viewport's: its window was not drawn.
    pass: u64,
    armed: HashMap<ViewportId, Arm>,
    taken: HashMap<ViewportId, (u32, Vec<Keystroke>)>,
    release: HashMap<ViewportId, (u64, Vec<Event>)>,
    /// The modifier keys down in each viewport, as its events said.
    modifiers: HashMap<ViewportId, Modifiers>,
}

/// The window whose triggers see the viewport's next keys.
#[derive(Clone, Copy)]
struct Arm {
    window: u32,
    /// Escape too (no script runs, which Escape would stop).
    escape: bool,
    /// Keys waiting: every editing key waits its turn.
    all: bool,
    /// An option set uses a Space key event and keeps arrow navigation
    /// inside the set. Preserve those events through the same owner gate.
    option_set: bool,
    pass: u64,
}

impl egui::plugin::Plugin for KeyGate {
    fn debug_name(&self) -> &'static str {
        "keystroke triggers"
    }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let viewport = input.viewport_id;
        if viewport == ViewportId::ROOT {
            self.pass += 1;
        }
        let pass = self.pass;
        let fresh = move |set: u64| set + 1 >= pass;
        let mac = ctx.os() == egui::os::OperatingSystem::Mac;
        let arm = self.armed.remove(&viewport).filter(|arm| fresh(arm.pass));
        let mut modifiers = self.modifiers.get(&viewport).copied().unwrap_or_default();
        let mut taken = Vec::new();
        input.events.retain(|event| {
            match event {
                Event::ModifiersChanged(now) => modifiers = *now,
                Event::WindowFocused(false) => modifiers = Modifiers::default(),
                _ => {}
            }
            let Some(arm) = arm else {
                return true;
            };
            if arm.option_set && matches!(event, Event::Text(text) if text == " ") {
                // Space's key event is the action; its accompanying text
                // event must not enter the item a second time.
                return false;
            }
            let mut key = match event {
                Event::Key {
                    key: Key::Space,
                    pressed: true,
                    modifiers,
                    ..
                } if arm.option_set && modifiers.is_none() => Some(Keystroke {
                    event: event.clone(),
                    text: Some(" ".into()),
                    modifiers: super::modifier_keys(*modifiers),
                }),
                _ => keystroke(event, modifiers, mac, arm.all),
            };
            if arm.option_set
                && matches!(
                    event,
                    Event::Key {
                        key: Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight,
                        ..
                    }
                )
            {
                if let Some(key) = &mut key {
                    // Existing option controls navigate with arrows
                    // without a keystroke trigger, but still wait in order.
                    key.text = None;
                }
            }
            match key {
                Some(key) if arm.escape || !key.is_escape() => {
                    taken.push(key);
                    false
                }
                _ => true,
            }
        });
        self.modifiers.insert(viewport, modifiers);
        match arm {
            Some(arm) => {
                self.taken.insert(viewport, (arm.window, taken));
            }
            None => {
                self.taken.remove(&viewport);
            }
        }
        if let Some((set, events)) = self.release.remove(&viewport) {
            if fresh(set) {
                input.events.splice(0..0, events);
            }
        }
    }
}

impl KeyGate {
    /// The keys taken for `window` in this pass of `viewport`.
    fn take(&mut self, viewport: ViewportId, window: u32) -> Vec<Keystroke> {
        match self.taken.get_mut(&viewport) {
            Some((owner, keys)) if *owner == window => std::mem::take(keys),
            _ => Vec::new(),
        }
    }

    fn arm(
        &mut self,
        viewport: ViewportId,
        window: u32,
        escape: bool,
        all: bool,
        option_set: bool,
    ) {
        let pass = self.pass;
        self.armed.insert(
            viewport,
            Arm {
                window,
                escape,
                all,
                option_set,
                pass,
            },
        );
    }

    fn release(&mut self, viewport: ViewportId, event: Event) {
        let pass = self.pass;
        let (set, events) = self.release.entry(viewport).or_default();
        *set = pass;
        events.push(event);
    }

    #[cfg(test)]
    pub(in crate::app) fn released(&self, viewport: ViewportId) -> Vec<Event> {
        self.release
            .get(&viewport)
            .map(|(_, events)| events.clone())
            .unwrap_or_default()
    }
}

/// The window a key was typed into, as it was then.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct KeyOwner {
    epoch: u64,
    /// The window instance (`Get ( WindowUUID )`), not just its number.
    window: String,
    file: u64,
    mode: Mode,
    layout: Option<u32>,
    record: Option<i64>,
}

/// What had the window's keyboard: the active field, its object and its
/// editor; nothing (the layout's keystrokes); or another control (the
/// record number box, a button), whose keys no trigger sees.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct KeyTarget {
    field: Option<String>,
    object: Option<u64>,
    editor: Option<egui::Id>,
}

/// A key its triggers decide.
#[derive(Clone, Debug)]
pub(in crate::app) struct HeldKey {
    key: Keystroke,
    owner: KeyOwner,
    target: KeyTarget,
    /// The triggers still to run, in order.
    stages: Vec<(TriggerEvent, Option<u64>)>,
}

/// The window's keys on their way through its keystroke triggers.
#[derive(Default)]
pub(in crate::app) struct Keys {
    /// Typed while an earlier key waited, in order, with where.
    ahead: VecDeque<(KeyOwner, KeyTarget, Keystroke)>,
    /// Let through (or without a trigger): it goes to its editor when the
    /// window's pass ends.
    approved: Option<Box<HeldKey>>,
    /// Given back for the window's next pass: the next key waits until the
    /// editor has applied it.
    released: bool,
    /// An approved navigation key may move the keyboard. Only that key,
    /// with no intervening pointer focus change, can transfer its typeahead.
    navigation: Option<(KeyOwner, KeyTarget)>,
}

impl OpenMakerApp {
    /// The window as a key typed into it now belongs to it.
    pub(in crate::app) fn key_owner(&self) -> KeyOwner {
        let session = self.w.session.as_ref();
        KeyOwner {
            epoch: self.epoch,
            window: self.w.uuid.clone(),
            file: self.w.file_id,
            mode: self.w.mode,
            layout: session.and_then(|s| s.layout),
            record: session.and_then(|s| s.current_record()).map(|(_, r)| r.id),
        }
    }

    /// What has the window's keyboard now. The editor is egui's, so this
    /// is only meaningful in the window's own pass.
    fn key_target(&self, ctx: &egui::Context) -> KeyTarget {
        let field = self.w.document.active.clone();
        KeyTarget {
            object: field.as_deref().and_then(|f| {
                self.w
                    .document
                    .active_object
                    .or_else(|| self.field_object(f))
            }),
            field,
            editor: ctx.memory(|m| m.focused()),
        }
    }

    /// The keystroke triggers of `target`, in FileMaker's order: the active
    /// object's OnObjectKeystroke, then the layout's OnLayoutKeystroke.
    /// Browse mode only; Find and Preview mode keys reach the window
    /// without them.
    fn keystroke_stages(&self, target: &KeyTarget) -> Vec<(TriggerEvent, Option<u64>)> {
        if self.w.mode != Mode::Browse || target.field.is_some() != target.editor.is_some() {
            return Vec::new();
        }
        let enabled = |event, object| {
            self.trigger_record(event, object)
                .is_some_and(|t| t.script.is_some() && t.modes & 1 != 0)
        };
        let mut stages = Vec::new();
        if let Some(object) = target
            .object
            .filter(|o| enabled(TriggerEvent::ObjectKeystroke, Some(*o)))
        {
            stages.push((TriggerEvent::ObjectKeystroke, Some(object)));
        }
        if enabled(TriggerEvent::LayoutKeystroke, None) {
            stages.push((TriggerEvent::LayoutKeystroke, None));
        }
        stages
    }

    /// Whether keys typed into the window wait: for their triggers, for
    /// the editor, or behind another key.
    fn keys_waiting(&self) -> bool {
        let id = self.w.id;
        let keys = &self.w.keys;
        let key = |window: u32, held: &Held| window == id && matches!(held, Held::Key(_));
        !keys.ahead.is_empty()
            || keys.approved.is_some()
            || keys.released
            || self.script.held.values().any(|(w, h)| key(*w, h))
            || self.script.resumes.iter().any(|(w, h, ..)| key(*w, h))
    }

    /// Something other than the document has the keyboard: a dialog, an
    /// alert, a menu or popup; or the record is being saved. The open list
    /// of the drop-down list being edited is not elsewhere: its editor
    /// keeps the keyboard, and typing there goes to the triggers.
    fn keyboard_elsewhere(&self, ctx: &egui::Context) -> bool {
        let popup = ctx.any_popup_open() && !self.w.document.editor_list_open(ctx);
        self.dialog_over_document() || popup || self.w.document.pending
    }

    /// The keys taken out of this pass's input for the window wait their
    /// turn, in order.
    pub(in crate::app) fn take_keystrokes(&mut self, ctx: &egui::Context) {
        let (viewport, window) = (ctx.viewport_id(), self.w.id);
        let keys = ctx
            .with_plugin(|gate: &mut KeyGate| gate.take(viewport, window))
            .unwrap_or_default();
        if keys.is_empty() {
            return;
        }
        let owner = self.key_owner();
        let target = self.key_target(ctx);
        self.w.keys.ahead.extend(
            keys.into_iter()
                .map(|key| (owner.clone(), target.clone(), key)),
        );
    }

    /// The window's pass has ended. The key let through goes to its editor
    /// in the next pass; when none waits for its triggers, the next key
    /// goes to its own; and the gate takes the next pass's keys when the
    /// window has keystroke triggers, or keys wait. A window without the
    /// keyboard drops its keys.
    pub(in crate::app) fn settle_keystrokes(&mut self, ctx: &egui::Context, input: bool) {
        let id = self.w.id;
        if !self.script.running {
            // No script is left to answer for them (the worker stopped).
            self.script
                .held
                .retain(|_, (w, h)| *w != id || !matches!(h, Held::Key(_)));
        }
        if !input {
            self.w.keys = Keys::default();
            return;
        }
        if ctx.input(|i| {
            i.events.iter().any(|event| {
                matches!(
                    event,
                    Event::PointerButton { pressed: true, .. } | Event::WindowFocused(false)
                )
            })
        }) {
            self.w.keys.navigation = None;
        }
        // A key given back last pass was applied in this one; the next
        // waits for the focus it moved to settle.
        if !std::mem::take(&mut self.w.keys.released) {
            if self.w.keys.approved.is_none() && self.may_dispatch_key() {
                if let Some((owner, from)) = self.w.keys.navigation.take() {
                    if owner == self.key_owner() && !self.keyboard_elsewhere(ctx) {
                        let target = self.key_target(ctx);
                        for (queued_owner, queued_target, _) in &mut self.w.keys.ahead {
                            if *queued_owner == owner && *queued_target == from {
                                *queued_target = target.clone();
                            }
                        }
                    }
                }
                self.dispatch_keystroke(ctx);
            }
            self.release_keystroke(ctx);
        }
        // The next pass applies the key given back, or takes the next one.
        // A trigger's end wakes the window itself.
        let keys = &self.w.keys;
        if keys.released || (!keys.ahead.is_empty() && self.may_dispatch_key()) {
            ctx.request_repaint();
        }
        let waiting = self.keys_waiting();
        let gated = self.w.session.is_some()
            && self.w.connection_lost.is_none()
            && !self.recovery_holds()
            && !self.keyboard_elsewhere(ctx)
            && (waiting
                || (!self.script.busy()
                    && !self.keystroke_stages(&self.key_target(ctx)).is_empty()));
        if gated {
            // Esc stops a running script (`abort_keys`).
            let escape = !self.script.running;
            let viewport = ctx.viewport_id();
            let option_set = self
                .key_target(ctx)
                .object
                .and_then(|object| self.option_set_keys(object))
                .is_some();
            ctx.with_plugin(|gate: &mut KeyGate| {
                gate.arm(viewport, id, escape, waiting, option_set)
            });
        }
    }

    /// No script runs or waits for the window, and its record is not
    /// being saved.
    fn may_dispatch_key(&self) -> bool {
        let id = self.w.id;
        !self.script.busy()
            && !self.w.document.pending
            && !self.script.held.values().any(|(w, _)| *w == id)
    }

    /// The next key typed goes to its triggers, or without any to its
    /// editor. One typed before the window went to another layout, record
    /// or mode goes.
    fn dispatch_keystroke(&mut self, ctx: &egui::Context) {
        while let Some((owner, target, key)) = self.w.keys.ahead.pop_front() {
            if owner != self.key_owner() || target != self.key_target(ctx) {
                continue;
            }
            let stages = if key.text.is_some() {
                self.keystroke_stages(&target)
            } else {
                Vec::new()
            };
            self.continue_keystroke(
                ctx,
                HeldKey {
                    key,
                    owner,
                    target,
                    stages,
                },
            );
            return;
        }
    }

    /// Run `held`'s next trigger, holding the key until its script ends;
    /// with none left, the key is let through.
    fn continue_keystroke(&mut self, ctx: &egui::Context, mut held: HeldKey) {
        while !held.stages.is_empty() {
            let (event, object) = held.stages.remove(0);
            let info = TriggerInfo {
                keystroke: held.key.text.clone(),
                modifier_keys: Some(held.key.modifiers),
                ..TriggerInfo::default()
            };
            let next = Held::Key(Box::new(held.clone()));
            if self.fire(ctx, event, object, Some(next), info) {
                return;
            }
        }
        self.w.keys.approved = Some(Box::new(held));
    }

    /// A keystroke trigger let the key go on (`proceed`), or stopped it:
    /// then nothing is typed and no further trigger runs. Nor do they when
    /// the window, its layout, record, mode or active field changed while
    /// the trigger ran.
    pub(super) fn resume_keystroke(&mut self, ctx: &egui::Context, held: HeldKey, proceed: bool) {
        let object = self.w.document.active.as_deref().and_then(|f| {
            self.w
                .document
                .active_object
                .or_else(|| self.field_object(f))
        });
        if !proceed
            || held.owner != self.key_owner()
            || held.target.field != self.w.document.active
            || held.target.object != object
        {
            return;
        }
        self.continue_keystroke(ctx, held);
    }

    /// The key let through goes back into the window's input for its next
    /// pass, for the editor it was typed into. When that editor no longer
    /// has the keyboard, or something else does (a dialog), it goes.
    fn release_keystroke(&mut self, ctx: &egui::Context) {
        let Some(held) = self.w.keys.approved.take() else {
            return;
        };
        if held.owner != self.key_owner()
            || held.target != self.key_target(ctx)
            || self.keyboard_elsewhere(ctx)
        {
            return;
        }
        if let (Some(object), Some(text)) = (held.target.object, held.key.text.as_ref()) {
            if matches!(
                held.key.event,
                Event::Text(_)
                    | Event::Key {
                        key: Key::Space,
                        ..
                    }
            ) {
                if let Some((layout, record)) = self.option_set_keys(object) {
                    self.w
                        .document
                        .queue_option_key(layout, record, object, text.clone());
                    self.w.keys.released = true;
                    ctx.request_repaint();
                    return;
                }
            }
        }
        let viewport = ctx.viewport_id();
        let navigation = matches!(
            held.key.event,
            Event::Key {
                key: Key::Tab | Key::Enter | Key::Escape,
                ..
            }
        )
        .then_some((held.owner, held.target));
        let event = held.key.event;
        if ctx
            .with_plugin(|gate: &mut KeyGate| gate.release(viewport, event))
            .is_some()
        {
            self.w.keys.released = true;
            self.w.keys.navigation = navigation;
        }
    }

    /// The window's held and waiting keys go (the connection was lost).
    pub(super) fn drop_keystrokes(&mut self) {
        self.w.keys = Keys::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn seen(event: Event, mac: bool) -> Option<(Option<String>, i64)> {
        keystroke(&event, Modifiers::NONE, mac, false).map(|k| (k.text, k.modifiers))
    }

    #[test]
    fn navigational_keys_are_the_code_tables_control_characters() {
        for (k, code) in [
            (Key::Backspace, 8),
            (Key::Tab, 9),
            (Key::Enter, 13),
            (Key::Escape, 27),
            (Key::ArrowLeft, 28),
            (Key::ArrowUp, 29),
            (Key::ArrowRight, 30),
            (Key::ArrowDown, 31),
            (Key::Delete, 127),
        ] {
            let (text, modifiers) = seen(key(k, Modifiers::NONE), false).unwrap();
            assert_eq!(
                text.unwrap().chars().map(u32::from).collect::<Vec<_>>(),
                [code]
            );
            assert_eq!(modifiers, 0);
        }
        // Shift-Tab is Tab; the Shift key is in the modifiers.
        assert_eq!(
            seen(key(Key::Tab, Modifiers::SHIFT), false),
            Some((Some("\t".into()), 1))
        );
        // Option-Shift on macOS is 9 (the Get ( TriggerModifierKeys ) example).
        assert_eq!(
            seen(
                key(Key::ArrowLeft, Modifiers::SHIFT.plus(Modifiers::ALT)),
                true
            ),
            Some((Some("\u{1c}".into()), 9))
        );
        // Typed characters, with the modifiers down when they were typed.
        assert_eq!(
            keystroke(&Event::Text("A".into()), Modifiers::SHIFT, false, false)
                .map(|k| (k.text, k.modifiers)),
            Some((Some("A".into()), 1))
        );
        // Keys without a code reach the editor untouched, unless keys wait.
        assert!(seen(key(Key::Home, Modifiers::NONE), false).is_none());
        assert!(keystroke(
            &key(Key::End, Modifiers::NONE),
            Modifiers::NONE,
            false,
            true
        )
        .is_some_and(|k| k.text.is_none()));
        assert!(seen(key(Key::A, Modifiers::NONE), false).is_none());
    }

    #[test]
    fn keyboard_shortcuts_do_not_activate_keystroke_triggers() {
        // macOS: Command combinations; Control ones do (except FileMaker's
        // own Ctrl-Up and Ctrl-Down).
        assert!(seen(key(Key::Backspace, Modifiers::MAC_CMD), true).is_none());
        assert!(seen(key(Key::Enter, Modifiers::COMMAND), true).is_none());
        assert_eq!(
            seen(key(Key::ArrowLeft, Modifiers::CTRL), true),
            Some((Some("\u{1c}".into()), 4))
        );
        assert!(seen(key(Key::ArrowDown, Modifiers::CTRL), true).is_none());
        // Windows (and Linux): Alt and Ctrl combinations.
        assert!(seen(key(Key::ArrowLeft, Modifiers::CTRL), false).is_none());
        assert!(seen(key(Key::Backspace, Modifiers::ALT), false).is_none());
        // Key releases are not keystrokes.
        let mut release = key(Key::Backspace, Modifiers::NONE);
        if let Event::Key { pressed, .. } = &mut release {
            *pressed = false;
        }
        assert!(seen(release, false).is_none());
    }

    fn input(viewport: ViewportId, events: Vec<Event>) -> egui::RawInput {
        egui::RawInput {
            viewport_id: viewport,
            events,
            ..Default::default()
        }
    }

    #[test]
    fn the_gate_takes_armed_keys_and_gives_released_ones_back_first() {
        use egui::plugin::Plugin as _;
        let ctx = egui::Context::default();
        let mut gate = KeyGate::default();
        let root = ViewportId::ROOT;
        // Not armed: nothing is taken.
        let mut raw = input(root, vec![key(Key::Backspace, Modifiers::NONE)]);
        gate.input_hook(&ctx, &mut raw);
        assert_eq!(raw.events.len(), 1);
        gate.arm(root, 7, false, false, false);
        let typed = vec![
            Event::Text("a".into()),
            key(Key::Escape, Modifiers::NONE),
            key(Key::Backspace, Modifiers::COMMAND),
        ];
        let mut raw = input(root, typed.clone());
        gate.input_hook(&ctx, &mut raw);
        // Escape stays for a running script; the shortcut stays.
        assert_eq!(raw.events, typed[1..]);
        assert!(gate.take(root, 8).is_empty());
        let taken = gate.take(root, 7);
        assert_eq!(taken.len(), 1);
        gate.release(root, taken[0].event.clone());
        let mut raw = input(root, vec![Event::Text("b".into())]);
        gate.input_hook(&ctx, &mut raw);
        assert_eq!(
            raw.events,
            [Event::Text("a".into()), Event::Text("b".into())]
        );
    }

    /// Window 2 (behind window 1) had "!" typed into its Name editor
    /// `editor`, which held it for a trigger.
    fn held_in_window_2(app: &mut OpenMakerApp, ctx: &egui::Context, editor: egui::Id) -> Held {
        ctx.memory_mut(|m| m.request_focus(editor));
        app.with_window(2, |app| {
            app.w.document.active = Some("Name".into());
            let target = app.key_target(ctx);
            assert_eq!(target.editor, Some(editor));
            Held::Key(Box::new(HeldKey {
                key: keystroke(&Event::Text("!".into()), Modifiers::NONE, false, false).unwrap(),
                owner: app.key_owner(),
                target,
                stages: Vec::new(),
            }))
        })
        .unwrap()
    }

    fn released(ctx: &egui::Context) -> Vec<Event> {
        ctx.with_plugin(|gate: &mut KeyGate| gate.released(ViewportId::ROOT))
            .unwrap()
    }

    #[test]
    fn a_let_through_key_goes_once_to_the_editor_it_was_typed_into() {
        let ctx = egui::Context::default();
        ctx.add_plugin(KeyGate::default());
        let mut app = crate::app::tests::two_record_windows();
        let editor = egui::Id::unique("window 2 Name");
        let held = held_in_window_2(&mut app, &ctx, editor);
        app.resume_held(&ctx, 2, held, true, None);
        // In its own window, not the one in front.
        assert!(app.w.keys.approved.is_none());
        assert!(app.windows[0].keys.approved.is_some());
        app.with_window(2, |app| app.settle_keystrokes(&ctx, true));
        assert_eq!(released(&ctx), [Event::Text("!".into())]);
        // Applied in the next pass: never given back again.
        app.with_window(2, |app| app.settle_keystrokes(&ctx, true));
        app.with_window(2, |app| app.settle_keystrokes(&ctx, true));
        assert_eq!(released(&ctx).len(), 1);
        assert!(!app.windows[0].keys.released);
    }

    /// What changes in the app while a key's trigger runs.
    type Change = fn(&mut OpenMakerApp, &egui::Context);

    #[test]
    fn a_stopped_or_stale_key_reaches_no_editor() {
        let editor = egui::Id::unique("window 2 Name");
        let stale: [(&str, Change); 5] = [
            // Exit Script [ False ].
            ("vetoed", |_, _| {}),
            ("record changed", |app, _| {
                app.windows[0].session.as_mut().unwrap().current = 1;
            }),
            ("window closed, its number reused", |app, _| {
                app.windows[0].uuid = "another window".into();
            }),
            ("field left", |app, _| app.windows[0].document.active = None),
            ("editor lost the keyboard", |_, ctx| {
                ctx.memory_mut(|m| m.request_focus(egui::Id::unique("record number box")));
            }),
        ];
        for (case, change) in stale {
            let ctx = egui::Context::default();
            ctx.add_plugin(KeyGate::default());
            let mut app = crate::app::tests::two_record_windows();
            let held = held_in_window_2(&mut app, &ctx, editor);
            let proceed = case != "vetoed";
            if case == "editor lost the keyboard" {
                app.resume_held(&ctx, 2, held, proceed, None);
                change(&mut app, &ctx);
            } else {
                change(&mut app, &ctx);
                app.resume_held(&ctx, 2, held, proceed, None);
            }
            app.with_window(2, |app| app.settle_keystrokes(&ctx, true));
            assert!(released(&ctx).is_empty(), "{case}");
            assert!(app.windows[0].keys.approved.is_none(), "{case}");
            assert!(!app.windows[0].keys.released, "{case}");
        }
    }

    #[test]
    fn a_release_for_a_viewport_that_was_not_drawn_since_goes() {
        use egui::plugin::Plugin as _;
        let ctx = egui::Context::default();
        let mut gate = KeyGate::default();
        let other = ViewportId::from_hash_of("window 2");
        gate.input_hook(&ctx, &mut input(ViewportId::ROOT, Vec::new()));
        gate.release(other, Event::Text("a".into()));
        gate.arm(other, 2, true, false, false);
        // Two root passes go by without the window's viewport.
        gate.input_hook(&ctx, &mut input(ViewportId::ROOT, Vec::new()));
        gate.input_hook(&ctx, &mut input(ViewportId::ROOT, Vec::new()));
        let mut raw = input(other, vec![key(Key::Tab, Modifiers::NONE)]);
        gate.input_hook(&ctx, &mut raw);
        assert_eq!(raw.events, [key(Key::Tab, Modifiers::NONE)]);
        assert!(gate.take(other, 2).is_empty());
    }
}
