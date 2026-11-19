//! The script oracle's results (`tools/fms-script-harness/results`): per run
//! line, what `RunScriptAndClose` returned, the expressions evaluated after
//! the script, and in order the trace of every executed step, the event-log
//! lines and the `@call`s; and the block indexes `ScanScript` wrote.

use super::battery::unescape;

/// A `>` line: one executed step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub depth: usize,
    pub script: String,
    pub line: usize,
    pub step: u16,
    pub error: i32,
    pub control: bool,
    pub last_error: i32,
    pub watch: Vec<String>,
}

/// What happened during a run, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Step(Step),
    /// `!`: the event-log line `SMScriptRuntime::CheckError` writes.
    Log {
        code: i32,
        message: String,
    },
    /// `@`: a `@call` and its outcome.
    Call(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub script: String,
    pub parameter: String,
    /// `RunScriptAndClose`'s error.
    pub error: i32,
    /// The top script's Exit Script result as text.
    pub result: String,
    /// `Get ( LastError )` after the script.
    pub last_error: i32,
    /// The run line's expressions, evaluated after the script.
    pub values: Vec<String>,
    pub events: Vec<Event>,
}

/// `# scan NAME: ...; index:id=value (0-based): 0:141=0 1:68=3 ...`: each
/// step's stored id (negative when disabled) and step value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan {
    pub script: String,
    pub steps: Vec<(i16, u16)>,
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    pub runs: Vec<Run>,
    pub scans: Vec<Scan>,
}

pub fn parse(text: &str) -> Results {
    let mut results = Results::default();
    for raw in text.lines() {
        if let Some(scan) = raw.strip_prefix("# scan ") {
            if let Some((name, rest)) = scan.split_once(": open ") {
                let steps = rest
                    .split_once("(0-based):")
                    .map(|(_, list)| list)
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(|item| {
                        let (_, step) = item.split_once(':').expect("index:id=value");
                        let (id, value) = step.split_once('=').expect("id=value");
                        (id.parse().expect("id"), value.parse().expect("value"))
                    })
                    .collect();
                results.scans.push(Scan {
                    script: unescape(name),
                    steps,
                });
            }
            continue;
        }
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let fields: Vec<String> = raw.split('\t').map(unescape).collect();
        let event = match fields[0].as_str() {
            ">" => Event::Step(Step {
                depth: number(&fields[1]),
                script: fields[2].clone(),
                line: number(&fields[3]),
                step: number(&fields[4]),
                error: number(&fields[5]),
                control: fields[7] == "1",
                last_error: number(&fields[8]),
                watch: fields[9..].to_vec(),
            }),
            "!" => Event::Log {
                code: number(&fields[1]),
                message: fields[2].clone(),
            },
            "@" => Event::Call(fields[1..].join("\t")),
            _ => {
                results.runs.push(Run {
                    script: fields[0].clone(),
                    parameter: fields[1].clone(),
                    error: number(&fields[2]),
                    result: fields[3].clone(),
                    last_error: number(&fields[4]),
                    values: fields[5..].to_vec(),
                    events: Vec::new(),
                });
                continue;
            }
        };
        results
            .runs
            .last_mut()
            .expect("a run line before its trace")
            .events
            .push(event);
    }
    results
}

fn number<T: std::str::FromStr>(s: &str) -> T {
    s.parse()
        .unwrap_or_else(|_| panic!("{s:?} is not a number"))
}
