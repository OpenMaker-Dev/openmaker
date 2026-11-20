//! The step catalogs of `docs/re/catalogs`: step names and categories
//! (`script-steps.csv`) and which clients support each step
//! (`script-step-compat.csv`, script-runtime.md 10.1).

use std::collections::BTreeMap;
use std::sync::OnceLock;

const STEPS: &str = include_str!("../../../docs/re/catalogs/script-steps.csv");
const COMPAT: &str = include_str!("../../../docs/re/catalogs/script-step-compat.csv");

/// The rows of a catalog CSV as maps from column name to value. Fields may be
/// quoted (with `""` for a quote); no field spans lines.
pub(crate) fn csv_rows(text: &str) -> Vec<BTreeMap<String, String>> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = split_csv(lines.next().unwrap_or_default());
    lines
        .map(|line| header.iter().cloned().zip(split_csv(line)).collect())
        .collect()
}

fn split_csv(line: &str) -> Vec<String> {
    let (mut out, mut field, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    out.push(field);
    out
}

/// A step id's name, category and description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepInfo {
    pub id: u16,
    /// Empty for ids FileMaker's string table does not name.
    pub name: String,
    pub category: String,
    pub description: String,
}

fn infos() -> &'static BTreeMap<u16, StepInfo> {
    static INFOS: OnceLock<BTreeMap<u16, StepInfo>> = OnceLock::new();
    INFOS.get_or_init(|| {
        csv_rows(STEPS)
            .into_iter()
            .filter_map(|mut row| {
                let id = row.get("id")?.parse().ok()?;
                let mut take = |k: &str| row.remove(k).unwrap_or_default();
                Some((
                    id,
                    StepInfo {
                        id,
                        name: take("name"),
                        category: take("category"),
                        description: take("description"),
                    },
                ))
            })
            .collect()
    })
}

/// Name, category and description of a step id.
pub fn step_info(id: u16) -> Option<&'static StepInfo> {
    infos().get(&id)
}

/// The step's English name, if FileMaker names the id.
pub fn step_name(id: u16) -> Option<&'static str> {
    step_info(id)
        .map(|info| info.name.as_str())
        .filter(|name| !name.is_empty())
}

/// Every step id the catalog lists (0..=250, FileMaker Pro 26.1.1's).
pub fn step_ids() -> impl Iterator<Item = u16> {
    infos().keys().copied()
}

/// Ids FileMaker 26 refuses with error 4: obsolete steps and the
/// feature-gated Execute URL (224) and Validate Scripts (239)
/// (script-runtime.md 2.5).
pub const OBSOLETE_STEP_IDS: [u16; 13] =
    [15, 52, 54, 58, 59, 78, 100, 153, 176, 198, 204, 224, 239];

/// Whether FileMaker accepts the step id at all: below 251 in FileMaker
/// Pro 26.1.1, below 248 in 26.0.3 (script-runtime.md 2.5). A valid id
/// without a step (248 among them) fails with error 3, as one without a
/// handler.
pub fn is_valid_step_id(id: u16) -> bool {
    id < 251
}

/// Whether FileMaker Server 26.0.3 accepts the step id: below 248
/// (script-runtime.md 2.5). OpenMaker Server's script runtimes keep its
/// bound until FileMaker Server 26.1.1 ships, so a step only FileMaker Pro
/// 26.1.1 knows fails there with error 4, as on 26.0.3.
pub fn is_server_step_id(id: u16) -> bool {
    id < 248
}

/// A client that runs scripts, by its `StateFlags` bit (script-runtime.md 10.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Client {
    Mac,
    Windows,
    Server,
    /// FileMaker Go (iOS).
    Go,
    /// Custom Web Publishing and the Data API.
    CustomWeb,
    WebDirect,
}

impl Client {
    pub const ALL: [Client; 6] = [
        Client::Mac,
        Client::Windows,
        Client::Server,
        Client::Go,
        Client::CustomWeb,
        Client::WebDirect,
    ];

    /// The client's `ScriptStep::StateFlags` bit.
    pub fn bit(self) -> u32 {
        match self {
            Client::Mac => 0x2,
            Client::Windows => 0x4,
            Client::Server => 0x8,
            Client::Go => 0x10,
            Client::CustomWeb => 0x20,
            Client::WebDirect => 0x40,
        }
    }
}

/// `StateFlags` bit of steps that cannot be a button's single step.
pub const NOT_BUTTON_COMPATIBLE: u32 = 0x80;

/// A Claris reference compatibility column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Yes,
    Partial,
    No,
}

impl Support {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "Yes" => Some(Self::Yes),
            "Partial" => Some(Self::Partial),
            "No" => Some(Self::No),
            _ => None,
        }
    }
}

/// The Claris reference's compatibility columns for a step (`None` where
/// the reference has no page or no entry).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClarisSupport {
    pub pro: Option<Support>,
    pub go: Option<Support>,
    pub webdirect: Option<Support>,
    pub server: Option<Support>,
    pub cloud: Option<Support>,
    pub data_api: Option<Support>,
    pub custom_web: Option<Support>,
}

/// One row of the compatibility table: the engine's own bits
/// (`sStepsUnstoredFlagsTable`), the Claris columns, and what FileMaker
/// Server's script engine does with the step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepCompat {
    pub id: u16,
    /// `StateFlags`: 2 Mac, 4 Windows, 8 Server, 0x10 Go, 0x20 Custom Web,
    /// 0x40 WebDirect, 0x80 not usable as a button's single step.
    pub state_flags: u32,
    pub claris: ClarisSupport,
    /// The server script engine's outcome, e.g. `common handler` or
    /// `error 3 (no handler)`.
    pub server_dispatch: String,
    pub notes: String,
}

impl StepCompat {
    /// Whether the engine's table marks the step as supported by `client`.
    pub fn supports(&self, client: Client) -> bool {
        self.state_flags & client.bit() != 0
    }

    /// The clients the engine's table marks as supporting the step.
    pub fn clients(&self) -> Vec<Client> {
        Client::ALL
            .into_iter()
            .filter(|c| self.supports(*c))
            .collect()
    }

    /// Whether the step can be a button's single step.
    pub fn button_compatible(&self) -> bool {
        self.state_flags & NOT_BUTTON_COMPATIBLE == 0
    }
}

fn compats() -> &'static BTreeMap<u16, StepCompat> {
    static COMPATS: OnceLock<BTreeMap<u16, StepCompat>> = OnceLock::new();
    COMPATS.get_or_init(|| {
        csv_rows(COMPAT)
            .into_iter()
            .filter_map(|row| {
                let get = |k: &str| row.get(k).map(String::as_str).unwrap_or_default();
                let id = get("step_id").parse().ok()?;
                let state_flags =
                    u32::from_str_radix(get("state_flags_hex").trim_start_matches("0x"), 16)
                        .ok()?;
                let claris = |k: &str| Support::parse(get(k));
                Some((
                    id,
                    StepCompat {
                        id,
                        state_flags,
                        claris: ClarisSupport {
                            pro: claris("claris_pro"),
                            go: claris("claris_go"),
                            webdirect: claris("claris_webdirect"),
                            server: claris("claris_server"),
                            cloud: claris("claris_cloud"),
                            data_api: claris("claris_data_api"),
                            custom_web: claris("claris_cwp"),
                        },
                        server_dispatch: get("server_script_engine_dispatch").into(),
                        notes: get("notes").into(),
                    },
                ))
            })
            .collect()
    })
}

/// The compatibility row of a step id.
pub fn step_compat(id: u16) -> Option<&'static StepCompat> {
    compats().get(&id)
}

/// The clients that support a step id (empty for ids the table lacks).
pub fn supported_clients(id: u16) -> Vec<Client> {
    step_compat(id).map(StepCompat::clients).unwrap_or_default()
}

/// Whether `client` supports the step id. On the server an unsupported step
/// is skipped with error 3 and the script continues (script-runtime.md 10.2).
pub fn is_supported(id: u16, client: Client) -> bool {
    step_compat(id).is_some_and(|c| c.supports(client))
}

/// [`is_supported`] by FileMaker Server 26.0.3's table, which OpenMaker
/// Server's script runtimes keep until FileMaker Server 26.1.1 ships: its
/// steps only, and without the Server bit 26.1.1 gives Save a Copy as
/// Add-on Package (96) (script-runtime.md 10.1).
pub fn is_supported_on_server(id: u16, client: Client) -> bool {
    is_server_step_id(id) && is_supported(id, client) && !(client == Client::Server && id == 96)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_categories_come_from_the_catalog() {
        assert_eq!(step_name(1), Some("Perform Script"));
        assert_eq!(step_name(186), None);
        assert_eq!(step_info(1).unwrap().category, "Control");
        // FileMaker Pro 26.1.1's ids: 248 unnamed, 249 and 250 its new
        // steps, 147 spelled anew (script-steps.md 12.4).
        assert_eq!(step_ids().count(), 251);
        assert_eq!(step_name(248), None);
        assert_eq!(step_name(250), Some("Set Variable by Name"));
        assert_eq!(step_name(147), Some("Set Field by Name"));
        assert_eq!(split_csv(r#"a,"b, ""c""",d"#), ["a", r#"b, "c""#, "d"]);
    }

    #[test]
    fn compatibility_follows_the_engine_table() {
        assert_eq!(supported_clients(1), Client::ALL);
        // Save a Copy as XML: Mac, Windows and Server only (0x0e).
        assert_eq!(
            supported_clients(3),
            [Client::Mac, Client::Windows, Client::Server]
        );
        assert!(!is_supported(2, Client::Server));
        // 83 named steps lack the Server bit: 26.0.3's 84 (script-steps.md
        // 7) less Save a Copy as Add-on Package (96), which 26.1.1 runs on
        // the server; its two new steps run on every client.
        let unsupported = step_ids()
            .filter(|id| step_name(*id).is_some() && !is_supported(*id, Client::Server))
            .count();
        assert_eq!(unsupported, 83);
        assert_eq!(supported_clients(250), Client::ALL);
        assert!(supported_clients(248).is_empty());
        // OpenMaker Server keeps FileMaker Server 26.0.3's table.
        assert!(!is_supported_on_server(96, Client::Server));
        assert!(!is_supported_on_server(250, Client::Server));
        assert!(is_supported_on_server(3, Client::Server));
        let comment = step_compat(89).unwrap();
        assert!(!comment.button_compatible());
        assert!(step_compat(1).unwrap().button_compatible());
        assert_eq!(
            step_compat(6).unwrap().claris.server,
            Some(Support::Partial)
        );
    }
}
