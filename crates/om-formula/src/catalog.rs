//! The opcode catalog: every function, operator, keyword and Get selector
//! with its stored opcode, display name and argument counts.
//!
//! Embedded from `docs/re/catalogs/calc-tokens.csv` (calc-encoding.md 9) and,
//! for the Get selector names FileMaker displays (string resource 131),
//! `docs/re/catalogs/get-selectors.csv`. Names match case-insensitively
//! (calc-encoding.md 6.1).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::operand::PluginFunctionId;

const TOKENS: &str = include_str!("../../../docs/re/catalogs/calc-tokens.csv");
const SELECTORS: &str = include_str!("../../../docs/re/catalogs/get-selectors.csv");

/// A `DBOpcode`: the token of a function, operator, keyword or operand kind.
/// Stored as one byte below 248, or a prefix byte `f8`..`fe` and an index for
/// the newer function ranges (calc-encoding.md 2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Opcode(pub u16);

impl Opcode {
    pub const EMPTY: Self = Self(0);
    pub const SEPARATOR: Self = Self(3);
    pub const LEFT_PAREN: Self = Self(4);
    pub const RIGHT_PAREN: Self = Self(5);
    pub const LEFT_BRACKET: Self = Self(6);
    pub const RIGHT_BRACKET: Self = Self(7);
    pub const SYMBOL: Self = Self(0x0b);
    pub const WHITESPACE: Self = Self(0x0c);
    pub const BLOCK_COMMENT: Self = Self(0x0d);
    pub const LINE_COMMENT: Self = Self(0x0e);
    pub const NUMBER: Self = Self(0x10);
    pub const BOOLEAN: Self = Self(0x11);
    pub const TEXT: Self = Self(0x13);
    pub const KEY: Self = Self(0x14);
    pub const PLUGIN_ID: Self = Self(0x15);
    pub const FIELD: Self = Self(0x16);
    pub const FIELD_REF: Self = Self(0x17);
    pub const LOCAL: Self = Self(0x18);
    pub const LIST: Self = Self(0x19);
    pub const VARIABLE: Self = Self(0x1a);
    pub const CALL: Self = Self(0x1b);
    pub const TAIL_CALL: Self = Self(0x1c);
    pub const PLUGIN_CALL: Self = Self(0x1d);
    pub const NOT: Self = Self(0x1e);
    pub const AND: Self = Self(0x1f);
    pub const OR: Self = Self(0x20);
    pub const XOR: Self = Self(0x21);
    pub const PARAGRAPH: Self = Self(0x24);
    pub const ADD: Self = Self(0x25);
    pub const SUBTRACT: Self = Self(0x26);
    pub const MULTIPLY: Self = Self(0x27);
    pub const DIVIDE: Self = Self(0x28);
    pub const POWER: Self = Self(0x29);
    pub const NEGATE: Self = Self(0x2c);
    pub const SET_PRECISION: Self = Self(0x3d);
    pub const SET_RECURSION: Self = Self(0x40);
    pub const LESS: Self = Self(0x41);
    pub const LESS_OR_EQUAL: Self = Self(0x42);
    pub const LESS_OR_EQUAL_SIGN: Self = Self(0x43);
    pub const EQUAL: Self = Self(0x44);
    pub const NOT_EQUAL: Self = Self(0x45);
    pub const NOT_EQUAL_SIGN: Self = Self(0x46);
    pub const GREATER_OR_EQUAL: Self = Self(0x47);
    pub const GREATER_OR_EQUAL_SIGN: Self = Self(0x48);
    pub const GREATER: Self = Self(0x49);
    pub const IF: Self = Self(0x4b);
    pub const CASE: Self = Self(0x4c);
    pub const CHOOSE: Self = Self(0x4d);
    pub const CONCAT: Self = Self(0x50);
    pub const SUBSTITUTE: Self = Self(0x63);
    pub const GET_AS_NUMBER: Self = Self(0x68);
    pub const GET_SUMMARY: Self = Self(0x86);
    pub const GET_REPETITION: Self = Self(0x89);
    pub const GET: Self = Self(0x9b);
    pub const GET_SELECTOR: Self = Self(0x9c);
    pub const EVALUATE: Self = Self(0xdc);
    pub const GET_FIELD: Self = Self(0xdd);
    pub const LET: Self = Self(0xdf);
    pub const LOOKUP: Self = Self(0xe0);
    pub const LOOKUP_NEXT: Self = Self(0xe1);
    /// `SetVariable ( value ; repetition ; target )`, compiled from a Let
    /// binding that assigns a `$` variable or repeats a name (5.5).
    pub const SET_VARIABLE: Self = Self(0xe3);
    /// `$v[n]`: `GetVarRep ( repetition ; variable )` (5.4).
    pub const GET_VAR_REP: Self = Self(0xe4);
    pub const WHILE: Self = Self(2000);
    pub const SELF: Self = Self(2900);
    pub const JSON_SET_ELEMENT: Self = Self(2307);

    /// The catalog row of this opcode.
    pub fn info(self) -> Option<&'static OpInfo> {
        catalog().ops.get(&self.0)
    }

    /// The name FileMaker displays: the function or keyword name, or the
    /// operator's characters.
    pub fn name(self) -> Option<&'static str> {
        self.info().map(|i| i.name).filter(|n| !n.is_empty())
    }
}

/// What a catalog row is (`kind` column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Operator,
    Constant,
    /// Operand kinds: numbers, text, fields, variables, calls.
    Data,
    Punctuation,
    /// Formatted-stream only: whitespace and comments.
    Layout,
    Internal,
    Unused,
}

/// `DBCalcSolver::GetNumberOfArgs`: how the Polish stream stores a node's
/// arguments (calc-encoding.md 3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NArgs {
    Unknown,
    /// The opcode alone.
    NoArgs,
    /// A fixed number of arguments in source order, no count.
    Unary,
    Binary,
    Trinary,
    Quad,
    /// Like `Unary`/`Binary`, with a field-reference first argument (5.3).
    UnaryField,
    BinaryField,
    /// A u16 count, then the arguments last first.
    VarArgs,
    /// A form of its own (section 3.1).
    Special,
}

/// One catalog row.
#[derive(Clone, Debug)]
pub struct OpInfo {
    pub opcode: Opcode,
    /// The source name (string resource 130-145), or the operator's
    /// characters; empty for operand kinds.
    pub name: &'static str,
    pub kind: Kind,
    pub nargs: NArgs,
    /// `DBCalcSolver::Arity`: arguments read without a count.
    pub arity: u8,
    /// `DBCalcParse::GetMinMaxArgs`; `max_args` 0 means unlimited.
    pub min_args: u16,
    pub max_args: u16,
}

/// A built-in plug-in function: the Design functions (`FMDR`) and the mobile
/// functions (`FMMB`), stored as plug-in calls (calc-encoding.md 7).
#[derive(Clone, Debug)]
pub struct Builtin {
    pub id: PluginFunctionId,
    pub name: &'static str,
    pub min_args: u16,
    /// `None` when unlimited.
    pub max_args: Option<u16>,
}

struct Catalog {
    ops: HashMap<u16, OpInfo>,
    /// Lowercased names of functions, constants and word operators.
    names: HashMap<String, Vec<Opcode>>,
    selectors: Vec<&'static str>,
    selector_names: HashMap<String, u8>,
    builtins: Vec<Builtin>,
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(Catalog::load)
}

/// Split one CSV line, honouring double-quoted fields (the catalogs quote
/// fields that hold commas; names are never quoted).
fn columns(line: &'static str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                out.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&line[start..]);
    out
}

/// Required and optional parameter counts from a signature such as
/// `Location ( accuracy {; timeout} )` or `WindowNames {( fileName )}`.
fn signature_counts(signature: &str) -> (u16, Option<u16>) {
    let Some(open) = signature.find(['(', '{']) else {
        return (0, Some(0));
    };
    let params = &signature[open..];
    let (mut required, mut optional, mut depth) = (0u16, 0u16, 0);
    let mut in_param = false;
    for c in params.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            '(' | ')' | ';' | ' ' => {}
            '.' => return (required, None),
            _ if !in_param => {
                in_param = true;
                if depth > 0 {
                    optional += 1;
                } else {
                    required += 1;
                }
            }
            _ => {}
        }
        if matches!(c, ';' | ')') {
            in_param = false;
        }
    }
    (required, Some(required + optional))
}

impl Catalog {
    fn load() -> Self {
        let mut ops = HashMap::new();
        let mut names: HashMap<String, Vec<Opcode>> = HashMap::new();
        let mut builtins = Vec::new();
        for line in TOKENS.lines().skip(1) {
            let c = columns(line);
            let (opcode, stored, source, kind) = (c[0], c[1], c[4], c[5]);
            let number = |s: &str| s.parse::<u16>().unwrap_or(0);
            if kind == "builtin-plugin" {
                // `1d nnnn 15 464d4452 0001`: the plug-in id and function id.
                let parts: Vec<&str> = stored.split(' ').collect();
                let hex = |s: &str| u32::from_str_radix(s, 16).unwrap_or(0);
                let (min_args, max_args) = signature_counts(c[12]);
                builtins.push(Builtin {
                    id: PluginFunctionId {
                        plugin: hex(parts[3]).to_be_bytes(),
                        function: hex(parts[4]) as u16,
                    },
                    name: source,
                    min_args,
                    max_args,
                });
                continue;
            }
            let Ok(op) = opcode.parse::<u16>() else {
                continue;
            };
            let kind = match kind {
                "function" => Kind::Function,
                "operator" => Kind::Operator,
                "constant" => Kind::Constant,
                "data" => Kind::Data,
                "punctuation" => Kind::Punctuation,
                "layout" => Kind::Layout,
                "unused" => Kind::Unused,
                "get-selector" => continue,
                _ => Kind::Internal,
            };
            let nargs = match c[6] {
                "NoArgs" => NArgs::NoArgs,
                "Unary" => NArgs::Unary,
                "Binary" => NArgs::Binary,
                "Trinary" => NArgs::Trinary,
                "Quad" => NArgs::Quad,
                "UnaryField" => NArgs::UnaryField,
                "BinaryField" => NArgs::BinaryField,
                "VarArgs" => NArgs::VarArgs,
                "Special" => NArgs::Special,
                _ => NArgs::Unknown,
            };
            let name = match kind {
                Kind::Function | Kind::Constant | Kind::Operator | Kind::Punctuation => {
                    // Operator rows describe unary minus as `- (unary)`.
                    source.split(" (").next().unwrap_or(source)
                }
                _ => "",
            };
            if matches!(kind, Kind::Function | Kind::Constant | Kind::Operator)
                && name.starts_with(|c: char| c.is_ascii_alphabetic())
            {
                names
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(Opcode(op));
            }
            ops.insert(
                op,
                OpInfo {
                    opcode: Opcode(op),
                    name,
                    kind,
                    nargs,
                    arity: c[7].parse().unwrap_or(0),
                    min_args: number(c[8]),
                    max_args: number(c[9]),
                },
            );
        }
        let mut selectors = Vec::new();
        let mut selector_names = HashMap::new();
        for line in SELECTORS.lines().skip(1) {
            let c = columns(line);
            let (Ok(index), name) = (c[0].parse::<u8>(), c[1]) else {
                continue;
            };
            if selectors.len() <= usize::from(index) {
                selectors.resize(usize::from(index) + 1, "");
            }
            selectors[usize::from(index)] = name;
            if !name.is_empty() {
                selector_names.insert(name.to_lowercase(), index);
            }
        }
        Self {
            ops,
            names,
            selectors,
            selector_names,
            builtins,
        }
    }
}

/// Functions, constants and word operators (`and`, `not`, ...) named `name`,
/// compared case-insensitively. Two names have two rows each: `Extend` and
/// `Lower` are both a function and a text-style constant.
pub fn named(name: &str) -> &'static [Opcode] {
    catalog()
        .names
        .get(&name.to_lowercase())
        .map_or(&[], Vec::as_slice)
}

/// The name of Get selector `selector` as FileMaker displays it (resource
/// 131), e.g. `UUID` for 101.
pub fn get_selector_name(selector: u8) -> Option<&'static str> {
    catalog()
        .selectors
        .get(usize::from(selector))
        .copied()
        .filter(|n| !n.is_empty())
}

/// The Get selector named `name`, case-insensitively.
pub fn find_get_selector(name: &str) -> Option<u8> {
    catalog().selector_names.get(&name.to_lowercase()).copied()
}

/// The built-in plug-in function with this id (`FMDR`, `FMMB`).
pub fn builtin(id: PluginFunctionId) -> Option<&'static Builtin> {
    catalog().builtins.iter().find(|b| b.id == id)
}

/// The built-in plug-in function named `name`, case-insensitively.
pub fn find_builtin(name: &str) -> Option<&'static Builtin> {
    catalog()
        .builtins
        .iter()
        .find(|b| b.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_rows_load() {
        let left = Opcode(0x57).info().unwrap();
        assert_eq!(
            (left.name, left.nargs, left.arity),
            ("Left", NArgs::Binary, 2)
        );
        assert_eq!(Opcode::WHILE.name(), Some("While"));
        assert_eq!(Opcode(3501).name(), Some("JSONGetElement"));
        assert_eq!(Opcode::NEGATE.name(), Some("-"));
        assert_eq!(Opcode(0x63).info().unwrap().nargs, NArgs::VarArgs);
        assert_eq!(named("EXTEND"), &[Opcode(136), Opcode(175)]);
        assert_eq!(named("not"), &[Opcode::NOT]);
    }

    #[test]
    fn get_selectors_use_resource_131_names() {
        assert_eq!(get_selector_name(101), Some("UUID"));
        assert_eq!(get_selector_name(63), Some("CurrentTimestamp"));
        assert_eq!(find_get_selector("currenttimestamp"), Some(63));
        assert_eq!(get_selector_name(0), None);
    }

    #[test]
    fn builtin_plugins_come_with_argument_counts() {
        let layouts = find_builtin("layoutnames").unwrap();
        assert_eq!(&layouts.id.plugin, b"FMDR");
        assert_eq!(
            (layouts.id.function, layouts.min_args, layouts.max_args),
            (10, 1, Some(1))
        );
        let windows = find_builtin("WindowNames").unwrap();
        assert_eq!((windows.min_args, windows.max_args), (0, Some(1)));
        let beacons = find_builtin("RangeBeacons").unwrap();
        assert_eq!((beacons.min_args, beacons.max_args), (1, Some(4)));
        assert_eq!(find_builtin("DatabaseNames").unwrap().max_args, Some(0));
    }
}
