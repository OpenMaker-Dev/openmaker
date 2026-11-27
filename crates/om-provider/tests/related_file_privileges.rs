//! Owning-file login metadata over synthetic loopback TLS, including nested
//! formulas, unavailable metadata and reuse after a schema rebuild.
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related_privileges.rs"]
mod privileges;
#[path = "common/related.rs"]
mod related;
#[path = "common/privileges.rs"]
mod security;

use std::cell::RefCell;

use om_data::calc::eval::{display, evaluate_text, Context, Variables};
use om_data::calc::Date;
use om_data::host::{CachedSource, CalcData, CalcNeed, FileCalc, FileHost, SessionInfo};
use om_data::DataProvider;
use om_provider::NativeProvider;
use privileges::*;

fn answers(provider: &mut NativeProvider) -> Vec<String> {
    let schema = provider.calc_schema().unwrap().unwrap();
    assert_eq!(
        schema.authenticated_privileges,
        provider.account_privileges()
    );
    let metadata = provider.account_privileges();
    let session = SessionInfo {
        privilege_set_name: metadata
            .as_ref()
            .map(|p| p.account_set_name.clone())
            .unwrap_or_default(),
        privileges: metadata,
        ..SessionInfo::default()
    };
    let cache = RefCell::new(CalcData::default());
    let provider = RefCell::new(provider);
    let fetch = |needs: &[CalcNeed]| provider.borrow_mut().calc_data(needs).ok();
    let source = CachedSource::new(&cache).with_fetch(&fetch);
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&schema, &source, Date::from_ymd(2026, 11, 1).unwrap()),
        &session,
        &mut locals,
        &mut globals,
    );
    [
        REPORT,
        "Remote Tasks::Privileges",
        "Remote Tasks::NestedPrivileges",
        "Remote Tasks::DeepPrivileges",
        "Unknown Tasks::NestedPrivileges",
        REPORT,
    ]
    .map(|text| {
        let result = evaluate_text(text, &mut host, &Context::record(fixture::OCCURRENCE, 1));
        assert_eq!(result.code(), 0, "{text}");
        display(&result.value, &host)
    })
    .into()
}

#[test]
fn foreign_calculations_use_their_own_privilege_metadata_without_root_fallback() {
    let host = related::Host::new("owning-privileges", files(&[]));
    let mut provider = host.open(ROOT);
    for rebuild in [false, true] {
        if rebuild {
            provider.database_schema().unwrap();
        }
        assert_eq!(
            answers(&mut provider),
            [
                ROOT_ANSWER,
                RELATED_ANSWER,
                RELATED_ANSWER,
                NESTED_ANSWER,
                PLAIN_ANSWER,
                ROOT_ANSWER
            ]
        );
        let (_, logins) = host.captures.opens_and_logins();
        for file in [ROOT, RELATED, NESTED, UNKNOWN] {
            assert_eq!(logins.get(file), Some(&1), "{logins:?}");
        }
    }
    // Reuse the existing direct account-reference read on each authenticated
    // connection. No account parent or all-accounts scan is introduced.
    for file in [RELATED, NESTED] {
        // The root selection is denied for Read-Only / Data Entry accounts.
        // Each catalog must still load through its permitted individual path,
        // without another Login (checked above), including after a rebuild.
        let paths: Vec<_> = host
            .captures
            .with_opcode(om_protocol::Opcode::DownloadWithLock)
            .into_iter()
            .filter(|(name, _)| name == file)
            .map(|(_, tx)| om_protocol::parse_download_request(&tx).unwrap().key_path)
            .collect();
        for key in [3, 16, 32, 33] {
            assert_eq!(
                paths.iter().filter(|path| **path == [vec![key]]).count(),
                1,
                "{file}: {key} is read once through its own path"
            );
        }
    }
    for file in [ROOT, RELATED, NESTED, UNKNOWN] {
        let paths: Vec<_> = host
            .captures
            .with_opcode(om_protocol::Opcode::DownloadWithLock)
            .into_iter()
            .filter(|(name, _)| name == file)
            .map(|(_, tx)| om_protocol::parse_download_request(&tx).unwrap().key_path)
            .filter(|path| path.starts_with(&[vec![23], vec![1], vec![5]]))
            .collect();
        assert_eq!(paths.len(), 1, "{file}: {paths:?}");
        assert_eq!(paths[0].len(), 5);
        assert_eq!(paths[0][4], [0x0b]);
        if file == RELATED {
            assert_eq!(paths[0][3], [0x80, 0x81]);
        }
    }
    let schema = format!("{:?}", provider.calc_schema().unwrap());
    assert!(!schema.contains(std::str::from_utf8(security::AUTH_SENTINEL).unwrap()));
    provider.close();
    assert!(provider.account_privileges().is_none());
}

#[test]
fn refused_related_account_metadata_remains_unavailable() {
    // Only Related uses this opaque login key; root and the deeper file
    // retain their legitimate answers when its optional metadata is refused.
    let host = related::Host::configured("refused-owning-privileges", files(&[]), |server| {
        server.refusing_downloads(&[&[23], &[1], &[5], &[0x80, 0x81], &[0x0b]])
    });
    let mut provider = host.open(ROOT);
    assert_eq!(
        answers(&mut provider),
        [
            ROOT_ANSWER,
            UNKNOWN_ANSWER,
            UNKNOWN_ANSWER,
            NESTED_ANSWER,
            PLAIN_ANSWER,
            ROOT_ANSWER
        ]
    );
    assert!(provider.related_file_warnings().is_empty());
}

#[test]
fn partial_related_metadata_keeps_its_name_without_root_extensions_or_access() {
    let mut files = files(&[]);
    let child = &mut files
        .iter_mut()
        .find(|(name, _)| *name == RELATED)
        .unwrap()
        .1;
    // No extended privileges, and the account in a custom set without a
    // default record access (`02`): only Tasks has an entry of its own
    // (`05`), which lets it create, view, edit and delete; its layouts,
    // scripts and value lists as [Read-Only Access] has them.
    child.remove(&[&[23], &[3]]).unwrap();
    let partial_set: &[u8] = &[0x80, 0x85];
    child.put(
        &[&[23], &[2], &[5], partial_set, &[0x10]],
        om_scripts::text_node(&[], "Partial"),
    );
    child.put(
        &[&[23], &[2], &[5], partial_set, &[0x0a]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![0x03, 0x04]),
    );
    child.put(
        &[&[23], &[2], &[5], partial_set, &[0x0c]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![0, 0]),
    );
    for (catalog, quad) in [(0x04u8, 0xdeu8), (0x11, 0xd6), (0x21, 0xd6)] {
        child.put(
            &[&[23], &[2], &[5], partial_set, &[9], &[catalog]],
            om_hbam::OwnedNode::leaf(Vec::new(), vec![quad]),
        );
    }
    let tasks = om_hbam::slot::uint_segment(fixture::TABLE);
    child.put(
        &[&[23], &[2], &[5], partial_set, &[5], &tasks, &[2]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![0x55, 0x03]),
    );
    child.put(
        &[&[23], &[1], &[5], &[0x80, 0x81], &[0x0b]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![2, 0x80, 0x85]),
    );
    let host = related::Host::new("partial-owning-privileges", files);
    let mut provider = host.open(ROOT);
    let partial = "Partial|Partial||||";
    assert_eq!(
        answers(&mut provider),
        [
            ROOT_ANSWER,
            partial,
            partial,
            NESTED_ANSWER,
            PLAIN_ANSWER,
            ROOT_ANSWER
        ]
    );
}

/// Root's Office account may edit its own table; the same login owns a
/// Read-Only account in Related. Browse must offer no related field edits.
#[test]
fn related_read_only_rights_do_not_inherit_roots_edit_permission() {
    let mut files = files(&[]);
    let root = &mut files[0].1;
    let path: &[&[u8]] = &[&[4], &[5], &[1], &[7]];
    let serial = |key| om_hbam::serial::decode(&om_hbam::slot::uint_segment(key)).unwrap();
    let bounds = om_layout::Rect {
        top: 200.0,
        left: 10.0,
        bottom: 220.0,
        right: 200.0,
        ..Default::default()
    };
    let own = om_layout::encode::field_object(
        98,
        &bounds,
        serial(fixture::OCCURRENCE),
        serial(fixture::COUNT),
    );
    let object =
        om_layout::encode::field_object(99, &bounds, serial(REMOTE), serial(fixture::COUNT));
    let store = om_layout::encode::append_to_store(&[], &[98, 99], &[own, object]);
    root.put(path, om_hbam::OwnedNode::leaf([], store));
    let host = related::Host::new("related-read-only-edit", files);
    let mut root = host.open(ROOT);
    let schema = root.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let mut own = host.open(ROOT);
    own.update_record(
        fixture::LAYOUT as u32,
        1,
        [("Count".into(), Some("11".into()))].into(),
    )
    .unwrap();
    own.close();
    let mut child = host.open(RELATED);
    let before = child.records(fixture::LAYOUT as u32, 0, 1).unwrap();
    let count = before
        .fields
        .iter()
        .position(|f| f.name == "Count")
        .unwrap();
    let stored = before.records[0].values[count].clone();
    let write = om_data::ScriptWrite {
        layout: fixture::LAYOUT as u32,
        table: Some(table),
        record: Some(1),
        cells: [(fixture::COUNT, Some("99".into()))].into(),
        fields: Default::default(),
        typed: [fixture::COUNT].into(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
        single_field_step: false,
        assignments: None,
    };
    let denied = root.write_record(&write).unwrap_err();
    assert_eq!(
        child.records(fixture::LAYOUT as u32, 0, 1).unwrap().records[0].values[count],
        stored
    );
    let capabilities = root.record_capabilities(fixture::LAYOUT as u32).unwrap();
    assert!(capabilities.update, "Root stays editable");
    assert!(
        !capabilities.can_edit_related(REMOTE, fixture::COUNT),
        "Read-Only Related must not offer Count: {capabilities:?}"
    );
    assert_eq!(denied.code, Some(200), "{denied:?}");
}
