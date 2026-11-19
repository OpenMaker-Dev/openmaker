//! A backup's folder name and its `backup.config`, as FileMaker Server
//! writes them (docs/re/backups.md 2.1, 2.2).

use std::ffi::OsStr;
use std::io::{self, Read, Write};

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use jiff::Timestamp;

use super::fs::Folder;

/// The file in a backup's folder that holds its label, time, preserved
/// state and id.
pub(super) const CONFIG: &str = "backup.config";

/// What a backup's folder name ends with while its run writes it.
pub(super) const IN_PROGRESS: &str = "_InProgress";

/// The label of a backup a schedule made on its own.
pub(super) const AUTO_BACKUP: &str = "Auto-backup";

/// The label of a backup made on demand (Back Up Now).
pub(super) const ON_DEMAND: &str = "On-demand";

/// The longest label, in characters (OpenMaker's choice: FileMaker's
/// limit is unmeasured; its schedule names have 100).
pub const MAX_LABEL_CHARS: usize = 100;

/// The largest `backup.config` read; FileMaker's are about 100 bytes.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// The keys FileMaker writes, in its order.
const LABEL: &str = "Label";
const TIME_STAMP: &str = "TimeStamp";
const IS_PRESERVED: &str = "IsPreserved";
const ID: &str = "ID";

/// How the date and time of a run end its folder's name:
/// `_YYYY-MM-DD_HHMM`.
const NAME_TIME: &str = "_%Y-%m-%d_%H%M";
/// Its length.
const NAME_TIME_LEN: usize = "_YYYY-MM-DD_HHMM".len();

/// `TimeStamp`'s form, in UTC.
const TIME_STAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// The folder name of schedule `schedule`'s backup started at `at` (the
/// server's local time): `<schedule>_YYYY-MM-DD_HHMM`.
pub(super) fn folder_name(schedule: &str, at: DateTime) -> String {
    format!("{schedule}{}", at.strftime(NAME_TIME))
}

/// The schedule and start time a backup's folder name holds, read from its
/// end, so that a schedule name may hold `_` and digits too.
pub(super) fn parse_folder_name(name: &str) -> Option<(&str, DateTime)> {
    let split = name.len().checked_sub(NAME_TIME_LEN)?;
    if split == 0 || !name.is_char_boundary(split) {
        return None;
    }
    let (schedule, time) = name.split_at(split);
    // `strptime` is lenient about widths: the form is checked byte by byte.
    let digits = time.bytes().enumerate().all(|(i, b)| match i {
        0 | 11 => b == b'_',
        5 | 8 => b == b'-',
        _ => b.is_ascii_digit(),
    });
    if !digits {
        return None;
    }
    let at = DateTime::strptime(NAME_TIME, time).ok()?;
    Some((schedule, at))
}

/// A backup's `backup.config`: `Key=Value` lines. Lines and keys this
/// server does not know are kept as they are when it writes the file back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Config {
    lines: Vec<(String, String)>,
}

impl Config {
    /// The config of a new backup, as FileMaker writes one.
    pub fn new(label: &str, started: Timestamp, id: String) -> Self {
        let stamp = started
            .to_zoned(TimeZone::UTC)
            .strftime(TIME_STAMP_FORMAT)
            .to_string();
        Config {
            lines: vec![
                (LABEL.into(), label.into()),
                (TIME_STAMP.into(), stamp),
                (IS_PRESERVED.into(), "false".into()),
                (ID.into(), id),
            ],
        }
    }

    /// Read `text`; `None` unless every line is `Key=Value` and the config
    /// has an id (a random UUID's 32 hex digits).
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = Vec::new();
        for line in text.lines() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once('=')?;
            lines.push((key.to_string(), value.to_string()));
        }
        let config = Config { lines };
        let id = config.get(ID)?;
        (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(config)
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.lines
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn set(&mut self, key: &str, value: &str) {
        match self.lines.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value.to_string(),
            None => self.lines.push((key.to_string(), value.to_string())),
        }
    }

    /// The backup's id, in upper case.
    pub fn id(&self) -> String {
        self.get(ID).unwrap_or_default().to_ascii_uppercase()
    }

    pub fn label(&self) -> &str {
        self.get(LABEL).unwrap_or_default()
    }

    pub fn set_label(&mut self, label: &str) {
        self.set(LABEL, label);
    }

    pub fn preserved(&self) -> bool {
        self.get(IS_PRESERVED)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    pub fn set_preserved(&mut self, preserved: bool) {
        self.set(IS_PRESERVED, if preserved { "true" } else { "false" });
    }

    /// When the backup started, if its `TimeStamp` reads.
    pub fn time(&self) -> Option<Timestamp> {
        let text = self.get(TIME_STAMP)?;
        let at = DateTime::strptime(TIME_STAMP_FORMAT, text).ok()?;
        at.to_zoned(TimeZone::UTC).ok().map(|z| z.timestamp())
    }

    fn text(&self) -> String {
        self.lines
            .iter()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect()
    }
}

/// Why `label` cannot be a backup's label: it would not stay one line of
/// its `backup.config`, or it is longer than [`MAX_LABEL_CHARS`].
pub(super) fn check_label(label: &str) -> Result<(), String> {
    if label.chars().any(char::is_control) {
        return Err("a label is one line of text, without control characters".into());
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!("a label has at most {MAX_LABEL_CHARS} characters"));
    }
    Ok(())
}

/// A new backup's id: a random version-4 UUID as 32 upper-case hex digits,
/// as FileMaker writes it.
pub(super) fn new_id() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system random source is available");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The config of the backup `folder` is open on: a regular file, not a
/// link, of at most [`MAX_CONFIG_BYTES`].
pub(super) fn read(folder: &Folder) -> io::Result<Config> {
    let file = folder.read_file(OsStr::new(CONFIG))?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too large"));
    }
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(Config::parse)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a backup.config"))
}

/// Write `config` as the config of the backup `folder` is open on: a new
/// file beside it (readable by others, as FileMaker's are, less the
/// umask), flushed, renamed over it, and the folder flushed, so that a
/// crash leaves the old config or the new one.
pub(super) fn write(folder: &Folder, config: &Config) -> io::Result<()> {
    let temporary = OsStr::new("backup.config.omsave");
    // One left by a crash (only this server writes it, under the backups'
    // lock): a link is removed, not followed.
    match folder.remove_file(temporary) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let written = (|| {
        let mut file = folder.create_file(temporary, 0o664)?;
        file.write_all(config.text().as_bytes())?;
        file.sync_all()?;
        folder.replace(temporary, OsStr::new(CONFIG))
    })();
    if written.is_err() {
        let _ = folder.remove_file(temporary);
    }
    written?;
    folder.sync()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lab server's `backup.config`, its id replaced.
    const FILEMAKER: &str = "Label=Auto-backup\nTimeStamp=2026-11-11 06:00:00\n\
                             IsPreserved=false\nID=7F463F4CF60E4591BA1237B8B95A49B3\n";

    #[test]
    fn reads_and_writes_filemakers_backup_config() {
        let config = Config::parse(FILEMAKER).unwrap();
        assert_eq!(config.label(), "Auto-backup");
        assert!(!config.preserved());
        assert_eq!(
            config.time(),
            Some("2026-11-11T06:00:00Z".parse::<Timestamp>().unwrap())
        );
        assert_eq!(config.text(), FILEMAKER);
        let made = Config::new(
            "Auto-backup",
            "2026-11-11T06:00:00Z".parse().unwrap(),
            "7F463F4CF60E4591BA1237B8B95A49B3".into(),
        );
        assert_eq!(made, config);
        assert_eq!(made.text().len(), 102);
        // Keys it does not know stay, in their place; a change replaces
        // the value only.
        let mut other = Config::parse(&format!("Extra=1\r\n{FILEMAKER}")).unwrap();
        other.set_preserved(true);
        other.set_label("Before = upgrade");
        assert_eq!(
            other.text(),
            "Extra=1\nLabel=Before = upgrade\nTimeStamp=2026-11-11 06:00:00\n\
             IsPreserved=true\nID=7F463F4CF60E4591BA1237B8B95A49B3\n"
        );
        for broken in [
            "Label=x\n",
            "ID=12\n",
            "Label\nID=7F463F4CF60E4591BA1237B8B95A49B3\n",
        ] {
            assert_eq!(Config::parse(broken), None, "{broken:?}");
        }
        let id = new_id();
        assert!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!id.bytes().any(|b| b.is_ascii_lowercase()));
        assert_eq!(
            (
                &id[12..13],
                u8::from_str_radix(&id[16..17], 16).unwrap() >> 2
            ),
            ("4", 2)
        );
        assert!(check_label("New\nLabel=").is_err());
        assert!(check_label(&"x".repeat(101)).is_err());
        assert!(check_label("Before the upgrade").is_ok());
    }

    #[test]
    fn folder_names_hold_the_schedule_and_minute() {
        let at = jiff::civil::date(2026, 11, 11).at(0, 5, 59, 0);
        assert_eq!(folder_name("FMS", at), "FMS_2026-11-11_0005");
        assert_eq!(
            parse_folder_name("FMS_2026-11-11_0005"),
            Some(("FMS", jiff::civil::date(2026, 11, 11).at(0, 5, 0, 0)))
        );
        // A schedule name with the same shape inside.
        assert_eq!(
            parse_folder_name("A_2026-01-01_0000_2026-11-11_2359").map(|(s, _)| s),
            Some("A_2026-01-01_0000")
        );
        for name in [
            "FMS_2026-11-11_0000_InProgress",
            "_2026-11-11_0000",
            "FMS_2026-11-11_2400",
            "FMS_2026-1-111_0000",
            "FMS_2026-11-11_000",
            "é_2026-11-11_00000",
        ] {
            assert_eq!(parse_folder_name(name), None, "{name}");
        }
    }
}
