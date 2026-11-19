//! Runs `docs/re/catalogs/execute-sql-vectors.tsv` (FileMaker Server's own
//! answers, execute-sql.md 0) against the sample files the rows name, with
//! om-calc evaluating each row's calculation and om-sql answering its
//! `ExecuteSQL`/`ExecuteSQLe`, and reports covered and passed rows per
//! battery.
//!
//! ```sh
//! OM_SQL_RE=/path/to/openmaker/re cargo run -p om-sql --features fmp12 --example sql_vectors [-- -v]
//! ```
//!
//! Files: the FileMaker Server sample under `$OM_SQL_RE/extracted/fms`, the
//! MBS plug-in examples under `$OM_SQL_RE/work/calc-encoding/corpus-mbs`
//! and the Contacts templates under `$OM_SQL_TEMPLATES` (FileMaker Pro's
//! `DynamicTemplates`). Each is copied to a private directory as
//! `work.fmp12`, the name the harness's copies had (`BaseFileName`). The
//! clock is the harness's, 2026-10-28; rows that depend on the clock, the
//! session or uninitialized memory are skipped.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use om_calc::eval::{self, Context};
use om_calc::{Ctx, Date, Locale, Time, TimeStyle, Timestamp, Value};
use om_sql::fmp12::FileHost;

const VECTORS: &str = include_str!("../../../docs/re/catalogs/execute-sql-vectors.tsv");

/// One row of the catalog.
pub struct Row {
    pub line: usize,
    pub expression: String,
    pub file: String,
    pub context: String,
    pub result: String,
    pub error: i32,
    pub battery: String,
    pub skip: Option<&'static str>,
}

/// Undoes the catalog's escapes `\r \n \t \\ \uXXXX`.
pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = it.by_ref().take(4).collect();
                if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(c);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

pub fn rows() -> Vec<Row> {
    VECTORS
        .lines()
        .enumerate()
        .skip(1)
        .filter_map(|(i, line)| {
            let cols: Vec<&str> = line.split('\t').collect();
            let [expression, file, context, result, error, evidence] = cols[..] else {
                return None;
            };
            let battery = evidence
                .split_whitespace()
                .find_map(|w| w.strip_prefix("battery="))
                .unwrap_or("?")
                .to_string();
            let skip = ["clock", "session", "uninitialized"]
                .into_iter()
                .find(|d| evidence.contains(&format!("depends={d}")));
            Some(Row {
                line: i + 1,
                expression: unescape(expression),
                file: file.to_string(),
                context: context.to_string(),
                result: unescape(result),
                error: error.parse().unwrap_or(0),
                battery,
                skip,
            })
        })
        .collect()
}

/// Where a row's file is and its locale.
pub fn sample(file: &str) -> Option<(PathBuf, Locale)> {
    let re = std::env::var_os("OM_SQL_RE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../re"));
    let templates = std::env::var_os("OM_SQL_TEMPLATES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/Applications/FileMaker Pro.app/Contents/Resources/DynamicTemplates")
        });
    let name = file.split(" (").next()?;
    if name == "en_FMServer_Sample.fmp12" {
        let path = re.join("extracted/fms/deb/opt/FileMaker/FileMaker Server/Data/Databases/.Sample/en_FMServer_Sample");
        return Some((path, Locale::en_us()));
    }
    if let Some(lang) = name
        .strip_prefix("Contacts/")
        .and_then(|n| n.strip_suffix(".fmp12"))
    {
        let locale = match lang {
            "de" => Locale::de_de(),
            "sv" => Locale::sv_se(),
            "ja" => Locale::ja_jp(),
            _ => Locale::en_us(),
        };
        return Some((
            templates.join("Contacts").join(format!("{lang}.fmp12")),
            locale,
        ));
    }
    if let Some(rest) = name.strip_prefix("MBS example ") {
        let flat = format!("Examples__{}", rest.replace('/', "__").replace(' ', "_"));
        return Some((
            re.join("work/calc-encoding/corpus-mbs").join(flat),
            Locale::de_de(),
        ));
    }
    None
}

/// The harness clock (`clock=2026-10-28`).
pub fn now() -> Timestamp {
    let date = Date::from_ymd(2026, 10, 28).expect("date");
    let time = Time::from_micros(12 * 3_600_000_000, TimeStyle::Elapsed);
    Timestamp::from_date_time(date, time).expect("timestamp")
}

/// A row's calculation context: `03d00001 record 30`, `03d00004 no record`.
pub fn context(spec: &str) -> Context {
    let mut words = spec.split_whitespace();
    let Some(key) = words.next().filter(|w| *w != "-") else {
        return Context::new();
    };
    let bytes: Vec<u8> = (0..key.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(key.get(i..i + 2)?, 16).ok())
        .collect();
    let occurrence = bytes
        .iter()
        .skip(1)
        .fold(0u64, |acc, b| acc << 8 | u64::from(*b));
    let record = match (words.next(), words.next()) {
        (Some("record"), Some(n)) => n.parse().ok(),
        _ => None,
    };
    Context {
        occurrence: Some(occurrence),
        record,
        ..Context::default()
    }
}

/// A result per row: `None` when skipped, else whether it passed and what
/// came back.
pub struct Outcome {
    pub row: Row,
    pub got: Option<(String, i32)>,
    pub elapsed: std::time::Duration,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.got.as_ref().is_some_and(|(text, code)| {
            let same = match elided(&self.row.result) {
                Some(kept) => kept.matches(text),
                None => *text == self.row.result,
            };
            same && *code == self.row.error
        })
    }
}

/// A long result the catalog keeps only as its size, digest and first
/// characters, because the value is a sample file's own content (copied
/// third-party text): `[elided: <n> bytes, sha256 <hex>, starts "<text>"]`.
pub struct Elided<'a> {
    pub bytes: usize,
    pub sha256: &'a str,
    pub starts: &'a str,
}

impl Elided<'_> {
    /// Whether `text` is the value kept: its UTF-8 length, its SHA-256 and
    /// its start.
    pub fn matches(&self, text: &str) -> bool {
        let digest: String = om_crypto::sha256(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        text.len() == self.bytes && text.starts_with(self.starts) && digest == self.sha256
    }
}

/// The stand-in a result row holds in place of its value, if it holds one.
pub fn elided(result: &str) -> Option<Elided<'_>> {
    let rest = result.strip_prefix("[elided: ")?.strip_suffix("\"]")?;
    let (bytes, rest) = rest.split_once(" bytes, sha256 ")?;
    let (sha256, starts) = rest.split_once(", starts \"")?;
    Some(Elided {
        bytes: bytes.parse().ok()?,
        sha256,
        starts,
    })
}

/// Runs every row; files that cannot be found skip their rows.
pub fn run() -> Vec<Outcome> {
    let dir = std::env::temp_dir().join(format!("om-sql-vectors-{}", std::process::id()));
    let mut hosts: BTreeMap<String, Option<FileHost>> = BTreeMap::new();
    let mut out = Vec::new();
    for (n, row) in rows().into_iter().enumerate() {
        if row.skip.is_some() {
            out.push(Outcome {
                row,
                got: None,
                elapsed: Default::default(),
            });
            continue;
        }
        let host = hosts.entry(row.file.clone()).or_insert_with(|| {
            let (path, _) = sample(&row.file)?;
            if !path.exists() {
                return None;
            }
            let copy_dir = dir.join(format!("{n}"));
            std::fs::create_dir_all(&copy_dir).ok()?;
            let copy = copy_dir.join("work.fmp12");
            std::fs::copy(&path, &copy).ok()?;
            let mut host = FileHost::open_file(&copy, now()).ok()?;
            // The harness session's answers (calc-values.md 12.1).
            host.set_get(1, Value::text("FMDeveloperTool 26.0.3.309 (08-26-2026)"));
            host.set_get(12, Value::text("English"));
            Some(host)
        });
        let Some(host) = host.as_mut() else {
            out.push(Outcome {
                row,
                got: None,
                elapsed: Default::default(),
            });
            continue;
        };
        let started = std::time::Instant::now();
        let cx_locale = om_calc::eval::CalcHost::locale(host).clone();
        let evaluation = eval::evaluate_text(&row.expression, host, &context(&row.context));
        let cx = Ctx::new(&cx_locale, now().date());
        let text = evaluation.value.to_text(&cx).into_string();
        out.push(Outcome {
            got: Some((text, evaluation.code())),
            row,
            elapsed: started.elapsed(),
        });
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// Covered and passed rows per battery.
pub fn report(outcomes: &[Outcome]) -> BTreeMap<String, (usize, usize, usize)> {
    let mut by_battery: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for o in outcomes {
        let e = by_battery.entry(o.row.battery.clone()).or_default();
        e.0 += 1;
        if o.got.is_some() {
            e.1 += 1;
            if o.passed() {
                e.2 += 1;
            }
        }
    }
    by_battery
}

fn main() {
    let verbose = std::env::args().any(|a| a == "-v");
    let timing = std::env::args().any(|a| a == "-t");
    let outcomes = run();
    if timing {
        let mut slow: Vec<&Outcome> = outcomes.iter().collect();
        slow.sort_by_key(|o| std::cmp::Reverse(o.elapsed));
        for o in slow.iter().take(10) {
            println!(
                "{:>8.1?} line {} {}",
                o.elapsed,
                o.row.line,
                o.row.expression.chars().take(100).collect::<String>()
            );
        }
    }
    let mut failed = 0;
    for o in &outcomes {
        let Some((text, code)) = &o.got else {
            continue;
        };
        if !o.passed() {
            failed += 1;
            if verbose {
                println!(
                    "line {} [{}] {}\n  expected {:?} ({})\n  got      {:?} ({})",
                    o.row.line,
                    o.row.battery,
                    o.row.expression,
                    o.row.result,
                    o.row.error,
                    text,
                    code
                );
            }
        }
    }
    println!("battery            rows  covered  passed");
    let (mut rows, mut covered, mut passed) = (0, 0, 0);
    for (b, (r, c, p)) in report(&outcomes) {
        println!("{b:<18} {r:>4} {c:>8} {p:>7}");
        rows += r;
        covered += c;
        passed += p;
    }
    println!(
        "{:<18} {rows:>4} {covered:>8} {passed:>7}  ({failed} failed)",
        "total"
    );
}
