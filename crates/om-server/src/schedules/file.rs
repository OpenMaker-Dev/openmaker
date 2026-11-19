//! The schedules file `om-server --schedules` reads: one section per
//! schedule, with FileMaker Server's options as keys.
//!
//! ```text
//! # Lines starting with # are comments.
//! [Nightly import]
//! file = Orders.fmp12
//! script = Import orders
//! account = Admin
//! parameter = full run
//! # once | daily | every 3 days | weekly mon, wed, fri
//! repeat = daily
//! # Or: every = 15 minutes from 08:00 to 18:00
//! at = 02:00
//! # Required for once and every n days.
//! start = 2026-11-01
//! # Inclusive; not for once.
//! end = 2026-12-31
//! # Seconds, minutes or hours.
//! time limit = 30 minutes
//! # Abort the run at the time limit.
//! end on time limit = yes
//! enabled = yes
//!
//! [Nightly backup]
//! # script (the default) or backup
//! type = backup
//! # all (the default), folder <folder> or file <name>
//! back up = all
//! # 1 to 99 (default 1): older backups that are not preserved are deleted
//! keep = 7
//! # Default: the server's --backup-folder.
//! target = /srv/openmaker/Backups
//! repeat = daily
//! at = 00:00
//! ```
//!
//! A value is trimmed; a value in double quotes keeps its spaces and reads
//! `\r`, `\n`, `\t`, `\"` and `\\` as escapes (a parameter with returns).
//! A backup schedule's name begins each backup's folder name, so it must
//! be a valid folder name (`crate::backups::check_schedule_name`).

use std::path::PathBuf;
use std::time::Duration;

use jiff::civil::{Date, Time, Weekday};

use super::calendar::{Days, Repeat, Times};
use super::{BackupFiles, BackupTask, Schedule, ScheduleTask, ScriptTask};
use crate::backups::MAX_KEEP;

/// A schedules file that cannot be read, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleFileError {
    /// The 1-based line.
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ScheduleFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ScheduleFileError {}

/// The schedules a schedules file defines, in its order.
pub fn parse_schedules(text: &str) -> Result<Vec<Schedule>, ScheduleFileError> {
    let mut sections: Vec<Section> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let error = |message: String| ScheduleFileError { line, message };
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('[') {
            let name = name
                .strip_suffix(']')
                .ok_or_else(|| error("a section name ends with ]".into()))?
                .trim();
            if name.is_empty() {
                return Err(error("a schedule needs a name".into()));
            }
            sections.push(Section {
                name: name.to_string(),
                line,
                keys: Vec::new(),
            });
            continue;
        }
        let (key, value) = trimmed
            .split_once('=')
            .ok_or_else(|| error(format!("expected `key = value`, not {trimmed:?}")))?;
        let key = key
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let section = sections
            .last_mut()
            .ok_or_else(|| error("a key before the first [schedule name]".into()))?;
        if section.keys.iter().any(|(_, k, _)| *k == key) {
            return Err(error(format!("{key:?} is given twice")));
        }
        section
            .keys
            .push((line, key, value_text(value).map_err(error)?));
    }
    let mut schedules: Vec<Schedule> = Vec::new();
    for section in sections {
        let line = section.line;
        let schedule = section.schedule()?;
        if schedules
            .iter()
            .any(|s| s.name.to_lowercase() == schedule.name.to_lowercase())
        {
            return Err(ScheduleFileError {
                line,
                message: format!("schedule {:?} is defined twice", schedule.name),
            });
        }
        schedules.push(schedule);
    }
    Ok(schedules)
}

struct Section {
    name: String,
    line: usize,
    /// (line, key, value)
    keys: Vec<(usize, String, String)>,
}

impl Section {
    fn take(&mut self, key: &str) -> Option<(usize, String)> {
        let at = self.keys.iter().position(|(_, k, _)| k == key)?;
        let (line, _, value) = self.keys.remove(at);
        Some((line, value))
    }

    fn require(&mut self, key: &str) -> Result<(usize, String), ScheduleFileError> {
        self.take(key).ok_or_else(|| ScheduleFileError {
            line: self.line,
            message: format!("schedule {:?} needs `{key} = ...`", self.name),
        })
    }

    fn schedule(mut self) -> Result<Schedule, ScheduleFileError> {
        let at_line = |line: usize| move |message: String| ScheduleFileError { line, message };
        let kind = self.take("type");
        let task = match kind.as_ref().map(|(line, v)| (*line, v.to_lowercase())) {
            None => ScheduleTask::Script(self.script_task()?),
            Some((_, kind)) if kind == "script" => ScheduleTask::Script(self.script_task()?),
            Some((_, kind)) if kind == "backup" => ScheduleTask::Backup(self.backup_task()?),
            Some((line, kind)) => {
                return Err(at_line(line)(format!("type {kind:?}: script or backup")))
            }
        };
        let repeat = self.repeat()?;
        let enabled = self
            .take("enabled")
            .map(|(line, v)| yes_no(&v).map_err(at_line(line)))
            .transpose()?
            .unwrap_or(true);
        if let Some((line, key, _)) = self.keys.first() {
            return Err(at_line(*line)(format!("unknown key {key:?}")));
        }
        let schedule = Schedule {
            name: self.name,
            task,
            repeat,
            enabled,
        };
        schedule.check().map_err(at_line(self.line))?;
        Ok(schedule)
    }

    /// A FileMaker script schedule's keys.
    fn script_task(&mut self) -> Result<ScriptTask, ScheduleFileError> {
        let at_line = |line: usize| move |message: String| ScheduleFileError { line, message };
        let (_, file) = self.require("file")?;
        let (_, script) = self.require("script")?;
        let (_, account) = self.require("account")?;
        let parameter = self.take("parameter").map(|(_, p)| p).unwrap_or_default();
        let time_limit = self
            .take("time limit")
            .map(|(line, v)| duration(&v).map_err(at_line(line)))
            .transpose()?;
        let end_on_time_limit = self
            .take("end on time limit")
            .map(|(line, v)| yes_no(&v).map_err(at_line(line)))
            .transpose()?
            .unwrap_or(false);
        Ok(ScriptTask {
            file,
            script,
            account,
            parameter,
            time_limit,
            end_on_time_limit,
        })
    }

    /// A backup schedule's keys: `back up`, `keep` and `target`.
    fn backup_task(&mut self) -> Result<BackupTask, ScheduleFileError> {
        let at_line = |line: usize| move |message: String| ScheduleFileError { line, message };
        let files = match self.take("back up") {
            None => BackupFiles::All,
            Some((line, value)) => backup_files(&value).map_err(at_line(line))?,
        };
        let keep = match self.take("keep") {
            None => 1,
            Some((line, value)) => value
                .parse()
                .ok()
                .filter(|keep| (1..=MAX_KEEP).contains(keep))
                .ok_or_else(|| {
                    at_line(line)(format!(
                        "keep: {value:?} is not a number from 1 to {MAX_KEEP}"
                    ))
                })?,
        };
        let folder = self.take("target").map(|(_, v)| PathBuf::from(v));
        Ok(BackupTask {
            files,
            folder,
            keep,
        })
    }

    /// The timing keys: `repeat`, `at` or `every`, `start` and `end`.
    fn repeat(&mut self) -> Result<Repeat, ScheduleFileError> {
        let at_line = |line: usize| move |message: String| ScheduleFileError { line, message };
        let (repeat_line, repeat) = self.require("repeat")?;
        let date = |key: &str, section: &mut Section| -> Result<Option<Date>, ScheduleFileError> {
            section
                .take(key)
                .map(|(line, v)| {
                    v.parse::<Date>().map_err(|_| {
                        at_line(line)(format!("{key}: {v:?} is not a date like 2026-11-01"))
                    })
                })
                .transpose()
        };
        let start = date("start", self)?;
        let end = date("end", self)?;
        let at = self
            .take("at")
            .map(|(line, v)| time(&v).map_err(at_line(line)))
            .transpose()?;
        let every = self
            .take("every")
            .map(|(line, v)| every(&v).map_err(at_line(line)))
            .transpose()?;
        match repeat.to_lowercase().as_str() {
            "once" => {
                if every.is_some() || end.is_some() {
                    return Err(at_line(repeat_line)(
                        "a schedule that runs once has a start date and an `at` time only".into(),
                    ));
                }
                match (start, at) {
                    (Some(date), Some(time)) => Ok(Repeat::Once(date.to_datetime(time))),
                    _ => Err(at_line(repeat_line)(
                        "a schedule that runs once needs `start` and `at`".into(),
                    )),
                }
            }
            other => {
                let days = days(other).map_err(at_line(repeat_line))?;
                let times =
                    match (at, every) {
                        (Some(time), None) => Times::At(time),
                        (None, Some(every)) => every,
                        _ => return Err(at_line(repeat_line)(
                            "give either `at = <time>` or `every = <n> minutes|hours from <time> \
                             to <time>`"
                                .into(),
                        )),
                    };
                Ok(Repeat::Days {
                    days,
                    times,
                    start,
                    end,
                })
            }
        }
    }
}

/// `all`, `folder <folder>` or `file <name>`.
fn backup_files(text: &str) -> Result<BackupFiles, String> {
    let usage = || format!("back up {text:?}: all, folder <folder> or file <name>");
    if text.eq_ignore_ascii_case("all") {
        return Ok(BackupFiles::All);
    }
    let (word, rest) = text.split_once(char::is_whitespace).ok_or_else(usage)?;
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(usage());
    }
    match word.to_lowercase().as_str() {
        "folder" => Ok(BackupFiles::Folder(PathBuf::from(rest))),
        "file" => Ok(BackupFiles::File(rest.to_string())),
        _ => Err(usage()),
    }
}

/// A value: trimmed, or the inside of double quotes with escapes.
fn value_text(value: &str) -> Result<String, String> {
    let value = value.trim();
    let Some(inner) = value.strip_prefix('"') else {
        return Ok(value.to_string());
    };
    let inner = inner
        .strip_suffix('"')
        .ok_or_else(|| "a quoted value ends with \"".to_string())?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('r') => '\r',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('"') => '"',
            Some('\\') => '\\',
            other => return Err(format!("unknown escape \\{}", other.unwrap_or(' '))),
        });
    }
    Ok(out)
}

/// `H:MM` or `H:MM:SS`, 24-hour.
fn time(text: &str) -> Result<Time, String> {
    let bad = || format!("{text:?} is not a 24-hour time like 17:00");
    let parts = text
        .split(':')
        .map(|p| {
            (!p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<i8>().ok())
                .flatten()
        })
        .collect::<Option<Vec<i8>>>()
        .ok_or_else(bad)?;
    match parts.as_slice() {
        [h, m] => Time::new(*h, *m, 0, 0),
        [h, m, s] => Time::new(*h, *m, *s, 0),
        _ => return Err(bad()),
    }
    .map_err(|_| bad())
}

fn yes_no(text: &str) -> Result<bool, String> {
    match text.to_lowercase().as_str() {
        "yes" | "on" | "true" => Ok(true),
        "no" | "off" | "false" => Ok(false),
        _ => Err(format!("expected yes or no, not {text:?}")),
    }
}

/// `<n> seconds|minutes|hours` (or s, min, m, h).
fn duration(text: &str) -> Result<Duration, String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (number, unit) = match words.as_slice() {
        [number, unit] => (*number, *unit),
        [joined] => {
            let split = joined
                .find(|c: char| !c.is_ascii_digit())
                .ok_or_else(|| format!("{text:?} needs a unit (minutes or hours)"))?;
            joined.split_at(split)
        }
        _ => return Err(format!("{text:?} is not a duration like 30 minutes")),
    };
    let n: u64 = number
        .parse()
        .map_err(|_| format!("{text:?} is not a duration like 30 minutes"))?;
    let unit_seconds = match unit.to_lowercase().as_str() {
        "s" | "sec" | "second" | "seconds" => 1,
        "m" | "min" | "minute" | "minutes" => 60,
        "h" | "hour" | "hours" => 3600,
        _ => return Err(format!("unknown unit {unit:?}: seconds, minutes or hours")),
    };
    n.checked_mul(unit_seconds)
        .map(Duration::from_secs)
        .ok_or_else(|| format!("{text:?} is too long"))
}

/// `every = <n> minutes|hours from <time> to <time>`.
fn every(text: &str) -> Result<Times, String> {
    let lower = text.to_lowercase();
    let usage = || format!("{text:?} is not like `15 minutes from 08:00 to 18:00`");
    let (interval, rest) = lower.split_once(" from ").ok_or_else(usage)?;
    let (from, until) = rest.split_once(" to ").ok_or_else(usage)?;
    Ok(Times::Every {
        interval: duration(interval.trim())?,
        from: time(from.trim())?,
        until: time(until.trim())?,
    })
}

/// `daily`, `every <n> days`, `weekly <days>`.
fn days(text: &str) -> Result<Days, String> {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    match words.as_slice() {
        ["daily"] => Ok(Days::Daily),
        ["every", n, "days" | "day"] => n
            .parse()
            .map(Days::Every)
            .map_err(|_| format!("{n:?} is not a number of days")),
        ["weekly", days @ ..] => days
            .iter()
            .map(|d| weekday(d))
            .collect::<Result<Vec<_>, _>>()
            .map(Days::Weekly),
        _ => Err(format!(
            "repeat {text:?}: once, daily, every <n> days or weekly <days>"
        )),
    }
}

fn weekday(text: &str) -> Result<Weekday, String> {
    let day = match text.get(..3).unwrap_or(text) {
        "mon" => Weekday::Monday,
        "tue" => Weekday::Tuesday,
        "wed" => Weekday::Wednesday,
        "thu" => Weekday::Thursday,
        "fri" => Weekday::Friday,
        "sat" => Weekday::Saturday,
        "sun" => Weekday::Sunday,
        _ => return Err(format!("{text:?} is not a day of the week")),
    };
    Ok(day)
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    #[test]
    fn reads_every_option() {
        let schedules = parse_schedules(
            "# Schedules\n\
             [Nightly import]\n\
             file = Orders.fmp12\n\
             script = Import orders\n\
             account = Admin\n\
             parameter = \"  two\\rlines \\\"quoted\\\"\"\n\
             repeat = weekly Mon, wed FRIDAY\n\
             every = 15 minutes from 08:00 to 18:30\n\
             start = 2026-11-01\n\
             end = 2026-12-31\n\
             Time  Limit = 30 minutes\n\
             end on time limit = yes\n\
             enabled = no\n\
             \n\
             [Once]\n\
             file = a\n\
             script = b\n\
             account = c\n\
             repeat = once\n\
             start = 2026-11-02\n\
             at = 6:30\n\
             [Nightly backup]\n\
             type = Backup\n\
             back up = folder /srv/Databases/Sub\n\
             keep = 7\n\
             target = /srv/Backups\n\
             repeat = daily\n\
             at = 0:00\n\
             [Sample backup]\n\
             type = backup\n\
             back up = file  Sample.fmp12 \n\
             repeat = daily\n\
             at = 12:00\n",
        )
        .unwrap();
        assert_eq!(schedules.len(), 4);
        let script = |s: &Schedule| match &s.task {
            ScheduleTask::Script(task) => task.clone(),
            ScheduleTask::Backup(_) => panic!("{s:?}"),
        };
        let s = &schedules[0];
        let task = script(s);
        assert_eq!(
            (s.name.as_str(), task.file.as_str(), task.script.as_str()),
            ("Nightly import", "Orders.fmp12", "Import orders")
        );
        assert_eq!(task.parameter, "  two\rlines \"quoted\"");
        assert_eq!(
            s.repeat,
            Repeat::Days {
                days: Days::Weekly(vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday]),
                times: Times::Every {
                    interval: Duration::from_secs(900),
                    from: Time::new(8, 0, 0, 0).unwrap(),
                    until: Time::new(18, 30, 0, 0).unwrap(),
                },
                start: Some(date(2026, 11, 1)),
                end: Some(date(2026, 12, 31)),
            }
        );
        assert_eq!(task.time_limit, Some(Duration::from_secs(1800)));
        assert!(task.end_on_time_limit && !s.enabled);
        let once = &schedules[1];
        assert_eq!(once.repeat, Repeat::Once(date(2026, 11, 2).at(6, 30, 0, 0)));
        let task = script(once);
        assert_eq!(task.parameter, "");
        assert!(once.enabled && !task.end_on_time_limit && task.time_limit.is_none());
        assert_eq!(
            schedules[2].task,
            ScheduleTask::Backup(BackupTask {
                files: BackupFiles::Folder("/srv/Databases/Sub".into()),
                folder: Some("/srv/Backups".into()),
                keep: 7,
            })
        );
        assert_eq!(
            schedules[3].task,
            ScheduleTask::Backup(BackupTask {
                files: BackupFiles::File("Sample.fmp12".into()),
                folder: None,
                keep: 1,
            })
        );
    }

    #[test]
    fn errors_name_their_line() {
        let base = "[S]\nfile = f\nscript = s\naccount = a\n";
        let error = |extra: &str| parse_schedules(&format!("{base}{extra}")).unwrap_err();
        assert_eq!(error("repeat = daily\nat = 25:00\n").line, 6);
        assert_eq!(error("repeat = daily\n").line, 5);
        assert_eq!(error("repeat = hourly\nat = 1:00\n").line, 5);
        assert_eq!(error("repeat = daily\nat = 1:00\ncolour = red\n").line, 7);
        assert_eq!(error("repeat = daily\nat = 1:00\nat = 2:00\n").line, 7);
        assert_eq!(error("repeat = every 3 days\nat = 1:00\n").line, 1);
        assert_eq!(error("repeat = daily\nat = 1:00\ntime limit = 5\n").line, 7);
        assert_eq!(
            error("repeat = daily\nat = 1:00\n[s]\nfile = f\nscript = s\naccount = a\nrepeat = daily\nat = 1:00\n")
                .line,
            7
        );
        assert_eq!(parse_schedules("file = x\n").unwrap_err().line, 1);
        assert_eq!(parse_schedules("[S]\nfile = f\n").unwrap_err().line, 1);
        let backup = |name: &str, extra: &str| {
            parse_schedules(&format!(
                "[{name}]\ntype = backup\nrepeat = daily\nat = 1:00\n{extra}"
            ))
            .unwrap_err()
            .line
        };
        assert_eq!(backup("B", "keep = 0\n"), 5);
        assert_eq!(backup("B", "keep = 100\n"), 5);
        assert_eq!(backup("B", "back up = everything\n"), 5);
        assert_eq!(backup("B", "back up = folder\n"), 5);
        assert_eq!(backup("B", "script = s\n"), 5);
        // The name begins each backup's folder name.
        assert_eq!(backup("a/b", ""), 1);
        assert_eq!(backup("..", ""), 1);
        assert_eq!(parse_schedules("[S]\ntype = verify\n").unwrap_err().line, 2);
    }
}
