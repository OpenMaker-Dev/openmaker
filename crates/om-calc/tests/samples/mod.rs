//! In-memory copies of the files the harness batteries ran in
//! (`tools/fms-calc-harness`, calc-values.md 12.1): the schema and the data
//! the batteries read, as surveyed with `ExecuteSQL` (batteries `e02`,
//! `e04`), and the runner that evaluates a battery's expressions through
//! `om_formula::compile` and the evaluator and compares them with the
//! engine's results.
#![allow(dead_code)]

use std::collections::BTreeMap;

use std::cmp::Ordering;

use om_calc::eval::{self, Context, FieldInfo, Join, MemoryHost, Storage};
use om_calc::ops::{self, Comparison};
use om_calc::{CalcError, Ctx, Date, Locale, Number, Value, ValueType};

/// The harness clock (`clock=2026-10-28` rows).
pub fn today() -> Date {
    Date::from_ymd(2026, 10, 28).unwrap()
}

/// The occurrence a battery's stored context key names (`03d00001`).
pub fn occurrence(key: &str) -> Option<u64> {
    let bytes: Vec<u8> = (0..key.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(key.get(i..i + 2)?, 16).ok())
        .collect::<Option<_>>()?;
    let (&len, id) = bytes.split_first()?;
    (usize::from(len) == id.len()).then(|| id.iter().fold(0, |acc, &b| acc << 8 | u64::from(b)))
}

const TEXT: ValueType = ValueType::Text;
const NUMBER: ValueType = ValueType::Number;

fn stored(ty: ValueType, reps: u32) -> FieldInfo {
    FieldInfo::stored(ty, reps)
}

fn global(ty: ValueType) -> FieldInfo {
    FieldInfo {
        value_type: ty,
        repetitions: 1,
        storage: Storage::Global,
    }
}

/// The Contacts template of a locale, as the batteries without a table
/// context saw it: an occurrence `Contacts` (so that `Contacts::NoSuchField`
/// is 102 and an unqualified name 106) and the harness session's answers.
pub fn contacts(locale: Locale) -> MemoryHost {
    let mut host = MemoryHost::new(locale, today());
    host.add_table(1, "Contacts");
    host.add_occurrence(0xd00001, "Contacts", 1);
    host.add_field(1, 1, "First Name", stored(TEXT, 1));
    host.add_field(1, 2, "Last Name", stored(TEXT, 1));
    // Get ( SystemLanguage ) in the harness session.
    host.set_get(12, Value::text("English"));
    host
}

/// A record of AppleTouchBar: ID, `flex_Label` and `flex_Script`
/// repetitions 1-3, `_default`.
type AtbRecord = (
    u64,
    [&'static str; 3],
    [&'static str; 3],
    [&'static str; 12],
);

/// MBS example AppleTouchBar (battery x01, e04): table ATB, occurrence
/// `03d00001`, records 2, 8, 11, 15.
pub fn mbs_atb() -> MemoryHost {
    const T: u64 = 1;
    let mut host = MemoryHost::new(Locale::en_us(), today());
    host.add_table(T, "ATB");
    host.add_occurrence(0xd00001, "ATB", T);
    host.add_table(2, "ATB_Icons");
    host.add_occurrence(0xd00002, "ATB_Icons", 2);
    host.add_field(2, 1, "Info", stored(TEXT, 1));
    let fields: [(&str, ValueType, u32); 9] = [
        ("_Set", TEXT, 1),
        ("_Table", TEXT, 1),
        ("flex_Function", TEXT, 9),
        ("flex_Script", TEXT, 9),
        ("_flex_Add", TEXT, 9),
        ("flex_Icon", TEXT, 9),
        ("flex_Label", TEXT, 9),
        ("_default", TEXT, 12),
        ("Zoom", NUMBER, 1),
    ];
    for (i, (name, ty, reps)) in fields.iter().enumerate() {
        host.add_field(T, i as u64 + 1, name, stored(*ty, *reps));
    }
    host.add_field(T, 20, "Dev_Notes", global(TEXT));
    host.add_calculation(
        0xd00001,
        21,
        "f_Add",
        stored(TEXT, 1),
        "List ( Extend ( _flex_Add ) )",
    )
    .unwrap();
    let id = |name: &str| fields.iter().position(|f| f.0 == name).unwrap() as u64 + 1;
    let rows: [AtbRecord; 4] = [
        (
            2,
            ["\"+add\"", "\"Text\"", ""],
            ["ADR1_AddRec", "ADR1_DeleteRec", ""],
            [
                "AddRec",
                "Delete",
                "STouchBarItemIdentifierFixedSpaceLarge",
                "STouchBarItemIdentifierFlexibleSpace",
                "home",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
            ],
        ),
        (
            8,
            ["", "", ""],
            ["alpha", "beta", "delta"],
            [
                "Info",
                "STouchBarItemIdentifierFixedSpaceSmall",
                "Home",
                "Calendar",
                "Invoice",
                "STouchBarItemIdentifierFixedSpaceLarge",
                "Print",
                "List",
                "",
                "",
                "",
                "",
            ],
        ),
        (
            11,
            ["", "", ""],
            ["alpha", "beta", "delta"],
            [
                "Info", "Search", "read", "Excel", "PDF", "email", "setup", "return", "trash", "",
                "", "",
            ],
        ),
        (
            15,
            ["\"delete\"", "", ""],
            ["zeta", "", ""],
            ["Delete", "", "", "", "", "", "", "", "", "", "", ""],
        ),
    ];
    for (record, labels, scripts, defaults) in rows {
        host.add_record(T, record);
        for (rep, v) in labels.iter().enumerate() {
            set_text(&mut host, T, record, id("flex_Label"), rep as u32 + 1, v);
        }
        for (rep, v) in scripts.iter().enumerate() {
            set_text(&mut host, T, record, id("flex_Script"), rep as u32 + 1, v);
        }
        for (rep, v) in defaults.iter().enumerate() {
            set_text(&mut host, T, record, id("_default"), rep as u32 + 1, v);
        }
    }
    for record in [2, 8] {
        set_text(&mut host, T, record, id("_Table"), 1, "ATB_Icons");
    }
    set_text(&mut host, T, 2, id("_Set"), 1, "ADR1");
    // The stored repeating calculation _flex_Add of record 2 (x01 results).
    let flex_add = [
        "$r = MBS( \"TouchBar.AddButton\"; $t; \"AddRec\"; \"+add\"; ; \"AddRec\"; 0; Get(FileName); \"ADR1_AddRec\"); ",
        "$r = MBS( \"TouchBar.AddButton\"; $t; \"Delete\"; \"Text\"; ; \"Delete\"; 0; Get(FileName); \"ADR1_DeleteRec\"); ",
        "$r = MBS( \"TouchBar.AddButton\"; $t; \"home\"; ; ATB_Icons::Info; \"home\"; 0; Get(FileName); \"\"); ",
    ];
    for (rep, v) in flex_add.iter().enumerate() {
        set_text(&mut host, T, 2, id("_flex_Add"), rep as u32 + 1, v);
    }
    host
}

fn set_text(host: &mut MemoryHost, table: u64, record: u64, field: u64, rep: u32, v: &str) {
    if !v.is_empty() {
        host.set(table, record, field, rep, Value::text(v));
    }
}

/// MBS example Phidget Digital Outputs (battery x02): occurrence
/// `03d00001`, record 1; `Connected` has 8 repetitions, all 1.
pub fn mbs_phidget() -> MemoryHost {
    let mut host = MemoryHost::new(Locale::en_us(), today());
    host.add_table(1, "Phidget Outputs");
    host.add_occurrence(0xd00001, "Phidget Outputs", 1);
    host.add_field(1, 1, "Connected", stored(NUMBER, 8));
    for (i, name) in ["DeviceSerialNumber", "DeviceID", "ChannelClass"]
        .iter()
        .enumerate()
    {
        host.add_field(1, i as u64 + 2, name, stored(NUMBER, 1));
    }
    host.add_field(1, 5, "DeviceName", stored(TEXT, 1));
    host.add_field(1, 6, "DeviceLabel", stored(TEXT, 1));
    host.add_record(1, 1);
    for rep in 1..=8 {
        host.set(1, 1, 1, rep, Value::number(1));
    }
    host
}

/// Tasks occurrences of the FileMaker Server sample (x03, e02, e05).
pub const TASKS: u64 = 0xd00001;
pub const RELATED_TASKS: u64 = 0xd00003;

/// The FileMaker Server sample `en_FMServer_Sample.fmp12` (x03, e02, e05):
/// Tasks with 12 records (IDs 27..38), the unsorted self-join Related Tasks
/// to all of them, and Assignees and Attachments without records.
pub fn fms_sample() -> MemoryHost {
    let (tasks, assignees, attachments) = (1, 2, 3);
    let mut host = MemoryHost::new(Locale::en_us(), today());
    host.add_table(tasks, "Tasks");
    host.add_table(assignees, "Assignees");
    host.add_table(attachments, "Attachments");
    host.add_occurrence(TASKS, "Tasks", tasks);
    host.add_occurrence(RELATED_TASKS, "Related Tasks", tasks);
    host.add_occurrence(0xd00004, "Assignees", assignees);
    host.add_occurrence(0xd00005, "Attachments", attachments);
    let task_fields: [(&str, FieldInfo); 17] = [
        ("TASK ID MATCH FIELD", stored(NUMBER, 1)),
        ("Task", stored(TEXT, 1)),
        ("Due Date", stored(ValueType::Date, 1)),
        ("Description", stored(TEXT, 1)),
        ("Category", stored(TEXT, 1)),
        ("Status", stored(TEXT, 1)),
        ("Sort List Key | MATCH FIELD", stored(TEXT, 1)),
        ("Related | Sort Selection", global(TEXT)),
        ("Time Frame Order", stored(NUMBER, 1)),
        ("Time Frame", stored(TEXT, 1)),
        ("QuickFind", global(TEXT)),
        ("Assignee Number", stored(NUMBER, 1)),
        ("Attachment Number", stored(NUMBER, 1)),
        ("Days Till Due", stored(NUMBER, 1)),
        ("Past | Upcoming Filter", global(TEXT)),
        ("Day Label Plural", stored(TEXT, 1)),
        ("Result Label Plural", stored(TEXT, 1)),
    ];
    for (i, (name, info)) in task_fields.into_iter().enumerate() {
        host.add_field(tasks, i as u64 + 1, name, info);
    }
    for (table, extra) in [
        (assignees, ["Name", "Email", "Phone"]),
        (
            attachments,
            [
                "Comments",
                "Attachment | Container",
                "File Name Placeholder",
            ],
        ),
    ] {
        host.add_field(table, 1, "TASK ID MATCH FIELD", stored(NUMBER, 1));
        for (i, name) in extra.iter().enumerate() {
            host.add_field(table, i as u64 + 2, name, stored(TEXT, 1));
        }
    }
    let data = [
        (27, "Site Map Sketch", 7),
        (28, "Checklist", 1),
        (29, "Hire New Sales Person", 3),
        (30, "Update Web Page", 12),
        (31, "Pay Internet Service Bill", 5),
        (32, "Plan Holiday", 6),
        (33, "Review mock ups", 8),
        (34, "Write Page Text", 11),
        (35, "New Logo Art", 9),
        (36, "Determine Budget for Marketing Campaign", 2),
        (37, "Send Art to Vendor", 10),
        (38, "Repair Water Damage in Kitchen", 4),
    ];
    for (id, task, key) in data {
        host.add_record(tasks, id);
        host.set(tasks, id, 1, 1, Value::number(key));
        host.set(tasks, id, 2, 1, Value::text(task));
    }
    // The calculations of record 30 as the engine computed them (x03).
    host.set(tasks, 30, 9, 1, Value::number(7));
    host.set(tasks, 30, 10, 1, Value::text("2015"));
    host.set(tasks, 30, 12, 1, Value::number(0));
    host.set(tasks, 30, 13, 1, Value::number(0));
    host.set(tasks, 30, 14, 1, Value::number(0));
    host.relate(TASKS, RELATED_TASKS, Join::All);
    let by_key = Join::Equal { left: 1, right: 1 };
    host.relate(TASKS, 0xd00004, by_key);
    host.relate(TASKS, 0xd00005, by_key);
    host
}

/// MBS example FileMaker_Snippet_Storage (x04, e03): its custom functions,
/// as stored in the file (the text FileMaker displays).
pub fn mbs_snippets() -> MemoryHost {
    let mut host = MemoryHost::new(Locale::en_us(), today());
    host.add_table(1, "SnippetStorageDB");
    host.add_occurrence(0xd00001, "SnippetStorageDB", 1);
    host.add_custom_function(
        17,
        "__GEN_Range_ID",
        &["pBeginn", "pEnde", "pAbstand"],
        "/*__GEN_Range_ID ( pBeginn ; pEnde ; pAbstand )*/\r\rLet (\r\rd = pBeginn ;\r\rCase (\rd > pEnde ; \"\" ;\rIsEmpty ( pBeginn ) or IsEmpty ( pEnde ) ; \"\" ;\r\rd = pEnde ; d ;\r\rd & ¶ & __GEN_Range_ID ( d + pAbstand ; pEnde ; pAbstand )\r\r)\r\r)",
    )
    .unwrap();
    host.add_custom_function(
        16,
        "__GEN_RandomPW",
        &["pAnzahlZeichen", "pReturn", "pMethod"],
        "Let\r(\r[\rRep1 = \"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ßäáàâëéèêïíìîöóòôüúùûÄÁÀÂËÉÈÊÏÍÌÎÖÓÒÔÜÚÙÛ-_.,!§$%&([{)]}}+~#@€$£\" ;\rLR = Case\r(\rpMethod = 1 ; 26 ;\rpMethod = 2 ; 52 ;\rpMethod = 3 ; 62 ;\rpMethod = 4 ; Length ( Rep1 ) ;\rLength ( Rep1 )\r) ;\rRep2 = Left ( Rep1 ; LR ) ;\r\rString = pReturn & Middle ( Rep2 ; Round ( Random * ( LR - 1 ) ; 0 ) +1 ; 1 )\r] ;\r\rCase\r(\rLength ( String ) < pAnzahlZeichen ; __GEN_RandomPW ( pAnzahlZeichen ; String ; pMethod ) ;\rString\r)\r\r)",
    )
    .unwrap();
    host.add_custom_function(
        64,
        "__GEN_Rabatt",
        &["pBetrag", "pRabatt"],
        "Let ( [ \r\rrF = Substitute ( pRabatt ; \"+\" ; \"¶\" ) ;\rr1 = LeftValues ( rF ; 1) ;\rr2 = MiddleValues (  rF ;2; 1) ;\rr3 = MiddleValues (  rF ;3; 1) ;\rb1 = Round ( pBetrag * r1 / 100 ;2)  ;\rb2 = If (IsEmpty ( r2 ) ; 0 ; Round ( ( pBetrag - b1 ) * r2 / 100 ; 2 ) ) ;\rb3 = If (IsEmpty ( r3 ) ; 0 ; Round ( ( pBetrag - b1 - b2 ) * r3 /100 ; 2 ) ) \r\r] ; \r\rb1 + b2 + b3 \r\r)",
    )
    .unwrap();
    host
}

/// Stand-ins for library functions that are not merged yet, answered by
/// the host so that rows using them exercise the evaluator (field
/// operands, aggregate expansion, custom functions). Aggregates receive
/// every value their arguments stand for. Kept to what the batteries need,
/// on plain text; the library's own functions replace them as they land
/// (the evaluator only asks the host for functions the library lacks).
pub const STAND_INS: &[(&str, eval::HostFn)] = &[
    ("Count", count),
    ("List", list),
    ("Sum", sum),
    ("Average", average),
    ("Max", max),
    ("Min", min),
    ("Length", length),
    ("ValueCount", value_count),
    ("Left", left),
    ("Middle", middle),
    ("Round", round),
    ("GetAsText", get_as_text),
];

pub fn add_stand_ins(host: &mut MemoryHost) {
    for (name, f) in STAND_INS {
        host.add_function(name, *f);
    }
}

fn present(values: &[Value]) -> impl Iterator<Item = &Value> {
    values.iter().filter(|v| !v.is_empty())
}

fn count(values: &[Value], _: &Ctx) -> Result<Value, CalcError> {
    Ok(Value::number(present(values).count() as i64))
}

fn list(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let texts: Vec<String> = present(values)
        .map(|v| v.to_text(cx).into_string())
        .collect();
    Ok(Value::text(texts.join("\r")))
}

fn total(values: &[Value], cx: &Ctx) -> Option<(Number, i64)> {
    let mut n = 0;
    let mut sum: Option<Number> = None;
    for v in present(values) {
        let x = v.numeric_operand(cx);
        sum = Some(match sum {
            Some(s) => s.add(&x, cx.precision),
            None => x,
        });
        n += 1;
    }
    sum.map(|s| (s, n))
}

fn sum(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(Value::Number(
        total(values, cx).map_or_else(Number::empty, |(s, _)| s),
    ))
}

fn average(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    match total(values, cx) {
        Some((s, n)) => Ok(Value::Number(s.div(&Number::from_i64(n), cx.precision)?)),
        None => Ok(Value::Number(Number::empty())),
    }
}

/// `Max` and `Min`: as text when every value is text, else by number; the
/// value chosen keeps its type.
fn extreme(values: &[Value], cx: &Ctx, want: Ordering) -> Result<Value, CalcError> {
    let all_text = present(values).all(|v| v.value_type() == ValueType::Text);
    let order = |a: &Value, b: &Value| {
        if all_text {
            ops::compare(a, b, cx)
        } else {
            match a.numeric_operand(cx).compare(&b.numeric_operand(cx)) {
                Some(o) => Comparison::Ordered(o),
                None => Comparison::Unordered,
            }
        }
    };
    let mut best: Option<&Value> = None;
    for v in present(values) {
        best = match best {
            Some(b) if order(v, b) != Comparison::Ordered(want) => Some(b),
            _ => Some(v),
        };
    }
    Ok(best.cloned().unwrap_or_else(Value::empty_text))
}

fn max(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    extreme(values, cx, Ordering::Greater)
}

fn min(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    extreme(values, cx, Ordering::Less)
}

fn length(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let text = values[0].to_text(cx);
    // Plain ASCII without CR LF has one character per byte (the batteries
    // measure 60,000-character texts 60,000 times).
    let n = if text.as_str().is_ascii() && !text.as_str().contains("\r\n") {
        text.as_str().len()
    } else {
        text.length()
    };
    Ok(Value::number(n as i64))
}

fn value_count(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(Value::number(
        values[0].to_text(cx).value_ranges().len() as i64
    ))
}

fn count_arg(v: &Value, cx: &Ctx) -> i64 {
    v.to_number(cx).to_i64().unwrap_or(0)
}

fn left(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let n = count_arg(&values[1], cx).max(0) as usize;
    Ok(Value::text(
        values[0]
            .to_text(cx)
            .as_str()
            .chars()
            .take(n)
            .collect::<String>(),
    ))
}

fn middle(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let start = count_arg(&values[1], cx).max(1) as usize - 1;
    let n = count_arg(&values[2], cx).max(0) as usize;
    let text = values[0].to_text(cx);
    Ok(Value::text(
        text.as_str()
            .chars()
            .skip(start)
            .take(n)
            .collect::<String>(),
    ))
}

fn round(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    if values[0].is_empty() {
        return Ok(values[0].clone());
    }
    let digits = count_arg(&values[1], cx);
    Ok(Value::Number(values[0].numeric_operand(cx).round(digits)))
}

fn get_as_text(values: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(Value::Text(values[0].to_text(cx)))
}

/// The host for a battery's file (`file=` of the vectors, the suffix of a
/// result file), with the stand-ins.
pub fn host_for(file: &str) -> Option<MemoryHost> {
    let mut host = sample(file)?;
    add_stand_ins(&mut host);
    Some(host)
}

fn sample(file: &str) -> Option<MemoryHost> {
    Some(match file {
        "mbs-atb" => mbs_atb(),
        "mbs-phidget" => mbs_phidget(),
        "fms-sample-en" => fms_sample(),
        "mbs-snippets" => mbs_snippets(),
        "contacts-en" => contacts(Locale::en_us()),
        "contacts-de" => contacts(Locale::de_de()),
        "contacts-fr" => contacts(Locale::fr_fr()),
        "contacts-sv" => contacts(Locale::sv_se()),
        "contacts-ja" => contacts(Locale::ja_jp()),
        tag => contacts(Locale::for_tag(tag)?),
    })
}

/// Harness escapes (`\r`, `\n`, `\t`, `\\`, `\uXXXX`; others kept).
pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                chars.next();
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
                continue;
            }
            _ => {
                out.push('\\');
                continue;
            }
        }
        chars.next();
    }
    out
}

pub fn type_name(v: &Value) -> &'static str {
    match v.value_type() {
        ValueType::Text => "text",
        ValueType::Number => "number",
        ValueType::Date => "date",
        ValueType::Time => "time",
        ValueType::Timestamp => "timestamp",
        ValueType::Container => "container",
    }
}

/// Rows that recurse 5,000 or more custom function calls deep (x04, e03):
/// they run in release builds only, as a debug build takes minutes and
/// gigabytes of stack for them. `eval_host.rs` checks the call limits at
/// full depth with a function that is cheap in debug builds too.
pub fn too_slow_for_debug(expr: &str) -> bool {
    cfg!(debug_assertions)
        && expr.contains("__GEN_")
        && expr
            .split(|c: char| !c.is_ascii_digit())
            .any(|n| n.parse::<u64>().is_ok_and(|n| n >= 5000))
}

/// One expression evaluated through the real pipeline.
pub struct Outcome {
    pub text: String,
    pub code: i32,
    pub type_name: &'static str,
    /// Host functions the evaluation needed and the host does not answer
    /// (library functions not implemented yet, ExecuteSQL, ...): the row is
    /// not covered.
    pub unsupported: Vec<String>,
    /// Whether a stand-in answered part of it.
    pub stand_in: bool,
}

/// Compiles `expr` in the context's occurrence, as the harness's `Evaluate`
/// does, and evaluates it.
pub fn run(host: &mut MemoryHost, expr: &str, context: &Context) -> Outcome {
    host.unsupported.clear();
    host.calls.clear();
    let result = eval::evaluate_text(expr, host, context);
    let stand_in = host
        .calls
        .iter()
        .any(|c| STAND_INS.iter().any(|(name, _)| name == c));
    Outcome {
        text: eval::display(&result.value, host),
        code: result.code(),
        type_name: type_name(&result.value),
        unsupported: std::mem::take(&mut host.unsupported),
        stand_in,
    }
}

/// Counts per battery or file.
#[derive(Default)]
pub struct Tally {
    pub rows: usize,
    pub covered: usize,
    pub passed: usize,
    pub known: usize,
    /// Covered rows that needed a stand-in.
    pub stand_in: usize,
    /// Rows left to release builds (`too_slow_for_debug`).
    pub slow: usize,
}

pub fn print_tallies(title: &str, tallies: &BTreeMap<String, Tally>) {
    let mut total = Tally::default();
    println!("{title:<28} rows covered passed known stand-in release-only");
    for (name, t) in tallies {
        println!(
            "{name:<28} {:>4} {:>7} {:>6} {:>5} {:>8} {:>12}",
            t.rows, t.covered, t.passed, t.known, t.stand_in, t.slow
        );
        total.rows += t.rows;
        total.covered += t.covered;
        total.passed += t.passed;
        total.known += t.known;
        total.stand_in += t.stand_in;
        total.slow += t.slow;
    }
    println!(
        "{:<28} {:>4} {:>7} {:>6} {:>5} {:>8} {:>12}",
        "total", total.rows, total.covered, total.passed, total.known, total.stand_in, total.slow
    );
}
