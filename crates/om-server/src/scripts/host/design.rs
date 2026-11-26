//! The Design functions and layout object functions of a server script
//! session (calc-functions.md 5.8), answered as FileMaker Server's script
//! engine answers them: from the file's whole schema with its layouts and
//! scripts (`om_data::host` Design functions, read once per version of the
//! derived state when a calculation first asks), the session's records for
//! value lists, the hosted file's next serial values, and the session's
//! window for `WindowNames` and `GetLayoutObjectAttribute` (layout
//! coordinates: the server's window has no toolbar).

use std::sync::Arc;

use om_calc::eval::{default_call, Context, HostFunction};
use om_calc::{CalcError, Ctx, Locale, Value};
use om_data::host::layout_objects::{self, ObjectState, ObjectWindow};
use om_data::host::{read_field, CalcSchema, FileCalc, LayoutEntry};
use om_hbam::slot::uint_segment;
use om_hbam::{KeyTree, OwnedNode, SlotReader};
use om_layout::LayoutObject;
use om_scripts::runtime::Mode;

use super::related::SessionSource;
use super::ServerHost;
use crate::engine::{Read, ReadError, View, NO_LIMIT};

/// Error 10, "Requested data is missing": `LayoutObjectUUID` outside a
/// layout object's calculation.
const DATA_MISSING: u16 = 10;

/// The catalogs the Design functions read beyond the record rules': the
/// file's locale and options `[2]`, layouts `[4]` with their themes `[6]`
/// and fonts `[25]` (a portal's borders place its fields), custom
/// functions `[16]`, scripts `[17]`, external data sources `[32]` and
/// value lists `[33]`, with `[3]` and every table's fields `[T, 3]`, as
/// one slot list, read through `read`.
fn design_catalogs(read: Read<'_>) -> Result<Option<Vec<u8>>, ReadError> {
    let mut children: Vec<OwnedNode> = Vec::new();
    for key in [2u8, 3, 4, 6, 16, 17, 25, 0x20, 0x21] {
        if let Some(node) = read.view.subtree(&[&[key]], NO_LIMIT)? {
            children.push(node.into_owned());
        }
    }
    for table in &read.derived.catalog().tables {
        let t = uint_segment(table.id);
        if let Some(fields) = read.view.subtree(&[&t, &[3]], NO_LIMIT)? {
            children.push(OwnedNode::list(t, vec![fields.into_owned()]));
        }
    }
    children.sort_by(|a, b| a.segment.cmp(&b.segment));
    Ok(OwnedNode::list(Vec::new(), children).to_wire())
}

/// What the Design schema is compiled from, read through one reading: the
/// catalogs' wire ([`design_catalogs`]), and the derived state's catalog,
/// locale and file language.
type DesignParts = (Option<Vec<u8>>, Arc<om_hbam::Catalog>, Locale, Option<u32>);

/// The file's calculation schema with its layouts (objects, names), script
/// list and data sources, for the Design functions, from the catalogs
/// `wire` holds ([`design_catalogs`]).
fn design_schema(
    wire: &[u8],
    catalog: &om_hbam::Catalog,
    locale: Locale,
    file_language: Option<u32>,
    file: &str,
) -> Option<Arc<CalcSchema>> {
    let tree = KeyTree::from_slots(SlotReader::new(wire)).ok()?;
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let mut schema = om_data::rules::schema::calc_schema(&tree, catalog, stem);
    schema.locale = locale;
    schema.locale_unreadable = file_language.is_none();
    om_data::host::device::withhold_server_lacks(schema.names_mut());
    Some(Arc::new(schema))
}

/// The layout a session opens on: the file's "Switch to layout" when it is
/// on (File Options, file-metadata.md 3), as FileMaker Server's sessions
/// open on Contact Details in the Contacts template.
pub(super) fn opening_layout(view: &dyn View) -> Result<Option<u64>, ReadError> {
    let Some(prefs) = view.subtree(&[&[2]], NO_LIMIT)? else {
        return Ok(None);
    };
    let Some(wire) = OwnedNode::list(Vec::new(), vec![prefs.into_owned()]).to_wire() else {
        return Ok(None);
    };
    let Ok(tree) = KeyTree::from_slots(SlotReader::new(&wire)) else {
        return Ok(None);
    };
    Ok(om_data::meta::file_options(&tree).and_then(|options| options.default_layout()))
}

impl ServerHost {
    /// The Design view of the file's schema as the engine holds it now
    /// ([`Self::with_design`]).
    pub(super) fn design(&self) -> Option<Arc<CalcSchema>> {
        self.with_design(false, false, |_, _| Ok(()))
            .and_then(|(design, ())| design)
    }

    /// The Design view of the file's schema as the engine holds it now, and
    /// `then` of it, read through the same reading, one of the reads kept to
    /// one version (docs/server-storage.md 5). It is compiled from the
    /// catalogs the Design
    /// functions read and the catalog, locale and language of that
    /// reading's derived state, all of one version, and kept by that
    /// version, which a later call of the same version reuses. It is
    /// compiled once the reading is let go, unless `then` needs it
    /// (`schema_first`, for GetNextSerialValue, which resolves its field in
    /// the version whose serial value it reads); `then` is given it when it
    /// is kept already or compiled first. `then` reads the accounts when
    /// `accounts` (`Reading::serve_with_accounts`). `None` when the read
    /// fails, which ends the run ([`ServerHost::read`]).
    fn with_design<T: PartialEq + std::fmt::Debug>(
        &self,
        accounts: bool,
        schema_first: bool,
        mut then: impl FnMut(Option<&CalcSchema>, Read<'_>) -> Result<T, ReadError>,
    ) -> Option<(Option<Arc<CalcSchema>>, T)> {
        if let Err(error) = self.reads.begin(&self.hosted) {
            self.note_failed_read(&error);
            return None;
        }
        let reading = self.shared.engine.read(&self.hosted);
        let version = reading.read().derived.version();
        let cached = self
            .schema
            .design
            .borrow()
            .as_ref()
            .filter(|(of, _)| *of == version)
            .map(|(_, design)| design.clone());
        let compile = |(wire, catalog, locale, language): DesignParts| {
            let design = wire.and_then(|wire| {
                design_schema(&wire, &catalog, locale, language, &self.hosted.name)
            });
            *self.schema.design.borrow_mut() = Some((version, design.clone()));
            design
        };
        // The schema kept or compiled first, else what it is compiled from.
        let design: Result<Option<Arc<CalcSchema>>, DesignParts> = match cached {
            Some(design) => Ok(design),
            None => {
                let parts = reading.serve(|read| {
                    Ok::<_, ReadError>((
                        design_catalogs(read)?,
                        read.derived.shared_catalog(),
                        read.derived.locale(),
                        read.derived.file_language(),
                    ))
                });
                match parts {
                    Ok(parts) if schema_first => Ok(compile(parts)),
                    Ok(parts) => Err(parts),
                    Err(error) => {
                        drop(reading);
                        self.note_failed_read(&error);
                        return None;
                    }
                }
            }
        };
        let schema = design.as_ref().ok().and_then(Option::as_deref);
        let then = match accounts {
            true => reading.serve_with_accounts(|read| then(schema, read)),
            false => reading.serve(|read| then(schema, read)),
        };
        drop(reading);
        let design = design.unwrap_or_else(compile);
        match then {
            Ok(then) => Some((design, then)),
            Err(error) => {
                self.note_failed_read(&error);
                None
            }
        }
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
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetLayoutObjectAttribute") => {
                Ok(self.layout_object_attribute(args))
            }
            _ => self.shared_call(function, args, context),
        }
    }

    /// [`Self::host_call`]'s answers that read the session without
    /// changing it: all but `GetLayoutObjectAttribute`, which evaluates the
    /// window's objects. An unstored calculation read outside a step's
    /// evaluation (a sort's, `calculated::Reader`) answers through this,
    /// so that the same field reads the same there.
    pub(super) fn shared_call(
        &self,
        function: HostFunction<'_>,
        args: &[Value],
        context: &Context,
    ) -> Result<Value, CalcError> {
        // GetSensor and the location functions as FileMaker Server's script
        // engine answers them (script oracle s48, `om_data::host::device`).
        if let Some(answer) = om_data::host::device::answer(function, args, &self.cx(), true) {
            return answer;
        }
        match function {
            HostFunction::Builtin(builtin) => {
                if let Some(answer) = self.design_call(builtin.name, args, context) {
                    return answer;
                }
            }
            HostFunction::Named(name) if name.eq_ignore_ascii_case("LayoutObjectUUID") => {
                return Err(CalcError::from_code(DATA_MISSING).expect("nonzero"));
            }
            // FileMaker Server found no object (calc-functions.md 5.8).
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetLayoutObjectOwnerInfo") => {
                return Ok(Value::text(layout_objects::owner_info_not_found()));
            }
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetRecordIDsFromFoundSet") => {
                return self.record_ids_from_found_set(args, context);
            }
            // As FileMaker Server's script engine answers them (script
            // oracle s45; calc-functions.md, "GetRecordIDsFromFoundSet on
            // FileMaker Server").
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetAddonInfo") => {
                return server_function_answer(ServerFunction::AddonInfo, args, &self.cx());
            }
            HostFunction::Named(name) if name.eq_ignore_ascii_case("GetAVPlayerAttribute") => {
                return server_function_answer(ServerFunction::PlayerAttribute, args, &self.cx());
            }
            HostFunction::Named(name) if name.eq_ignore_ascii_case("Furigana") => {
                return server_function_answer(ServerFunction::Furigana, args, &self.cx());
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
        // The Design schema and, for GetNextSerialValue, the field's next
        // value, through one reading.
        let serial = name.eq_ignore_ascii_case("GetNextSerialValue");
        // The account the session's facts describe, from the same reading;
        // its facts are worked out once the reading is let go.
        let (schema, (next, account)) = self.with_design(true, serial, |schema, read| {
            let next = match (serial, schema) {
                (true, Some(schema)) => self.next_serial_in(schema, read, args, context)?,
                _ => None,
            };
            // A security catalog that could not be read fails the read.
            let account = read.derived.account(&self.account)?;
            Ok((next, account))
        })?;
        let schema = schema?;
        let source = SessionSource { host: self };
        let account_info = account
            .map(|account| account.authenticated_info())
            .unwrap_or_default();
        let mut session = self.session_info_with(account_info);
        session.window_name = self.window_name();
        session.windows = self
            .window_names()
            .into_iter()
            .map(|name| (session.file_name.clone(), name))
            .collect();
        if let Some(layout) = self.window_layout(&schema) {
            session.layout_name = layout.name;
        }
        let file = FileCalc::new(&schema, &source, self.today).with_session(&session);
        if serial {
            let cx = self.cx();
            let file_name = args
                .first()
                .map(|v| v.to_text(&cx).into_string())
                .unwrap_or_default();
            if !file.names_this_file(&file_name) {
                return Some(Ok(Value::empty_text()));
            }
            return Some(Ok(next.map_or_else(Value::empty_text, Value::text)));
        }
        file.design(name, args, context)
    }

    /// `GetNextSerialValue`: the hosted file's current next value
    /// (`[T, 3, 5, F, 0c]`, advanced by every record the session creates),
    /// read through `read`, the reading `schema` is of; `None` (empty) for a
    /// field without a serial number. Whether its file argument names this
    /// file is the caller's ([`Self::design_call`]).
    fn next_serial_in(
        &self,
        schema: &CalcSchema,
        read: Read<'_>,
        args: &[Value],
        context: &Context,
    ) -> Result<Option<String>, ReadError> {
        let cx = self.cx();
        let text = |i: usize| {
            args.get(i)
                .map(|v| v.to_text(&cx).into_string())
                .unwrap_or_default()
        };
        let name = text(1);
        let (occurrence, field) = match name.split_once("::") {
            Some((occurrence, field)) => (schema.occurrence_named(occurrence).map(|o| o.id), field),
            None => (context.occurrence, name.as_str()),
        };
        let Some(table) = occurrence.and_then(|o| schema.table_of(o)) else {
            return Ok(None);
        };
        let Some(def) = table
            .fields
            .values()
            .find(|f| f.name.to_lowercase() == field.to_lowercase())
        else {
            return Ok(None);
        };
        if def.next_serial.is_none() {
            return Ok(None);
        }
        let (t, f) = (uint_segment(table.id), uint_segment(def.id));
        let path: [&[u8]; 5] = [&t, &[3], &[5], &f, &[0x0c]];
        let live = read
            .view
            .subtree(&path, NO_LIMIT)?
            .and_then(|node| crate::scripts::values::cell_text(&node));
        Ok(Some(
            live.or_else(|| def.next_serial.clone()).unwrap_or_default(),
        ))
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

/// A function FileMaker Server's script engine answers without the
/// add-on, player or dictionary it reads on a client (script oracles s45,
/// s48). `GetSensor` is `om_data::host::device`'s.
#[derive(Clone, Copy)]
enum ServerFunction {
    AddonInfo,
    PlayerAttribute,
    Furigana,
}

/// What FileMaker Server's script engine answered (s45, s48):
/// - `GetAddonInfo`: `{` CR TAB `"APIVers" : 2` CR `}` for any name run
///   (`"x"`, `"com.example.Other"`, `"?"`), error 3 for empty text;
/// - `GetAVPlayerAttribute`: empty, no error, for every attribute run
///   (`"playbackState"`, `"position"`, empty text, an unknown name);
/// - `Furigana`: its text unchanged, whatever its option (none, 2, 99),
///   empty for empty text: the engine's answer without its reading
///   dictionary (a `kakasi` program it calls). A server with the
///   dictionary answers readings [unmeasured].
///
/// Other arguments are answered the same [I].
fn server_function_answer(
    function: ServerFunction,
    args: &[Value],
    cx: &Ctx<'_>,
) -> Result<Value, CalcError> {
    let text = || args.first().map(|value| value.to_text(cx).into_string());
    match function {
        ServerFunction::AddonInfo if text().unwrap_or_default().is_empty() => {
            Err(CalcError::from_code(3).expect("nonzero"))
        }
        ServerFunction::AddonInfo => Ok(Value::text("{\r\t\"APIVers\" : 2\r}")),
        ServerFunction::PlayerAttribute => Ok(Value::empty_text()),
        ServerFunction::Furigana => Ok(Value::text(text().unwrap_or_default())),
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
        let engine = crate::engine::Engine::default();
        let hosting = Hosting::load(
            &[HostedFile {
                name: "work.fmp12".into(),
                path,
            }],
            false,
            &Default::default(),
            &engine,
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
            joins: Default::default(),
            recordings: None,
            mailboxes: Default::default(),
            engine: std::sync::Arc::new(engine),
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
            download_memory: crate::transfer::DownloadMemory::new(usize::MAX),
            #[cfg(debug_assertions)]
            test_panic: Default::default(),
            #[cfg(debug_assertions)]
            test_refusal: Default::default(),
            #[cfg(debug_assertions)]
            script_gate: Default::default(),
            #[cfg(debug_assertions)]
            failing_timestamps: Default::default(),
            #[cfg(debug_assertions)]
            read_only_before_write: Default::default(),
            #[cfg(debug_assertions)]
            join_lookup_delay: Default::default(),
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
