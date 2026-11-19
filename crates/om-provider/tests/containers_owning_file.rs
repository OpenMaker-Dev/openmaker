//! Synthetic hosted files deliberately share every native id. Library
//! reads and copies must follow their owning sessions, including after a
//! library drop and a file close/reopen.
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;

use fixture::{PHOTO_MEMBER, TABLE};
use om_data::calc::{Container, StreamType, Text};
use om_data::host::{CalcNeed, MemberKey};
use om_data::{DataProvider, ScriptWrite};
use om_hbam::{serial, slot::uint_segment, OwnedNode};
use om_provider::NativeProvider;

const REMOTE: u64 = 0xd0_0040;

fn payload(name: &str, body: &[u8]) -> Container {
    Container {
        text: Text::plain(name),
        streams: vec![
            (StreamType(*b"FILE"), body.to_vec()),
            (StreamType(*b"JPEG"), b"synthetic-preview".to_vec()),
            (StreamType(*b"SIZE"), vec![0, 7, 0, 9]),
        ],
        path_type: Some("movie"),
        ..Default::default()
    }
}

fn tree(value: &Container) -> OwnedNode {
    let mut delete = om_scripts::ScriptStep::new(10);
    delete.flags |= om_scripts::flag::NO_INTERACT;
    let mut root = fixture::tree(
        &[fixture::script(
            1,
            "Drop library",
            vec![fixture::on(86, true), delete],
        )],
        true,
    );
    fixture::add_photo(&mut root);
    let (mut nodes, md5) = om_data::containers::tree(value).unwrap();
    nodes.push(OwnedNode::leaf(vec![0x12], 1u32.to_be_bytes().to_vec()));
    nodes.sort_by(|a, b| a.segment.cmp(&b.segment));
    let t = uint_segment(TABLE);
    let m = serial::encode(PHOTO_MEMBER);
    root.put(&[&t, &[0x1f], &[5], &m], OwnedNode::list(m.clone(), nodes));
    root.put(
        &[&t, &[0x1f], &[1], &[8]],
        OwnedNode::list(
            vec![8],
            vec![OwnedNode::list(
                md5.to_vec(),
                vec![OwnedNode::leaf(m, vec![])],
            )],
        ),
    );
    root
}

fn header(provider: &mut NativeProvider, table: u64) -> Container {
    let member = serial::encode(PHOTO_MEMBER);
    let data = provider
        .calc_data(&[CalcNeed::Member {
            table,
            member: member.clone(),
        }])
        .unwrap();
    assert!(
        data.library.data.is_empty(),
        "metadata read downloaded stream bodies"
    );
    data.library.members[&(table, member.clone())]
        .as_ref()
        .unwrap()
        .value(table, &member)
}

fn body(provider: &mut NativeProvider, key: MemberKey) -> Container {
    let data = provider
        .calc_data(&[CalcNeed::MemberData {
            table: key.0,
            member: key.1.clone(),
        }])
        .unwrap();
    let stored = data.library.data[&key].as_ref().unwrap();
    assert!(stored.intact);
    stored.data.clone()
}

fn layout(provider: &mut NativeProvider) -> u32 {
    provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == "Tasks")
        .unwrap()
        .id
}

#[test]
fn identical_native_ids_keep_file_specific_headers_streams_and_copy_ownership() {
    let root_value = payload("root.dat", b"root bytes");
    let child_value = payload("child.dat", b"different related bytes");
    let mut root = tree(&root_value);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    let host = related::Host::new(
        "container-owning-file",
        vec![("Root.fmp12", root), ("Related.fmp12", tree(&child_value))],
    );
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    let mapped = schema.table_of(REMOTE).unwrap().id;
    assert_ne!(mapped, TABLE);
    let root_header = header(&mut provider, TABLE);
    let child_header = header(&mut provider, mapped);
    assert_eq!(root_header.text.as_str(), "root.dat");
    assert_eq!(child_header.text.as_str(), "child.dat");
    assert_ne!(
        root_header.library.as_ref().unwrap().md5,
        child_header.library.as_ref().unwrap().md5
    );
    assert_ne!(
        root_header.library.as_ref().unwrap().owner,
        child_header.library.as_ref().unwrap().owner
    );
    assert_eq!(
        root_header.library.as_ref().unwrap().md5,
        om_data::containers::tree(&root_value).unwrap().1
    );
    assert_eq!(
        child_header.library.as_ref().unwrap().md5,
        om_data::containers::tree(&child_value).unwrap().1
    );
    let body_reads = host.captures.with_opcode(om_protocol::Opcode::DownloadWithLock).into_iter()
        .filter(|(_, tx)| {
            let request = om_protocol::parse_download_request(tx).unwrap();
            matches!(request.key_path.as_slice(), [_, library, members, _] if library == &[0x1f] && members == &[5])
        }).collect::<Vec<_>>();
    assert!(
        body_reads.is_empty(),
        "metadata-only reads fetched bodies: {body_reads:?}"
    );
    for (table, expected) in [(TABLE, &root_value), (mapped, &child_value)] {
        let actual = body(&mut provider, (table, serial::encode(PHOTO_MEMBER)));
        assert_eq!(actual.text, expected.text);
        assert_eq!(actual.streams, expected.streams);
        assert_eq!(actual.path_type, expected.path_type);
    }
    let layout = layout(&mut provider);
    let copied = provider
        .container_reference(layout, "Photo", &child_header)
        .unwrap()
        .unwrap();
    provider
        .update_record(layout, 2, [("Photo".into(), Some(copied.clone()))].into())
        .unwrap();
    let member = om_data::containers::reference(&copied).unwrap();
    assert_ne!(member, serial::encode(PHOTO_MEMBER));
    let actual = body(&mut provider, (TABLE, member.clone()));
    assert_eq!(actual.streams, child_value.streams);
    assert_eq!(actual.text, child_value.text);
    assert_eq!(actual.path_type, child_value.path_type);

    // Ordinary imported container writes acquire in the actual owner's
    // library. Even the copied members may have colliding numeric ids.
    provider.begin_record_commit().unwrap();
    // The owner's source resolver must still refuse a stored value from a
    // connection it cannot identify; supplying immutable bytes is supported.
    assert_eq!(
        provider
            .related_container_reference(mapped, fixture::PHOTO, &root_header)
            .unwrap_err()
            .code,
        Some(1225)
    );
    let related_copy = provider
        .related_container_reference(mapped, fixture::PHOTO, &root_value)
        .unwrap()
        .unwrap();
    provider
        .write_record(&ScriptWrite {
            layout,
            table: Some(mapped),
            record: Some(1),
            cells: [(fixture::PHOTO, Some(related_copy.clone()))].into(),
            ..ScriptWrite::default()
        })
        .unwrap();
    provider.commit_record_session(false).unwrap();
    let related_member = om_data::containers::reference(&related_copy).unwrap();
    assert_eq!(related_member, member);
    let mut check = host.open("Related.fmp12");
    let check_layout = self::layout(&mut check);
    let saved = check.records(check_layout, 0, 1).unwrap();
    let photo = saved.fields.iter().position(|f| f.name == "Photo").unwrap();
    assert_eq!(saved.records[0].values[photo].as_ref(), Some(&related_copy));
    let actual = body(&mut check, (TABLE, related_member));
    assert_eq!(actual.streams, root_value.streams);
    assert_eq!(actual.text, root_value.text);
    assert_eq!(actual.path_type, root_value.path_type);
    check.close();
    assert_eq!(
        body(&mut provider, (TABLE, member)).streams,
        child_value.streams
    );
    assert!(host
        .captures
        .downloaded_tables()
        .iter()
        .all(|(_, table)| *table != mapped));

    provider.close();
    provider.open(&host.connection("Root.fmp12")).unwrap();
    assert!(provider
        .container_reference(layout, "Photo", &child_header)
        .is_err());
    let fresh = header(&mut provider, TABLE);
    assert_ne!(
        fresh.library.as_ref().unwrap().owner,
        root_header.library.as_ref().unwrap().owner
    );
    assert_eq!(fresh.text, root_value.text);
    provider.close();
    host.handle.shutdown(std::time::Duration::from_secs(1));
}

#[test]
fn dropped_library_reuses_ids_without_reusing_cached_bytes_or_stale_copy_values() {
    let host = related::Host::new(
        "container-reused-id",
        vec![("Root.fmp12", tree(&payload("before.dat", b"old bytes")))],
    );
    let mut provider = host.open("Root.fmp12");
    let layout = layout(&mut provider);
    let old = header(&mut provider, TABLE);
    assert_eq!(
        body(&mut provider, (TABLE, serial::encode(1)))
            .text
            .as_str(),
        "before.dat"
    );
    let result = provider
        .perform_script_on_server(&om_data::runtime::ServerScriptCall {
            file: 0,
            script: 1,
            parameter: None,
            wait: true,
        })
        .unwrap();
    assert_eq!(result.error, om_data::runtime::ErrorCode::NONE);
    let replacement = payload("after.dat", b"replacement bytes");
    let reference = provider
        .container_reference(layout, "Photo", &replacement)
        .unwrap()
        .unwrap();
    assert_eq!(
        om_data::containers::reference(&reference).unwrap(),
        serial::encode(1)
    );
    provider
        .create_record(layout, [("Photo".into(), Some(reference))].into())
        .unwrap();
    let fresh = header(&mut provider, TABLE);
    assert_eq!(fresh.text, replacement.text);
    assert_ne!(
        fresh.library.as_ref().unwrap().md5,
        old.library.as_ref().unwrap().md5
    );
    assert_eq!(
        body(&mut provider, (TABLE, serial::encode(1))).streams,
        replacement.streams
    );
    let mut cache = om_data::host::CalcData::default();
    let current = provider
        .calc_data(&[CalcNeed::MemberData {
            table: TABLE,
            member: serial::encode(1),
        }])
        .unwrap();
    cache.merge(current);
    let cache = std::cell::RefCell::new(cache);
    let source = om_data::host::CachedSource::new(&cache);
    assert!(
        om_data::host::library_data(&source, &old).is_err(),
        "a stale value must not read a replacement body from CalcData"
    );
    assert_eq!(
        provider
            .container_reference(layout, "Photo", &old)
            .unwrap_err()
            .code,
        Some(1225)
    );
    provider.close();
    host.handle.shutdown(std::time::Duration::from_secs(1));
}

#[test]
fn aliases_share_related_session_and_preserve_body_identity() {
    const ALIAS: u64 = 0xd0_0041;
    let mut root = tree(&payload("root.dat", b"root bytes"));
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    related::reference(&mut root, (ALIAS, "Alias Tasks"), 2, "Related.fmp12");
    let child = payload("alias.dat", b"related alias bytes");
    let host = related::Host::new(
        "container-aliases",
        vec![("Root.fmp12", root), ("Related.fmp12", tree(&child))],
    );
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    let alias = schema.table_of(ALIAS).unwrap().id;
    assert_eq!(remote, alias, "aliases should share a schema identity");
    for _ in 0..3 {
        for table in [remote, TABLE, alias] {
            let h = header(&mut provider, table);
            let b = body(&mut provider, (table, serial::encode(1)));
            assert_eq!(
                h.library.as_ref().unwrap().as_ref(),
                b.library.as_ref().unwrap().as_ref()
            );
            if table != TABLE {
                assert_eq!(b.streams, child.streams);
            }
        }
    }
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Related.fmp12"), Some(&1));
    provider.close();
}
