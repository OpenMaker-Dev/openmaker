//! Loopback TLS record-batch benchmark, including the client and paging.
//! cargo run --release -p om-server --example download_list_perf
use std::path::PathBuf;
use std::time::Instant;

use om_hbam::{serial, OwnedNode};
use om_protocol::{LoginEnvironment, MachineInfo};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};
use om_session::Session;

const FILE: &str = "download-perf.fmp12";
const CREATION: u32 = 0x01020304;
const TABLE: &[u8] = &[0x80, 1];

struct Fixture {
    path: PathBuf,
    records: OwnedNode,
}

impl Fixture {
    fn new(count: usize) -> Self {
        let records = OwnedNode::list(
            [5],
            (1..=count)
                .map(|id| {
                    OwnedNode::list(
                        serial::encode(id as u64),
                        vec![
                            OwnedNode::leaf([1], vec![b'x'; 512]),
                            OwnedNode::leaf([2], (id as u64).to_be_bytes().to_vec()),
                        ],
                    )
                })
                .collect(),
        );
        let mut root = OwnedNode::default();
        root.put(
            &[&[2], &[254]],
            OwnedNode::leaf([], CREATION.to_be_bytes().to_vec()),
        );
        root.put(
            &[&[23], &[1], &[5], &[2], &[16]],
            OwnedNode::leaf([], om_scsu::encode_scsu("Admin")),
        );
        root.put(&[TABLE, &[5]], records.clone());
        let mut superblock = vec![0; 4096];
        superblock[4..15].copy_from_slice(&om_hbam::container::SIGNATURE);
        superblock[15..20].copy_from_slice(om_hbam::container::MAGIC);
        superblock[20..24].copy_from_slice(&4096u32.to_be_bytes());
        let path = std::env::temp_dir().join(format!(
            "om-download-perf-{}-{count}-{:?}.fmp12",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, om_hbam::writer::rebuild(&root, &superblock).unwrap()).unwrap();
        Self { path, records }
    }

    fn connect(&self) -> Session {
        let config = ServerConfig {
            identity: ServerIdentity {
                name: "download-perf".into(),
                version_string: "benchmark".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: vec![HostedFile {
                name: FILE.into(),
                path: self.path.clone(),
            }],
            insecure_files: false,
            record_dir: None,
        };
        let server = Server::bind(
            "127.0.0.1:0",
            config,
            Identity::self_signed(vec!["localhost".into()]).unwrap(),
        )
        .unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || server.serve());
        let client = rcgen::generate_simple_self_signed(vec!["client".into()]).unwrap();
        let mut session = Session::connect(
            "127.0.0.1",
            port,
            client.cert.pem().as_bytes(),
            client.signing_key.serialize_pem().as_bytes(),
            "127.0.0.1",
            &MachineInfo::default(),
        )
        .unwrap();
        session.open_file("127.0.0.1", FILE).unwrap();
        assert_eq!(session.fetch_internal_creation().unwrap(), CREATION);
        let login = session
            .login(
                "Admin",
                "synthetic",
                CREATION,
                &LoginEnvironment::for_remote_file("127.0.0.1", FILE),
            )
            .unwrap();
        assert!(login.is_success(), "{login:?}");
        session
    }

    fn expected(&self, keys: &[Vec<u8>]) -> Vec<u8> {
        let mut parent = self.records.clone();
        parent.children.retain(|c| keys.contains(&c.segment));
        parent.to_wire().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn check(session: &mut Session, keys: &[Vec<u8>], expected: &[u8]) -> f64 {
    let borrowed: Vec<_> = keys.iter().map(Vec::as_slice).collect();
    let start = Instant::now();
    let reply = session.download_list(&[TABLE, &[5]], &borrowed, 0).unwrap();
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(reply.status, 0);
    assert_eq!(reply.slots, expected);
    ms
}

fn main() {
    println!("records,requested,median_ms,samples_ms");
    for count in [1_000usize, 10_000] {
        let fixture = Fixture::new(count);
        let mut session = fixture.connect();
        for requested in [0, 10, 100, count] {
            let mut keys: Vec<_> = (1..=requested)
                .rev()
                .map(|id| serial::encode(id as u64))
                .collect();
            if requested > 0 {
                keys.push(serial::encode((count + 100) as u64));
            }
            keys.sort_unstable();
            let expected = fixture.expected(&keys);
            check(&mut session, &keys, &expected);
            let samples: Vec<_> = (0..7)
                .map(|_| check(&mut session, &keys, &expected))
                .collect();
            let mut sorted = samples.clone();
            sorted.sort_by(f64::total_cmp);
            println!("{count},{requested},{:.6},{samples:?}", sorted[3]);
        }
        session.disconnect().unwrap();
    }
}

#[test]
fn record_batches_preserve_wire_order_missing_and_empty_selection() {
    let fixture = Fixture::new(256);
    let mut session = fixture.connect();
    for ids in [
        vec![200, 1, 200, 300, 127, 128],
        vec![],
        (1..=256).rev().collect(),
    ] {
        let mut keys: Vec<_> = ids.into_iter().map(serial::encode).collect();
        keys.sort_unstable();
        keys.dedup();
        check(&mut session, &keys, &fixture.expected(&keys));
    }
    let missing = session.download_list(&[TABLE, &[99]], &[&[1]], 0).unwrap();
    assert_eq!(missing.status, 0);
    assert_eq!(missing.slots, [0x80, 0x40]);
    session.disconnect().unwrap();
}
