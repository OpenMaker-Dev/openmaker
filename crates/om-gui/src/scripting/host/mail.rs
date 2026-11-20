//! Send Mail's handoff to the system's mail client: the `mailto:` URL
//! goes to the handler the system registers for `mailto:` (freedesktop's
//! `x-scheme-handler/mailto` through `xdg-open`, macOS's `open`, Windows'
//! `url.dll` protocol handler), not to a web browser's launcher, which on
//! Linux opens the default browser whatever handles mail. The step waits
//! for the handler to say whether it took the message.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the opener may take to say whether it handed the message on.
/// One still running then (a handler that stays in the foreground) has
/// taken it, and is left to finish.
const HANDOFF: Duration = Duration::from_secs(10);

/// Hand `url`, a `mailto:` URL, to the system's `mailto:` handler: Ok once
/// it took it, else why not.
pub(super) fn open_mailto(url: &str) -> Result<(), String> {
    let (program, before): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(windows) {
        ("rundll32", &["url.dll,FileProtocolHandler"])
    } else {
        ("xdg-open", &[])
    };
    hand_off(program, before, url, HANDOFF)
}

/// Run `program` with `before` and `url` as its arguments and wait up to
/// `wait` for its exit: a non-zero exit is the handler's refusal.
fn hand_off(program: &str, before: &[&str], url: &str, wait: Duration) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(before)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("The system's mail handler ({program}) could not be started: {e}."))?;
    let deadline = Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "The system's mail handler ({program}) did not take the message ({status})."
                ))
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                // Reaped when it ends, not left behind as a zombie.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(e) => {
                return Err(format!(
                    "The system's mail handler ({program}) could not be waited for: {e}."
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
        hand_off("sh", &["-c", &script, "sh"], url, HANDOFF).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), url);
        let refused = hand_off("false", &[], url, HANDOFF).unwrap_err();
        assert!(refused.contains("did not take"), "{refused}");
        let missing = hand_off("om-no-such-mail-handler", &[], url, HANDOFF).unwrap_err();
        assert!(missing.contains("could not be started"), "{missing}");
        // One still running at the deadline has taken the message.
        let waiting = hand_off(
            "sh",
            &["-c", "sleep 5", "sh"],
            url,
            Duration::from_millis(50),
        );
        assert!(waiting.is_ok(), "{waiting:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
