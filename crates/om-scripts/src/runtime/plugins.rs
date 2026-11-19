//! A bounded, clean-room subset of one third-party plug-in's commands: the
//! MBS FileMaker Plugin (plug-in id `MBSP`, function 1, `MBS ( command ;
//! ... )`). Only what a script needs to run a script after an idle period,
//! ask with a simple alert, and make a folder on the desktop:
//!
//! - `FM.RunScriptIdle` and `FM.StopScriptIdle`;
//! - `Dialog.Reset`, `Dialog.SetMessage`, `Dialog.SetMode`,
//!   `Dialog.SetTimeout`, `Dialog.SetDefaultButton`,
//!   `Dialog.SetAlternateButton` and `Dialog.Run`;
//! - `Folders.UserDesktop`, `Files.CreateDirectory`, `Files.Delete` and
//!   `Path.FileMakerPathToNativePath`, through the host's own file
//!   operations (`super::host::Files`);
//! - DynaPDF's commands that make a document of pages of other PDFs and
//!   save it (`super::dynapdf`), answered by OpenMaker's own engine.
//!
//! The behavior is taken from the plug-in's public documentation and marked
//! there as documented or inferred (docs/re/mbs-plugin.md); it has not been
//! compared with the plug-in itself. No plug-in binary is loaded. Every
//! other command, and these where the host keeps no plug-in state (the
//! server, layout calculations), evaluate as they do without the plug-in:
//! [`call`] answers `None` and the caller's default applies. So does
//! `Files.CreateDirectory`, `Files.Delete` or a DynaPDF command in a
//! calculation outside a script's step, which never acts on files.

use std::time::Duration;

use om_calc::{CalcError, Container, Ctx, Value, ValueType};
use om_formula::PluginFunctionId;

use super::data_files::code::{CANNOT_CREATE, FILE_MISSING, INVALID_PATH, IN_USE};
use super::host::{FileId, HostError, HostResult, ScriptHost, WindowId};

/// The plug-in's four-character id, as calculations store it
/// (calc-encoding.md 7).
pub const MBS_PLUGIN: [u8; 4] = *b"MBSP";
/// Its single function, `MBS`, whose first argument names the command.
pub const MBS_FUNCTION: u16 = 1;

/// What the plug-in keeps between calls: the alert being built by the
/// `Dialog.*` commands. One per host session, as the plug-in keeps one per
/// process.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginState {
    pub dialog: DialogOptions,
    /// DynaPDF's open documents.
    pub pdf: super::dynapdf::Documents,
}

/// The `Dialog.*` options. `Dialog.Reset` restores the default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DialogOptions {
    pub message: String,
    /// `Dialog.SetMode`'s text, as given (`Warning`, `Information`, ...).
    pub mode: String,
    /// `Dialog.SetTimeout`, in seconds; none when 0 or unset.
    pub timeout: Option<f64>,
    /// `Dialog.SetDefaultButton`; empty is the localized "OK".
    pub default_button: String,
    /// `Dialog.SetAlternateButton`; empty is no alternate button.
    pub alternate_button: String,
}

/// The alert `Dialog.Run` shows.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginDialog {
    pub message: String,
    pub mode: AlertMode,
    /// The buttons, the default first. Their labels are what `Dialog.Run`
    /// returns.
    pub buttons: Vec<String>,
    pub timeout: Option<Duration>,
}

/// The alert's icon, from `Dialog.SetMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertMode {
    /// No mode, or one this subset does not know.
    Plain,
    /// `Information` or `Note`.
    Note,
    /// `Warning`.
    Caution,
    /// `Critical`, `Error` or `Stop`.
    Stop,
}

impl AlertMode {
    fn of(mode: &str) -> Self {
        match mode.trim().to_ascii_lowercase().as_str() {
            "information" | "note" => Self::Note,
            "warning" => Self::Caution,
            "critical" | "error" | "stop" => Self::Stop,
            _ => Self::Plain,
        }
    }
}

/// How the user answered a [`PluginDialog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginDialogAnswer {
    /// The button at this index of [`PluginDialog::buttons`].
    Button(usize),
    /// The timeout closed it.
    Timeout,
}

/// A script `FM.RunScriptIdle` scheduled: run it in `file`'s window
/// `window` (the caller's) once nobody has used the app for `delay`
/// seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct IdleScript {
    pub file: FileId,
    /// The window whose script scheduled it: the idle script runs there,
    /// with that window's layout and record.
    pub window: Option<WindowId>,
    pub script: u64,
    /// `Get ( ScriptParameter )` in the idle script.
    pub parameter: Option<String>,
    pub delay: f64,
}

/// What a host gives the plug-in. A host without plug-in state answers
/// `None` from [`Self::plugin_state`], and the commands then evaluate as
/// without the plug-in.
pub trait PluginHost {
    fn plugin_state(&mut self) -> Option<&mut PluginState>;
    /// Show the alert and wait for the answer.
    fn show_plugin_dialog(&mut self, dialog: &PluginDialog) -> HostResult<PluginDialogAnswer>;
    /// Schedule the idle script, replacing any, or cancel it (`None`).
    fn schedule_idle_script(&mut self, idle: Option<IdleScript>) -> HostResult;
    fn plugin_current_file(&mut self) -> FileId;
    fn plugin_current_window(&mut self) -> Option<WindowId>;
    /// The calling file's name, `Get ( FileName )`.
    fn plugin_file_name(&mut self) -> String;
    fn plugin_script_named(&mut self, file: FileId, name: &str) -> Option<u64>;
    fn plugin_script_exists(&mut self, file: FileId, id: u64) -> bool;
    /// The user's desktop folder, a native path, where there is one.
    fn plugin_desktop_folder(&mut self) -> Option<String>;
    /// Create the folder at native path `path` with the folders above it.
    fn plugin_create_folder(&mut self, path: &str) -> HostResult;
    /// The bytes of the file at native path `path`, refused past `limit`.
    fn plugin_read_file(&mut self, path: &str, limit: u64) -> HostResult<Vec<u8>>;
    /// Replace the file at native path `path` with `data`.
    fn plugin_write_file(&mut self, path: &str, data: &[u8]) -> HostResult;
    /// A container value's data, loaded from storage when the value only
    /// names it.
    fn plugin_container_data(&mut self, container: &Container) -> Option<Vec<u8>>;
    /// The native path of a FileMaker path list's line for this platform.
    fn plugin_native_path(&mut self, path: &str, pass_through: bool) -> HostResult<String>;
    /// Delete the file at native path `path`.
    fn plugin_delete_file(&mut self, path: &str) -> HostResult;
    /// Lay out a DynaPDF text block, drawing nothing.
    fn plugin_check_pdf_text(&mut self, block: &super::dynapdf_text::TextBlock) -> HostResult;
    /// A PDF of one page per entry of `pages`, its text drawn, for DynaPDF
    /// pages.
    fn plugin_render_pdf_text(
        &mut self,
        pages: &[&[super::dynapdf_text::TextBlock]],
    ) -> HostResult<Vec<u8>>;
}

/// A script host gives the plug-in what it keeps: the default keeps no
/// plug-in state, so the server's and the tests' memory hosts evaluate the
/// commands as without the plug-in.
impl<T: ScriptHost + ?Sized> PluginHost for T {
    fn plugin_state(&mut self) -> Option<&mut PluginState> {
        ScriptHost::plugin_state(self)
    }
    fn show_plugin_dialog(&mut self, dialog: &PluginDialog) -> HostResult<PluginDialogAnswer> {
        super::host::Dialogs::show_plugin_dialog(self, dialog)
    }
    fn schedule_idle_script(&mut self, idle: Option<IdleScript>) -> HostResult {
        ScriptHost::schedule_idle_script(self, idle)
    }
    fn plugin_current_file(&mut self) -> FileId {
        self.current_file()
    }
    fn plugin_current_window(&mut self) -> Option<WindowId> {
        self.current_window()
    }
    fn plugin_file_name(&mut self) -> String {
        /// `Get ( FileName )`'s selector (calc-encoding.md 7).
        const FILE_NAME: u8 = 5;
        let context = self.calc_context();
        let locale = self.locale().clone();
        let cx = Ctx::new(&locale, self.today());
        self.get(FILE_NAME, &context)
            .map(|v| v.to_text(&cx).into_string())
            .unwrap_or_default()
    }
    fn plugin_script_named(&mut self, file: FileId, name: &str) -> Option<u64> {
        self.script_named(file, name)
    }
    fn plugin_script_exists(&mut self, file: FileId, id: u64) -> bool {
        self.script(file, id).is_some()
    }
    fn plugin_desktop_folder(&mut self) -> Option<String> {
        super::host::Files::desktop_folder(self)
    }
    fn plugin_create_folder(&mut self, path: &str) -> HostResult {
        super::host::Files::create_folder(self, path)
    }
    fn plugin_read_file(&mut self, path: &str, limit: u64) -> HostResult<Vec<u8>> {
        super::host::Files::read_file(self, path, limit)
    }
    fn plugin_write_file(&mut self, path: &str, data: &[u8]) -> HostResult {
        super::host::Files::write_file(self, path, data)
    }
    fn plugin_container_data(&mut self, container: &Container) -> Option<Vec<u8>> {
        if !container.streams.is_empty() {
            return Some(container.data().to_vec());
        }
        let context = self.calc_context();
        self.container_data(container, &context)
            .ok()
            .map(|stored| stored.data.data().to_vec())
    }
    fn plugin_native_path(&mut self, path: &str, pass_through: bool) -> HostResult<String> {
        super::host::Files::native_path(self, path, pass_through)
    }
    fn plugin_delete_file(&mut self, path: &str) -> HostResult {
        super::host::Files::delete_native_file(self, path)
    }
    fn plugin_check_pdf_text(&mut self, block: &super::dynapdf_text::TextBlock) -> HostResult {
        super::host::Files::check_pdf_text(self, block)
    }
    fn plugin_render_pdf_text(
        &mut self,
        pages: &[&[super::dynapdf_text::TextBlock]],
    ) -> HostResult<Vec<u8>> {
        super::host::Files::render_pdf_text(self, pages)
    }
}

/// What the plug-in returns on success.
pub(super) const OK: &str = "OK";
/// The longest alert timeout kept, a day (a longer one is none), and the
/// longest idle delay (a longer one is an error) [I: OpenMaker's bound, so
/// no deadline overflows].
const MAX_TIMEOUT: f64 = 86_400.0;
/// `Dialog.Run`'s answer when the timeout closed the alert.
const TIMEOUT: &str = "Timeout";

/// Evaluate plug-in function `id` with `args` (in source order), or `None`
/// when it is not one of this subset's commands or the host keeps no
/// plug-in state. `effects` is whether the calculation is a script step's,
/// which may act on files: outside a step (a button's parameter, a layout's
/// calculation) `Files.CreateDirectory` answers `None` too (OpenMaker's
/// choice; in FileMaker Pro any calculation may call the plug-in).
pub fn call<H: PluginHost + ?Sized>(
    host: &mut H,
    cx: &Ctx,
    id: PluginFunctionId,
    args: &[Value],
    effects: bool,
) -> Option<Result<Value, CalcError>> {
    if id.plugin != MBS_PLUGIN || id.function != MBS_FUNCTION {
        return None;
    }
    let command = args.first()?.to_text(cx).into_string();
    let command = Command::of(&command)?;
    host.plugin_state()?;
    let args = &args[1..];
    let text = |i: usize| args.get(i).map(|v| v.to_text(cx).into_string());
    let answer = match command {
        Command::RunScriptIdle => run_script_idle(host, cx, args),
        Command::StopScriptIdle => match host.schedule_idle_script(None) {
            Ok(()) => OK.to_owned(),
            Err(_) => error("Failed to stop the idle script."),
        },
        Command::Reset => {
            host.plugin_state()?.dialog = DialogOptions::default();
            OK.to_owned()
        }
        Command::SetMessage => {
            host.plugin_state()?.dialog.message = text(0).unwrap_or_default();
            OK.to_owned()
        }
        Command::SetMode => {
            host.plugin_state()?.dialog.mode = text(0).unwrap_or_default();
            OK.to_owned()
        }
        Command::SetTimeout => {
            let seconds = args.first().and_then(|v| v.to_number(cx).to_f64());
            host.plugin_state()?.dialog.timeout = seconds.filter(|s| *s > 0.0 && *s <= MAX_TIMEOUT);
            OK.to_owned()
        }
        Command::SetDefaultButton => {
            host.plugin_state()?.dialog.default_button = text(0).unwrap_or_default();
            OK.to_owned()
        }
        Command::SetAlternateButton => {
            host.plugin_state()?.dialog.alternate_button = text(0).unwrap_or_default();
            OK.to_owned()
        }
        Command::Run => run_dialog(host)?,
        Command::UserDesktop => host
            .plugin_desktop_folder()
            .unwrap_or_else(|| error("There is no desktop folder.")),
        Command::CreateDirectory if !effects => return None,
        Command::CreateDirectory => match text(0) {
            Some(path) => create_directory(host, &path),
            None => error("Missing parameters."),
        },
        Command::DeleteFile if !effects => return None,
        Command::DeleteFile => match text(0) {
            Some(paths) => delete_files(host, &paths),
            None => error("Missing parameters."),
        },
        Command::NativePath => match text(0) {
            Some(path) => {
                // PassThroughNativePath: "Default is 1" [O, docs].
                let pass_through = args
                    .get(1)
                    .is_none_or(|v| v.to_number(cx).to_f64() != Some(0.0));
                match host.plugin_native_path(&path, pass_through) {
                    Ok(native) => native,
                    Err(HostError::Error(code)) if code == INVALID_PATH => error("Invalid path."),
                    Err(_) => error("No path for this platform."),
                }
            }
            None => error("Missing parameters."),
        },
        Command::ReplaceNewline => {
            // Mode and Wrap are whole numbers as FileMaker compares numbers
            // (at their display precision); an empty Wrap is 0, any other
            // Wrap that is not 0, text that is no number included, refused.
            let number = |i: usize| {
                args.get(i)
                    .map(|v| v.to_number(cx))
                    .filter(|n| !n.is_empty())
            };
            let wrap = match args.get(2) {
                None => Some(0),
                Some(v) if v.to_text(cx).into_string().is_empty() => Some(0),
                Some(v) => {
                    let n = v.to_number(cx);
                    if n.is_empty() {
                        None
                    } else {
                        whole(&n)
                    }
                }
            };
            match (text(0), number(1)) {
                (Some(text), Some(mode)) => match (whole(&mode), wrap) {
                    (_, Some(wrap)) if wrap != 0 => error("OpenMaker does not wrap lines."),
                    (_, None) => error("OpenMaker does not wrap lines."),
                    (Some(mode), Some(_)) => replace_newline(&text, mode).unwrap_or_else(error),
                    (None, Some(_)) => error("Invalid mode."),
                },
                _ => error("Missing parameters."),
            }
        }
        Command::Pdf(_) if !effects => return None,
        Command::Pdf(command) => return Some(Ok(super::dynapdf::call(host, cx, command, args))),
    };
    Some(Ok(Value::text(answer)))
}

/// The commands of this subset; the plug-in's names are not case-sensitive
/// [I].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    RunScriptIdle,
    StopScriptIdle,
    Reset,
    SetMessage,
    SetMode,
    SetTimeout,
    SetDefaultButton,
    SetAlternateButton,
    Run,
    UserDesktop,
    CreateDirectory,
    DeleteFile,
    NativePath,
    ReplaceNewline,
    Pdf(super::dynapdf::Command),
}

impl Command {
    fn of(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        if let Some(command) = super::dynapdf::Command::of(&name) {
            return Some(Self::Pdf(command));
        }
        Some(match name.as_str() {
            "fm.runscriptidle" => Self::RunScriptIdle,
            "fm.stopscriptidle" => Self::StopScriptIdle,
            "dialog.reset" => Self::Reset,
            "dialog.setmessage" => Self::SetMessage,
            "dialog.setmode" => Self::SetMode,
            "dialog.settimeout" => Self::SetTimeout,
            "dialog.setdefaultbutton" => Self::SetDefaultButton,
            "dialog.setalternatebutton" => Self::SetAlternateButton,
            "dialog.run" => Self::Run,
            "folders.userdesktop" => Self::UserDesktop,
            "files.createdirectory" => Self::CreateDirectory,
            "files.delete" => Self::DeleteFile,
            "path.filemakerpathtonativepath" => Self::NativePath,
            // Its old name, String.ReplaceNewline, still answers [O, docs].
            "text.replacenewline" | "string.replacenewline" => Self::ReplaceNewline,
            _ => return None,
        })
    }
}

/// The plug-in's error text: `[MBS]` and the reason [I].
pub(super) fn error(reason: &str) -> String {
    format!("[MBS] {reason}")
}

/// `FM.RunScriptIdle ( DelaySeconds ; FileName ; ScriptName {;
/// ScriptParameter } )`: ScriptName may be the script's id instead [O,
/// documentation]. The script runs in the caller's window, so FileName
/// must be the calling file's own name (`Get ( FileName )`, compared
/// without case): OpenMaker refuses another file's script with an error
/// rather than open or look up another file (its choice; the plug-in runs
/// it). The script is resolved now, in the calling file.
fn run_script_idle<H: PluginHost + ?Sized>(host: &mut H, cx: &Ctx, args: &[Value]) -> String {
    let (Some(delay), Some(file_name), Some(script)) = (args.first(), args.get(1), args.get(2))
    else {
        return error("Missing parameters.");
    };
    let delay = match delay.to_number(cx).to_f64() {
        Some(seconds) if (0.0..=MAX_TIMEOUT).contains(&seconds) => seconds,
        _ => return error("Invalid delay."),
    };
    let file = host.plugin_current_file();
    let file_name = file_name.to_text(cx).into_string();
    if file_name.to_lowercase() != host.plugin_file_name().to_lowercase() {
        return error("OpenMaker runs only an idle script of the calling file.");
    }
    let script = if script.value_type() == ValueType::Number {
        match script.to_number(cx).to_i64() {
            Some(id) if id > 0 && host.plugin_script_exists(file, id as u64) => id as u64,
            _ => return error("Script not found."),
        }
    } else {
        let name = script.to_text(cx).into_string();
        match host.plugin_script_named(file, &name) {
            Some(id) => id,
            None => return error(&format!("Script not found: {name}")),
        }
    };
    let idle = IdleScript {
        file,
        window: host.plugin_current_window(),
        script,
        parameter: args.get(3).map(|v| v.to_text(cx).into_string()),
        delay,
    };
    match host.schedule_idle_script(Some(idle)) {
        Ok(()) => OK.to_owned(),
        Err(_) => error("Failed to schedule the idle script."),
    }
}

/// `Files.CreateDirectory ( Path )`: "Creates a directory" at a "Native
/// path", "recursively as needed", and "Returns "OK" if the directory
/// already exists" [O, docs]. What the host refuses (a relative or invalid
/// path, a file in the way) is an error [I: the documentation gives no
/// error texts].
fn create_directory<H: PluginHost + ?Sized>(host: &mut H, path: &str) -> String {
    match host.plugin_create_folder(path) {
        Ok(()) => OK.to_owned(),
        Err(HostError::Error(code)) if code == INVALID_PATH => error("Invalid path."),
        Err(_) => error("Failed to create directory."),
    }
}

/// `Files.Delete ( Path )`: "Deletes a file directly without moving it to
/// trash"; several paths, one per line, are deleted in turn and the first
/// error "Stops" it; a path ending in `//` or `\\` is refused [O, docs].
/// OpenMaker deletes files only: the plug-in deletes a folder with what it
/// holds on macOS and Windows, and no folder on Linux; OpenMaker refuses a
/// folder everywhere (its choice).
fn delete_files<H: PluginHost + ?Sized>(host: &mut H, paths: &str) -> String {
    let lines: Vec<&str> = paths
        .split(['\r', '\n'])
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return error("Missing parameters.");
    }
    for line in lines {
        if line.ends_with("//") || line.ends_with("\\\\") {
            return error("Invalid path.");
        }
        match host.plugin_delete_file(line) {
            Ok(()) => {}
            Err(HostError::Error(code)) => {
                return error(match code {
                    c if c == INVALID_PATH => "Invalid path.",
                    c if c == FILE_MISSING => "File not found.",
                    c if c == IN_USE => "The file is in use.",
                    c if c == CANNOT_CREATE => "OpenMaker deletes files, not folders.",
                    _ => "Failed to delete the file.",
                })
            }
            Err(_) => return error("Failed to delete the file."),
        }
    }
    OK.to_owned()
}

/// `Dialog.Run`: the alert the options describe; the label of the button
/// chosen, or `Timeout` [O, documentation: "On Mac returns text of the
/// buttons clicked"; "The result of the dialog will be Timeout"].
fn run_dialog<H: PluginHost + ?Sized>(host: &mut H) -> Option<String> {
    let options = host.plugin_state()?.dialog.clone();
    let default = if options.default_button.is_empty() {
        "OK".to_owned()
    } else {
        options.default_button
    };
    let mut buttons = vec![default];
    if !options.alternate_button.is_empty() {
        buttons.push(options.alternate_button);
    }
    let dialog = PluginDialog {
        message: options.message,
        mode: AlertMode::of(&options.mode),
        timeout: options
            .timeout
            .and_then(|s| Duration::try_from_secs_f64(s).ok()),
        buttons,
    };
    Some(match host.show_plugin_dialog(&dialog) {
        Ok(PluginDialogAnswer::Button(i)) => dialog.buttons.get(i).cloned().unwrap_or_default(),
        Ok(PluginDialogAnswer::Timeout) => TIMEOUT.to_owned(),
        Err(_) => error("The dialog could not be shown."),
    })
}

/// `n` as a whole number, when it is one as FileMaker's `=` compares
/// numbers, at their display precision (decimal places): `( 1 / 3 ) * 3`,
/// which holds 0.999... beyond them, is 1; `2.5` and
/// `13.0000000000000001` are none. `None` for `?` and fractions.
fn whole(n: &om_calc::Number) -> Option<i64> {
    // `Int` rounds to the display precision before it truncates.
    let int = n.int().to_i64()?;
    (n.compare(&om_calc::Number::from_i64(int)) == Some(std::cmp::Ordering::Equal)).then_some(int)
}

/// `Text.ReplaceNewLine ( Text ; Mode {; Wrap } )` [O, docs]: each line
/// break of `text`, up to its first NUL, as Mode 0 none, 1 CR, 2 LF, 3
/// CRLF, 4 `<br>` or 5 `<br />`; 8 added removes duplicate line breaks.
/// [I] A line break is CRLF, CR or LF, and duplicates are line breaks in a
/// row. Wrapping is not documented beyond its length, so the caller
/// refuses a Wrap other than 0.
fn replace_newline(text: &str, mode: i64) -> Result<String, &'static str> {
    if !matches!(mode, 0..=5 | 8..=13) {
        return Err("Invalid mode.");
    }
    let mode = mode as u8;
    let newline = match mode & 7 {
        0 => "",
        1 => "\r",
        2 => "\n",
        3 => "\r\n",
        4 => "<br>",
        _ => "<br />",
    };
    let text = text.split('\0').next().unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut after_break = false;
    while let Some(ch) = chars.next() {
        if ch == '\r' || ch == '\n' {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            if !(mode & 8 != 0 && after_break) {
                out.push_str(newline);
            }
            after_break = true;
        } else {
            out.push(ch);
            after_break = false;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_calc::{Date, Locale};

    /// A host with one file of two scripts, answering dialogs from a queue.
    #[derive(Default)]
    struct Host {
        state: Option<PluginState>,
        idle: Vec<Option<IdleScript>>,
        shown: Vec<PluginDialog>,
        answers: Vec<PluginDialogAnswer>,
    }

    impl PluginHost for Host {
        fn plugin_state(&mut self) -> Option<&mut PluginState> {
            self.state.as_mut()
        }
        fn show_plugin_dialog(&mut self, dialog: &PluginDialog) -> HostResult<PluginDialogAnswer> {
            self.shown.push(dialog.clone());
            Ok(self.answers.remove(0))
        }
        fn schedule_idle_script(&mut self, idle: Option<IdleScript>) -> HostResult {
            self.idle.push(idle);
            Ok(())
        }
        fn plugin_current_file(&mut self) -> FileId {
            7
        }
        fn plugin_current_window(&mut self) -> Option<WindowId> {
            Some(3)
        }
        fn plugin_file_name(&mut self) -> String {
            "Sample".into()
        }
        fn plugin_script_named(&mut self, file: FileId, name: &str) -> Option<u64> {
            (file == 7 && name == "When Idle").then_some(21)
        }
        fn plugin_script_exists(&mut self, file: FileId, id: u64) -> bool {
            file == 7 && id == 21
        }
        fn plugin_desktop_folder(&mut self) -> Option<String> {
            None
        }
        fn plugin_create_folder(&mut self, _path: &str) -> HostResult {
            Err(HostError::Unsupported)
        }
        fn plugin_read_file(&mut self, _path: &str, _limit: u64) -> HostResult<Vec<u8>> {
            Err(HostError::Unsupported)
        }
        fn plugin_write_file(&mut self, _path: &str, _data: &[u8]) -> HostResult {
            Err(HostError::Unsupported)
        }
        fn plugin_container_data(&mut self, _container: &Container) -> Option<Vec<u8>> {
            None
        }
        fn plugin_native_path(&mut self, _path: &str, _pass_through: bool) -> HostResult<String> {
            Err(HostError::Unsupported)
        }
        fn plugin_delete_file(&mut self, _path: &str) -> HostResult {
            Err(HostError::Unsupported)
        }
        fn plugin_check_pdf_text(
            &mut self,
            _block: &crate::runtime::dynapdf_text::TextBlock,
        ) -> HostResult {
            Err(HostError::Unsupported)
        }
        fn plugin_render_pdf_text(
            &mut self,
            _pages: &[&[crate::runtime::dynapdf_text::TextBlock]],
        ) -> HostResult<Vec<u8>> {
            Err(HostError::Unsupported)
        }
    }

    const MBS: PluginFunctionId = PluginFunctionId {
        plugin: MBS_PLUGIN,
        function: MBS_FUNCTION,
    };

    /// The text `MBS ( args )` returns in a step, or `None` when it falls
    /// through.
    fn mbs(host: &mut Host, args: &[Value]) -> Option<String> {
        call(host, &cx(), MBS, args, true).map(|r| r.unwrap().to_text(&cx()).into_string())
    }

    fn text(s: &str) -> Value {
        Value::text(s)
    }

    fn enabled() -> Host {
        Host {
            state: Some(PluginState::default()),
            ..Host::default()
        }
    }

    #[test]
    fn the_idle_script_is_scheduled_in_the_callers_window_and_stopped() {
        let mut host = enabled();
        let ok = mbs(
            &mut host,
            &[
                text("FM.RunScriptIdle"),
                Value::number(7),
                text("Sample"),
                text("When Idle"),
            ],
        );
        assert_eq!(ok.as_deref(), Some("OK"));
        let idle = IdleScript {
            file: 7,
            window: Some(3),
            script: 21,
            parameter: None,
            delay: 7.0,
        };
        assert_eq!(host.idle, [Some(idle.clone())]);
        // By id, with a parameter, and the command in another case.
        let args = [
            text("fm.runscriptidle"),
            Value::number(2),
            text("sample"),
            Value::number(21),
            text("p"),
        ];
        assert_eq!(mbs(&mut host, &args).as_deref(), Some("OK"));
        let parameter = host.idle[1].as_ref().unwrap().parameter.as_deref();
        assert_eq!(parameter, Some("p"));
        let stop = mbs(&mut host, &[text("FM.StopScriptIdle")]);
        assert_eq!(stop.as_deref(), Some("OK"));
        assert_eq!(host.idle[2], None);
        // A script the file does not have, or a script of any other file,
        // schedules nothing.
        for (file, script) in [("Sample", "x"), ("Other", "When Idle")] {
            let args = [
                text("FM.RunScriptIdle"),
                Value::number(7),
                text(file),
                text(script),
            ];
            let answer = mbs(&mut host, &args).unwrap();
            assert!(answer.starts_with("[MBS] "), "{answer}");
        }
        assert_eq!(host.idle.len(), 3);
    }

    fn cx() -> Ctx<'static> {
        static LOCALE: std::sync::OnceLock<Locale> = std::sync::OnceLock::new();
        Ctx::new(
            LOCALE.get_or_init(Locale::en_us),
            Date::from_ymd(2026, 11, 17).unwrap(),
        )
    }

    #[test]
    fn the_dialog_returns_the_chosen_buttons_label_or_timeout() {
        let mut host = enabled();
        host.answers = vec![
            PluginDialogAnswer::Button(1),
            PluginDialogAnswer::Timeout,
            PluginDialogAnswer::Button(0),
        ];
        let set =
            |host: &mut Host, args: &[Value]| assert_eq!(mbs(host, args).as_deref(), Some("OK"));
        set(&mut host, &[text("Dialog.Reset")]);
        set(&mut host, &[text("Dialog.SetMessage"), text("Keep it?")]);
        set(&mut host, &[text("Dialog.SetMode"), text("Warning")]);
        set(&mut host, &[text("Dialog.SetTimeout"), Value::number(10)]);
        set(&mut host, &[text("Dialog.SetDefaultButton"), text("Keep")]);
        set(
            &mut host,
            &[text("Dialog.SetAlternateButton"), text("Drop")],
        );
        assert_eq!(
            mbs(&mut host, &[text("Dialog.Run")]).as_deref(),
            Some("Drop")
        );
        assert_eq!(
            host.shown[0],
            PluginDialog {
                message: "Keep it?".into(),
                mode: AlertMode::Caution,
                buttons: vec!["Keep".into(), "Drop".into()],
                timeout: Some(Duration::from_secs(10)),
            }
        );
        assert_eq!(
            mbs(&mut host, &[text("Dialog.Run")]).as_deref(),
            Some("Timeout")
        );
        // A timeout past the bound is none.
        set(&mut host, &[text("Dialog.SetTimeout"), Value::text("1e30")]);
        assert_eq!(host.plugin_state().unwrap().dialog.timeout, None);
        // Reset: the localized OK alone, no timeout.
        set(&mut host, &[text("Dialog.Reset")]);
        assert_eq!(mbs(&mut host, &[text("Dialog.Run")]).as_deref(), Some("OK"));
        assert_eq!(host.shown[2].buttons, ["OK"]);
        assert_eq!(host.shown[2].timeout, None);
    }

    #[test]
    fn line_breaks_are_replaced_by_the_modes_and_duplicates_removed() {
        let mut host = enabled();
        let replace = |host: &mut Host, input: &str, mode: i64| {
            mbs(
                host,
                &[
                    text("Text.ReplaceNewLine"),
                    text(input),
                    Value::number(mode),
                ],
            )
        };
        let input = "a\rb\r\nc\n\n\rd";
        let rows = [
            (0, "abcd"),
            (1, "a\rb\rc\r\r\rd"),
            (2, "a\nb\nc\n\n\nd"),
            (3, "a\r\nb\r\nc\r\n\r\n\r\nd"),
            (4, "a<br>b<br>c<br><br><br>d"),
            (5, "a<br />b<br />c<br /><br /><br />d"),
            (2 + 8, "a\nb\nc\nd"),
        ];
        for (mode, expected) in rows {
            assert_eq!(
                replace(&mut host, input, mode).as_deref(),
                Some(expected),
                "{mode}"
            );
        }
        // The old name; output stops at the first NUL; other modes and a
        // wrap are refused.
        assert_eq!(
            mbs(
                &mut host,
                &[
                    text("String.ReplaceNewline"),
                    text("x\0\ry"),
                    Value::number(2)
                ]
            )
            .as_deref(),
            Some("x")
        );
        assert_eq!(
            replace(&mut host, input, 6).as_deref(),
            Some("[MBS] Invalid mode.")
        );
        // A fraction is no mode, and a Wrap that is not a number is not 0;
        // a mode that is whole at the display precision is one.
        let number = |text: &str| om_calc::Number::parse_pure_text(text, '.', ',').unwrap();
        for fraction in ["2.5", "13.0000000000000001"] {
            assert_eq!(
                mbs(
                    &mut host,
                    &[
                        text("Text.ReplaceNewLine"),
                        text(input),
                        Value::Number(number(fraction))
                    ]
                )
                .as_deref(),
                Some("[MBS] Invalid mode."),
                "{fraction}"
            );
        }
        // ( 1 / 3 ) * 3 holds 0.999... past its display precision, where
        // FileMaker compares it as 1: Mode 1.
        let third = om_calc::Number::from_i64(1)
            .div(&om_calc::Number::from_i64(3), 16)
            .unwrap();
        let one = third.mul(&om_calc::Number::from_i64(3), 16);
        assert_eq!(
            mbs(
                &mut host,
                &[
                    text("Text.ReplaceNewLine"),
                    text("a\nb"),
                    Value::Number(one)
                ]
            )
            .as_deref(),
            Some("a\rb")
        );
        for wrap in ["?", "abc"] {
            assert_eq!(
                mbs(
                    &mut host,
                    &[
                        text("Text.ReplaceNewLine"),
                        text(input),
                        Value::number(2),
                        text(wrap)
                    ]
                )
                .as_deref(),
                Some("[MBS] OpenMaker does not wrap lines."),
                "{wrap}"
            );
        }
        assert_eq!(
            mbs(
                &mut host,
                &[
                    text("Text.ReplaceNewLine"),
                    text(input),
                    Value::number(2),
                    Value::number(80)
                ]
            )
            .as_deref(),
            Some("[MBS] OpenMaker does not wrap lines.")
        );
    }

    #[test]
    fn other_commands_and_hosts_without_plugin_state_fall_through() {
        let mut host = enabled();
        assert_eq!(mbs(&mut host, &[text("Screen.Count")]), None);
        assert_eq!(mbs(&mut host, &[]), None);
        let mut server = Host::default();
        assert_eq!(mbs(&mut server, &[text("Dialog.Reset")]), None);
        let other = PluginFunctionId {
            plugin: *b"XXXX",
            function: 1,
        };
        assert!(call(&mut host, &cx(), other, &[text("Dialog.Reset")], true).is_none());
    }
}
