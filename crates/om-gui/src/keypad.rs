//! The numeric keypad's Enter key, which FileMaker Pro tells apart from
//! Return. In Browse it commits the record (script-runtime.md 7.1: one
//! FileMaker Pro 26.0.3 observation on an ordinary Text field; Claris
//! "Committing data in records"), or goes to the next object from a field
//! whose "Go to next object using" includes the Enter key (layout-model.md
//! 5.4.1). Keystroke triggers see it as character 10, Return as 13
//! (OnObjectKeystroke's Code table).
//!
//! egui has one Enter key: egui-winit 0.37 gives both winit's
//! `KeyCode::Enter` and `KeyCode::NumpadEnter` the key `Key::Enter` and the
//! physical key `Key::Enter`. So the app's event loop ([`Keys`]) gives
//! keypad Enter's winit events another physical key, `F35`, before
//! egui-winit reads them. egui then delivers keypad Enter as
//! `Key::Enter` with the physical key [`MARK`]: every widget still reads it
//! as Enter (egui itself never reads an event's physical key), and
//! [`is_enter`] tells it apart. No keyboard has an F35 key whose logical
//! key is Enter, so nothing else carries that pair.

use egui::{Event, Key, Modifiers};

/// The physical key egui reports for the keypad's Enter in OpenMaker's
/// event loop.
const MARK: Key = Key::F35;

/// The event egui gets for the keypad's Enter pressed or released with
/// `modifiers`, as OpenMaker's event loop delivers it: for tests, which
/// have no event loop.
pub fn enter_event(pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key: Key::Enter,
        physical_key: Some(MARK),
        pressed,
        repeat: false,
        modifiers,
    }
}

/// Whether `event` is the keypad's Enter, pressed or released.
pub(crate) fn is_enter(event: &Event) -> bool {
    matches!(
        event,
        Event::Key {
            key: Key::Enter,
            physical_key: Some(MARK),
            ..
        }
    )
}

/// Whether `event` presses the keypad's Enter without Command (Ctrl on
/// Linux and Windows), which is Commit's own shortcut with either Enter.
pub(crate) fn is_enter_press(event: &Event) -> bool {
    is_enter(event)
        && matches!(
            event,
            Event::Key {
                pressed: true,
                modifiers,
                ..
            } if !modifiers.command && !modifiers.mac_cmd
        )
}

pub(crate) use event_loop::run;

mod event_loop {
    use std::sync::Mutex;

    use eframe::UserEvent;
    use winit::application::ApplicationHandler;
    use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::keyboard::{KeyCode, PhysicalKey};
    use winit::window::WindowId;

    /// Run eframe's app on an event loop of OpenMaker's own, as
    /// `eframe::run_native` runs it (`run_app_on_demand`, the event loop
    /// built with `options`' hook), with keypad Enter marked ([`Keys`]).
    pub(crate) fn run(
        app_name: &str,
        mut options: eframe::NativeOptions,
        app_creator: eframe::AppCreator<'_>,
    ) -> eframe::Result {
        use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;

        let mut builder = EventLoop::<UserEvent>::with_user_event();
        if let Some(hook) = options.event_loop_builder.take() {
            hook(&mut builder);
        }
        let mut event_loop = builder.build()?;
        catch_eframe_errors();
        let app = eframe::create_native(app_name, options, app_creator, &event_loop);
        event_loop.run_app_on_demand(&mut Keys(app))?;
        match EXIT_ERROR.lock().ok().and_then(|mut error| error.take()) {
            Some(error) => Err(eframe::Error::AppCreation(error.into())),
            None => Ok(()),
        }
    }

    /// eframe's handler, with keypad Enter's physical key made [`MARK`]'s
    /// (`KeyCode::F35`) in every window event before eframe reads it.
    ///
    /// [`MARK`]: super::MARK
    struct Keys<A>(A);

    impl<A: ApplicationHandler<UserEvent>> ApplicationHandler<UserEvent> for Keys<A> {
        fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
            self.0.new_events(event_loop, cause);
        }

        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.0.resumed(event_loop);
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
            self.0.user_event(event_loop, event);
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            window_id: WindowId,
            mut event: WindowEvent,
        ) {
            if let WindowEvent::KeyboardInput { event: key, .. } = &mut event {
                if key.physical_key == PhysicalKey::Code(KeyCode::NumpadEnter) {
                    key.physical_key = PhysicalKey::Code(KeyCode::F35);
                }
            }
            self.0.window_event(event_loop, window_id, event);
        }

        fn device_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            device_id: DeviceId,
            event: DeviceEvent,
        ) {
            self.0.device_event(event_loop, device_id, event);
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.0.about_to_wait(event_loop);
        }

        fn suspended(&mut self, event_loop: &ActiveEventLoop) {
            self.0.suspended(event_loop);
        }

        fn exiting(&mut self, event_loop: &ActiveEventLoop) {
            self.0.exiting(event_loop);
        }

        fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
            self.0.memory_warning(event_loop);
        }
    }

    /// The error that ended eframe's event loop, which `run` returns.
    static EXIT_ERROR: Mutex<Option<String>> = Mutex::new(None);

    /// How eframe 0.37 logs the error that ends its event loop
    /// (`WinitAppWrapper::handle_event_result`). `eframe::run_native` returns
    /// that error; the handler `eframe::create_native` gives keeps it, so
    /// [`EframeErrors`] reads it from the log.
    const EXIT_PREFIX: &str = "Exiting because of error: ";

    /// Keeps the error eframe logs as it ends its event loop. Nothing else
    /// is logged anywhere, as before (the app installs no other logger).
    struct EframeErrors;

    impl log::Log for EframeErrors {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() == log::Level::Error && metadata.target().starts_with("eframe")
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let message = record.args().to_string();
            if let Some(error) = message.strip_prefix(EXIT_PREFIX) {
                if let Ok(mut exit) = EXIT_ERROR.lock() {
                    *exit = Some(error.to_owned());
                }
            }
        }

        fn flush(&self) {}
    }

    /// Install [`EframeErrors`] unless the process has a logger already
    /// (an embedding program's), which then sees eframe's errors itself.
    fn catch_eframe_errors() {
        static LOGGER: EframeErrors = EframeErrors;
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Error);
        }
    }
}
