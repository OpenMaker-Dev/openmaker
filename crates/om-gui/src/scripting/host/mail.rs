//! Send Mail's handoff to the system's mail client. The `mailto:` URL goes
//! to the application the system registers for `mailto:`, never to a web
//! browser, and the step learns whether it took the message:
//!
//! - Linux and other freedesktop systems: the default handler of
//!   `x-scheme-handler/mailto` (`xdg-mime query default`), launched from
//!   its desktop entry's `Exec` line. `xdg-open` is not used: it exits 0
//!   when the handler fails, and with no handler it hands the whole draft
//!   to a web browser. No registered handler is a refusal.
//! - macOS: `open`, which exits non-zero when no application takes the
//!   URL.
//! - Windows: refused (3) for now: `url.dll`'s protocol handler reports no
//!   launch failure, and a reporting launcher is not built yet.
//!
//! The launched handler is waited for, up to [`HANDOFF`]: a zero exit (a
//! client already running took the message) is taken; a non-zero exit is
//! its refusal; one still running then has not confirmed the handoff, and
//! the step answers 3 (the owner's decision on #593), leaving the client
//! running: its compose window may still appear after the step returns.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a handler may take to say whether it took the message; one
/// that has not exited by then is unconfirmed.
const HANDOFF: Duration = Duration::from_secs(10);

/// Hand `url`, a `mailto:` URL, to the system's `mailto:` handler: Ok once
/// it took it, else why not.
pub(super) fn open_mailto(url: &str) -> Result<(), String> {
    if cfg!(windows) {
        return Err(
            "Sending mail through the email client is not supported on Windows yet.".into(),
        );
    }
    let argv = if cfg!(target_os = "macos") {
        vec!["open".to_owned(), url.to_owned()]
    } else {
        freedesktop_argv(url)?
    };
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| "The mail client's command line is empty.".to_owned())?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    hand_off(program, &args, HANDOFF)
}

/// The command line of the default `mailto:` handler's desktop entry, with
/// `url` in place of its URL field code.
fn freedesktop_argv(url: &str) -> Result<Vec<String>, String> {
    let output = Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/mailto"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("The system's mail client could not be looked up (xdg-mime): {e}."))?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || id.is_empty() {
        return Err("No mail client is registered for mailto: links.".into());
    }
    let entry = desktop_file(&id, &data_dirs())
        .ok_or_else(|| format!("The mail client's desktop entry {id} was not found."))?;
    let text = std::fs::read_to_string(&entry)
        .map_err(|e| format!("The mail client's desktop entry {id} could not be read: {e}."))?;
    let exec = desktop_exec(&text)
        .ok_or_else(|| format!("The mail client's desktop entry {id} has no command."))?;
    exec_argv(&exec, url)
        .ok_or_else(|| format!("The mail client's command in {id} cannot be read."))
}

/// `$XDG_DATA_HOME` (`~/.local/share`) then `$XDG_DATA_DIRS`
/// (`/usr/local/share:/usr/share`), as the base directory spec orders them.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        Some(home) => dirs.push(PathBuf::from(home)),
        None => {
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(PathBuf::from(home).join(".local/share"));
            }
        }
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(
        system
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// The desktop entry of id `id` under `dirs`' `applications`: the file of
/// that name, or (the desktop entry spec's id rule) one whose `-`s stand
/// for subdirectories.
fn desktop_file(id: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    if id.contains('/') || id.contains("..") {
        return None;
    }
    let candidates = || {
        let dashes: Vec<usize> = id.match_indices('-').map(|(i, _)| i).collect();
        let mut names = vec![PathBuf::from(id)];
        for &i in &dashes {
            names.push(Path::new(&id[..i]).join(&id[i + 1..]));
        }
        names
    };
    dirs.iter().find_map(|dir| {
        candidates()
            .into_iter()
            .map(|name| dir.join("applications").join(name))
            .find(|path| path.is_file())
    })
}

/// The `Exec` key of a desktop entry's `[Desktop Entry]` group, its string
/// escapes undone.
fn desktop_exec(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(value) = line.strip_prefix("Exec") {
            let value = value.trim_start();
            if let Some(value) = value.strip_prefix('=') {
                return Some(unescape_string(value.trim()));
            }
        }
    }
    None
}

/// A desktop entry string's escapes: `\s`, `\n`, `\t`, `\r`, `\\`.
fn unescape_string(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// An `Exec` value as arguments (the desktop entry spec's quoting: double
/// quotes, inside which `\"`, `` \` ``, `\$` and `\\` escape), with `url`
/// for `%u` or `%U` and the other field codes dropped; `url` is appended
/// when no field code names it. `None` for an unended quote or no program.
fn exec_argv(exec: &str, url: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut started) = (false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' => current.push(chars.next()?),
                c => current.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                started = true;
            }
            ' ' | '\t' => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return None;
    }
    if started {
        words.push(current);
    }
    let mut argv = Vec::new();
    let mut named = false;
    for word in words {
        match word.as_str() {
            "%u" | "%U" => {
                argv.push(url.to_owned());
                named = true;
            }
            "%f" | "%F" | "%i" | "%c" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            _ => {
                let mut out = String::new();
                let mut rest = word.chars().peekable();
                while let Some(c) = rest.next() {
                    if c != '%' {
                        out.push(c);
                        continue;
                    }
                    match rest.next() {
                        Some('%') => out.push('%'),
                        Some('u' | 'U') => {
                            out.push_str(url);
                            named = true;
                        }
                        Some(_) | None => {}
                    }
                }
                argv.push(out);
            }
        }
    }
    if !named {
        argv.push(url.to_owned());
    }
    (!argv.is_empty() && !argv[0].is_empty()).then_some(argv)
}

/// Run `program` with `args` and wait up to `wait` for its exit: zero is
/// taken, non-zero is the handler's refusal, and one still running then is
/// unconfirmed, an error, though the process is left to run (and reaped
/// when it ends).
fn hand_off(program: &str, args: &[&str], wait: Duration) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("The mail client ({program}) could not be started: {e}."))?;
    let deadline = Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "The mail client ({program}) did not take the message ({status})."
                ))
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                // Left to run (it may still open the draft), and reaped when
                // it ends, not left behind as a zombie.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Err(format!(
                    "The mail client ({program}) did not confirm the message within {} \
                     seconds; its window may still open.",
                    wait.as_secs()
                ));
            }
            Err(e) => {
                return Err(format!(
                    "The mail client ({program}) could not be waited for: {e}."
                ))
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The handler gets the URL as one argument, and its refusal or
    /// absence is the step's failure rather than a silent success.
    #[test]
    fn the_handler_takes_the_url_and_its_refusal_fails() {
        let dir = std::env::temp_dir().join(format!("om-gui-mailto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("url");
        let script = format!("printf %s \"$1\" > '{}'", out.display());
        let url = "mailto:pat@example.com?subject=a%20b&body=c%0D%0Ad";
        hand_off("sh", &["-c", &script, "sh", url], HANDOFF).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), url);
        let refused = hand_off("false", &[url], HANDOFF).unwrap_err();
        assert!(refused.contains("did not take"), "{refused}");
        let missing = hand_off("om-no-such-mail-client", &[url], HANDOFF).unwrap_err();
        assert!(missing.contains("could not be started"), "{missing}");
        // An acknowledgment within the wait is taken; a failure after it,
        // or no answer by then, is not confirmed: the step's error.
        let quick = hand_off("sh", &["-c", "sleep 0.05; exit 0"], Duration::from_secs(5));
        assert!(quick.is_ok(), "{quick:?}");
        let late = hand_off(
            "sh",
            &["-c", "sleep 1; exit 42"],
            Duration::from_millis(100),
        );
        assert!(late.unwrap_err().contains("did not confirm"));
        let open = hand_off("sh", &["-c", "sleep 5"], Duration::from_millis(50));
        assert!(open.unwrap_err().contains("did not confirm"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The registered handler's desktop entry gives the command line, the
    /// URL in place of its field code, found by id also under a
    /// subdirectory.
    #[test]
    fn the_mailto_handler_runs_from_its_desktop_entry() {
        let url = "mailto:pat@example.com";
        let entry = "[Desktop Entry]\nName=Mail\nExec=\"/opt/My Mail/mail\" -compose %u --flag\n\n\
                     [Desktop Action New]\nExec=other %u\n";
        let exec = desktop_exec(entry).unwrap();
        assert_eq!(
            exec_argv(&exec, url).unwrap(),
            ["/opt/My Mail/mail", "-compose", url, "--flag"]
        );
        assert_eq!(
            exec_argv("mail --url=%u %F", url).unwrap(),
            ["mail", &format!("--url={url}")]
        );
        assert_eq!(exec_argv("mail", url).unwrap(), ["mail", url]);
        assert_eq!(exec_argv("mail \"open", url), None);
        assert_eq!(desktop_exec("[Desktop Entry]\nName=Mail\n"), None);

        let dir = std::env::temp_dir().join(format!("om-gui-desktop-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("applications/kde")).unwrap();
        std::fs::write(dir.join("applications/kde/mail.desktop"), entry).unwrap();
        assert_eq!(
            desktop_file("kde-mail.desktop", std::slice::from_ref(&dir)),
            Some(dir.join("applications/kde/mail.desktop"))
        );
        assert_eq!(
            desktop_file("none.desktop", std::slice::from_ref(&dir)),
            None
        );
        assert_eq!(
            desktop_file("../x.desktop", std::slice::from_ref(&dir)),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
