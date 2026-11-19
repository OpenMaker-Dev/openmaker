//! The Design functions and layout object functions of a server script
//! session (calc-functions.md 5.8), answered as FileMaker Server's script
//! engine answers them: from the file's whole schema with its layouts and
//! scripts (`om_data::host` Design functions, read once per schema
//! generation when a calculation first asks), the session's records for
//! value lists, the hosted file's next serial values, and the session's
//! window for `WindowNames` and `GetLayoutObjectAttribute` (layout
//! coordinates: the server's window has no toolbar).

use std::sync::Arc;

use om_calc::eval::{default_call, Context, HostFunction};
use om_calc::{CalcError, Value};
use om_data::host::layout_objects::{self, ObjectState, ObjectWindow};
use om_data::host::{read_field, CalcSchema, FileCalc, LayoutEntry};
use om_hbam::slot::uint_segment;
use om_hbam::{KeyTree, OwnedNode, SlotReader};
use om_layout::LayoutObject;
use om_scripts::runtime::Mode;

use super::related::SessionSource;
use super::ServerHost;
use crate::store::Store;

/// Error 10, "Requested data is missing": `LayoutObjectUUID` outside a
/// layout object's calculation.
const DATA_MISSING: u16 = 10;

/// The catalogs the Design functions read beyond the record rules': the
/// file's locale and options `[2]`, layouts `[4]` with their themes `[6]`
/// and fonts `[25]` (a portal's borders place its fields), custom
/// functions `[16]`, scripts `[17]`, external data sources `[32]` and
/// value lists `[33]`, with `[3]` and every table's fields `[T, 3]`.
fn design_catalogs(store: &Store) -> Option<Vec<u8>> {
    let catalog = store.catalog();
    let mut children: Vec<OwnedNode> = [2u8, 3, 4, 6, 16, 17, 25, 0x20, 0x21]
        .iter()
        .filter_map(|key| store.node(&[&[*key]]).cloned())
        .collect();
    for table in &catalog.tables {
        let t = uint_segment(table.id);
        if let Some(fields) = store.node(&[&t, &[3]]) {
            children.push(OwnedNode::list(t, vec![fields.clone()]));
        }
    }
    children.sort_by(|a, b| a.segment.cmp(&b.segment));
    OwnedNode::list(Vec::new(), children).to_wire()
}

/// The file's calculation schema with its layouts (objects, names), script
/// list and data sources, for the Design functions.
pub(super) fn design_schema(store: &Store, file: &str) -> Option<Arc<CalcSchema>> {
    let wire = design_catalogs(store)?;
    let tree = KeyTree::from_slots(SlotReader::new(&wire)).ok()?;
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let mut schema = om_data::rules::schema::calc_schema(&tree, store.catalog(), stem);
    schema.locale = store.locale();
    Some(Arc::new(schema))
}

/// The layout a session opens on: the file's "Switch to layout" when it is
/// on (File Options, file-metadata.md 3), as FileMaker Server's sessions
/// open on Contact Details in the Contacts template.
pub(super) fn opening_layout(store: &Store) -> Option<u64> {
    let prefs = store.node(&[&[2]])?.clone();
    let wire = OwnedNode::list(Vec::new(), vec![prefs]).to_wire()?;
    let tree = KeyTree::from_slots(SlotReader::new(&wire)).ok()?;
    om_data::meta::file_options(&tree)?.default_layout()
}

impl ServerHost {
    /// The Design view of the file's schema, built when first needed.
    pub(super) fn design(&self) -> Option<Arc<CalcSchema>> {
        self.schema
            .design
            .get_or_init(|| {
                let store = self.hosted.read_store();
                design_schema(&store, &self.hosted.name)
            })
            .clone()
    }

    /// The window's layout in the Design view.
    pub(super) fn window_layout(&self, schema: &CalcSchema) -> Option<LayoutEntry> {
        let window = self.window.as_ref()?;
        let id = self.schema.layouts.get(window.layout)?.id;
        schema
            .layouts()
            .iter()
            .find(|l| u64::from(l.id) == id)
            .cloned()
    }

    /// Functions the session answers: the Design functions,
    /// `GetLayoutObjectAttribute`, `LayoutObjectUUID`; the rest as any
    /// host without them.
    pub(super) fn host_call(
        &mut self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> Result<Value, CalcError> {
        match function {
            HostFunction::Builtin(builtin) => {
                if let Some(answer) = self.design_call(builtin.name, args, context) {
                    return answer;
                }
            }
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetLayoutObjectAttribute") => {
                return Ok(self.layout_object_attribute(args));
            }
            HostFunction::Named(name) if name.eq_ignore_ascii_case("LayoutObjectUUID") => {
                return Err(CalcError::from_code(DATA_MISSING).expect("nonzero"));
            }
            // FileMaker Server found no object (calc-functions.md 5.8).
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetLayoutObjectOwnerInfo") => {
                return Ok(Value::text(layout_objects::owner_info_not_found()));
            }
            _ => {}
        }
        default_call(function)
    }

    fn design_call(
        &self,
        name: &str,
        args: &[Value],
        context: &Context,
    ) -> Option<Result<Value, CalcError>> {
        let schema = self.design()?;
        let source = SessionSource { host: self };
        let mut session = self.session_info();
        session.window_name.clone_from(&session.file_name);
        if let Some(layout) = self.window_layout(&schema) {
            session.layout_name = layout.name;
        }
        let file = FileCalc::new(&schema, &source, self.today).with_session(&session);
        if name.eq_ignore_ascii_case("GetNextSerialValue") {
            return Some(Ok(self.next_serial_value(&file, args, context)));
        }
        file.design(name, args, context)
    }

    /// `GetNextSerialValue`: the hosted file's current next value
    /// (`[T, 3, 5, F, 0c]`, advanced by every record the session creates),
    /// empty for a field without a serial number.
    fn next_serial_value(&self, file: &FileCalc<'_>, args: &[Value], context: &Context) -> Value {
        let cx = self.cx();
        let text = |i: usize| {
            args.get(i)
                .map(|v| v.to_text(&cx).into_string())
                .unwrap_or_default()
        };
        let schema = file.schema;
        if !file.names_this_file(&text(0)) {
            return Value::empty_text();
        }
        let name = text(1);
        let (occurrence, field) = match name.split_once("::") {
            Some((occurrence, field)) => (schema.occurrence_named(occurrence).map(|o| o.id), field),
            None => (context.occurrence, name.as_str()),
        };
        let Some(table) = occurrence.and_then(|o| schema.table_of(o)) else {
            return Value::empty_text();
        };
        let Some(def) = table
            .fields
            .values()
            .find(|f| f.name.to_lowercase() == field.to_lowercase())
        else {
            return Value::empty_text();
        };
        if def.next_serial.is_none() {
            return Value::empty_text();
        }
        let store = self.hosted.read_store();
        let (t, f) = (uint_segment(table.id), uint_segment(def.id));
        let live = store
            .node(&[&t, &[3], &[5], &f, &[0x0c]])
            .and_then(crate::scripts::values::cell_text);
        Value::text(live.or_else(|| def.next_serial.clone()).unwrap_or_default())
    }

    /// `GetLayoutObjectAttribute` on the window's layout, in layout
    /// coordinates, on the window's current record.
    fn layout_object_attribute(&mut self, args: &[Value]) -> Value {
        let cx = self.cx();
        let text = |i: usize| {
            args.get(i)
                .map(|v| v.to_text(&cx).into_string())
                .unwrap_or_default()
        };
        let number = |i: usize| {
            args.get(i)
                .and_then(|v| v.to_number(&cx).to_i64())
                .map_or(0, |n| u32::try_from(n.max(0)).unwrap_or(u32::MAX))
        };
        let (name, attribute, repetition, row) = (text(0), text(1), number(2), number(3));
        let Some(schema) = self.design() else {
            return Value::empty_text();
        };
        let Some(layout) = self.window_layout(&schema).and_then(|l| l.layout) else {
            return Value::empty_text();
        };
        let window = self.window.as_ref();
        // A list or table without records draws no body.
        let list_view = layout.def.as_ref().is_some_and(|d| d.flags & 0x4200 != 0);
        let body_shown =
            !list_view || window.is_some_and(|w| !w.found.is_empty() && w.mode != Mode::Find);
        // The focused field's first object.
        let focus = self.focus.as_ref().and_then(|f| {
            layout_objects::objects_in_order(&layout)
                .into_iter()
                .find(|o| {
                    o.field_spec().is_some_and(|s| {
                        om_layout::occurrence_id(s.table_key) == Some(f.occurrence)
                            && om_hbam::serial::raw_of_id(s.field_key) == Some(f.field)
                    })
                })
                .map(|o| o.id)
        });
        let object_window = ObjectWindow {
            layout: &layout,
            screen: None,
            focus,
            body_shown,
        };
        let mut state = WindowState { host: self };
        layout_objects::layout_object_attribute(
            &object_window,
            &schema,
            &name,
            &attribute,
            repetition,
            row,
            &mut state,
        )
    }
}

/// The window's current record as `GetLayoutObjectAttribute` reads it.
struct WindowState<'h> {
    host: &'h mut ServerHost,
}

impl ObjectState for WindowState<'_> {
    fn content(&mut self, object: &LayoutObject, repetition: u32, row: u32) -> String {
        let host = &mut *self.host;
        let Some((occurrence, record)) = host
            .window
            .as_ref()
            .and_then(|w| Some((w.occurrence, w.record()?)))
        else {
            return String::new();
        };
        let Some(spec) = object.field_spec() else {
            return String::new();
        };
        let (Some(field_occurrence), Some(field)) = (
            om_layout::occurrence_id(spec.table_key),
            om_hbam::serial::raw_of_id(spec.field_key),
        ) else {
            return String::new();
        };
        // A related field shows the portal row's record, else the first.
        let index = row.max(1) as usize - 1;
        let target = if field_occurrence == occurrence {
            Some(record)
        } else {
            host.related_set(occurrence, record, field_occurrence)
                .ok()
                .and_then(|records| records.get(index).copied())
        };
        let Some(target) = target else {
            return String::new();
        };
        let value = read_field(host, field_occurrence, target, field, repetition.max(1));
        om_calc::eval::display(&value, &*host)
    }

    fn hidden(&mut self, object: &LayoutObject) -> bool {
        let Some(hide) = object.calcs.hide.clone() else {
            return false;
        };
        let host = &mut *self.host;
        let context = match host
            .window
            .as_ref()
            .and_then(|w| Some((w.occurrence, w.record()?)))
        {
            Some((occurrence, record)) => Context::record(occurrence, record),
            None => host
                .window
                .as_ref()
                .map(|w| Context {
                    occurrence: Some(w.occurrence),
                    ..Context::new()
                })
                .unwrap_or_default(),
        };
        om_data::host::layout::holds(host, &hide, &context)
    }
}

#[cfg(test)]
pub(super) mod tests {
    //! FileMaker Server's Design answers (`om-data/tests/data/design`,
    //! battery `s30-design`) replayed through a server script session on
    //! the same files, hosted. Ignored by default: the files exist only on
    //! the analysis host (`OM_HARNESS_FILES`).
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use om_calc::eval::{evaluate_text, CalcHost, Context};
    use om_calc::Ctx;
    use ring::rand::SystemRandom;

    use super::super::ServerHost;
    use crate::config::{HostedFile, Privileges, ServerIdentity};
    use crate::connection::Shared;
    use crate::hosting::Hosting;
    use crate::scripts::{ScriptEngine, ScriptLimits};

    pub(in crate::scripts::host) const FILES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../re/work/fms-harness/files"
    );

    pub(in crate::scripts::host) fn unescape(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                }
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        }
        out
    }

    /// A session on a hosted copy of `file`, named `work.fmp12` as the
    /// harness's copy is.
    pub(in crate::scripts::host) fn session(file: &Path, dir: &Path) -> ServerHost {
        let path = dir.join("work.fmp12");
        std::fs::copy(file, &path).unwrap();
        let hosting = Hosting::load(
            &[HostedFile {
                name: "work.fmp12".into(),
                path,
            }],
            false,
            &Default::default(),
        )
        .unwrap();
        let shared = Arc::new(Shared {
            identity: ServerIdentity {
                name: "openmaker-test".into(),
                version_string: "OpenMaker Server test".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            hosting,
            local: false,
            login_limits: Default::default(),
            custom_sets: Default::default(),
            rng: SystemRandom::new(),
            connection_ids: Default::default(),
            recordings: None,
            mailboxes: Default::default(),
            scripts: ScriptEngine::new(ScriptLimits::DEFAULT),
            refused_downloads: Default::default(),
            omitted_list_paths: Default::default(),
            download_overrides: Default::default(),
            shutdown: Default::default(),
            late_index_builds: Default::default(),
            unterminated_query_replies: Default::default(),
            started: std::time::SystemTime::now(),
            block_new_users: Default::default(),
            clients: Default::default(),
            events: Default::default(),
            open_delays: Default::default(),
            sockets: Default::default(),
            listed: Default::default(),
            file_list_override: Default::default(),
            no_file_list: Default::default(),
            no_get_file_list: Default::default(),
            no_authoring: Default::default(),
            upload_memory: crate::uploads::UploadMemory::new(usize::MAX),
            #[cfg(debug_assertions)]
            test_panic: Default::default(),
            #[cfg(debug_assertions)]
            test_refusal: Default::default(),
            #[cfg(debug_assertions)]
            script_gate: Default::default(),
            #[cfg(debug_assertions)]
            failing_timestamps: Default::default(),
        });
        let hosted = shared.hosting.by_name("work.fmp12").unwrap().clone();
        ServerHost::new(shared, hosted, 0x55, "Admin", Privileges::FullAccess)
    }

    #[test]
    #[ignore = "requires the harness's sample and probe files"]
    fn design_functions_answer_as_filemaker_server_in_a_script_session() {
        let files = std::env::var_os("OM_HARNESS_FILES")
            .map_or_else(|| PathBuf::from(FILES), PathBuf::from);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../om-data/tests/data/design");
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&fixtures)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "tsv"))
            .collect();
        paths.sort();
        let (mut total, mut failures) = (0, Vec::new());
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            let header = |key: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(&format!("# {key}\t")))
                    .unwrap()
                    .to_owned()
            };
            let dir = std::env::temp_dir().join(format!("om-design-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut host = session(&files.join(header("file")), &dir);
            let layout = |host: &mut ServerHost| {
                CalcHost::get(host, 10, &Context::new())
                    .unwrap()
                    .to_text(&Ctx::new(host.locale(), host.today()))
                    .into_string()
            };
            // FileMaker Server's session ran the file's OnFirstWindowOpen
            // script, which may change layouts (FMS sample: Startup Screen
            // to Tasks); the file's "Switch to layout" is the session's own.
            if layout(&mut host) != header("layout") {
                use om_scripts::runtime::{LayoutTarget, Navigation};
                host.go_to_layout(&LayoutTarget::Name(header("layout")), None)
                    .unwrap();
            }
            assert_eq!(layout(&mut host), header("layout"), "{name}: the layout");
            let context = if name.ends_with(".script.tsv") {
                let w = host.window.as_ref().unwrap();
                Context {
                    occurrence: Some(w.occurrence),
                    record: w.record(),
                    ..Context::new()
                }
            } else {
                Context::new()
            };
            for line in text.lines().filter(|l| !l.starts_with('#')) {
                let Some((expression, expected)) = line.split_once('\t') else {
                    continue;
                };
                total += 1;
                let expected = unescape(expected);
                let result = evaluate_text(expression, &mut host, &context);
                let cx = Ctx::new(host.locale(), host.today());
                let mut got = result.value.to_text(&cx).into_string();
                if result.code() != 0 {
                    got = if expected.contains(' ') {
                        format!("#{} {got}", result.code())
                    } else {
                        format!("#{}", result.code())
                    };
                }
                if got != expected {
                    failures.push(format!(
                        "{name}: {expression}\n  want {expected:?}\n  got  {got:?}"
                    ));
                }
            }
            host.close();
            let _ = std::fs::remove_dir_all(&dir);
        }
        for f in &failures {
            println!("{f}");
        }
        println!("{} of {total} rows match", total - failures.len());
        assert!(failures.is_empty(), "{} rows differ", failures.len());
    }

    #[test]
    #[ignore = "requires the harness's sample files"]
    fn layout_objects_answer_as_filemaker_server_in_a_script_session() {
        use om_scripts::runtime::{LayoutTarget, Navigation};
        let files = std::env::var_os("OM_HARNESS_FILES")
            .map_or_else(|| PathBuf::from(FILES), PathBuf::from);
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../om-data/tests/data/layout-objects");
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&fixtures)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        paths.sort();
        let (mut total, mut failures) = (0, Vec::new());
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            let header = |key: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(&format!("# {key}\t")))
                    .unwrap()
                    .to_owned()
            };
            let dir =
                std::env::temp_dir().join(format!("om-objects-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut host = session(&files.join(header("file")), &dir);
            host.go_to_layout(&LayoutTarget::Name(header("layout")), None)
                .unwrap();
            let context = {
                let w = host.window.as_ref().unwrap();
                Context {
                    occurrence: Some(w.occurrence),
                    record: w.record(),
                    ..Context::new()
                }
            };
            for line in text.lines().filter(|l| !l.starts_with('#')) {
                let Some((expression, expected)) = line.split_once('\t') else {
                    continue;
                };
                total += 1;
                let expected = unescape(expected);
                let result = evaluate_text(expression, &mut host, &context);
                let cx = Ctx::new(host.locale(), host.today());
                let got = result.value.to_text(&cx).into_string();
                if got != expected {
                    failures.push(format!(
                        "{name}: {expression}\n  want {expected:?}\n  got  {got:?}"
                    ));
                }
            }
            host.close();
            let _ = std::fs::remove_dir_all(&dir);
        }
        for f in &failures {
            println!("{f}");
        }
        println!("{} of {total} rows match", total - failures.len());
        assert!(failures.is_empty(), "{} rows differ", failures.len());
    }
}
