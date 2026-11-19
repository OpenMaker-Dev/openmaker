//! A container step's file dialog on the worker (host::containers): the
//! worker serves other windows while the dialog waits, and the answer
//! applies only to the file, window, record and field it was opened for.
//! Root and Peer reuse every layout, table, record and field id.
use super::*;
use crate::scripting::{ContainerFileDialog, Prompt};

/// Insert File [Rows::Photo] without a path: the dialog asks.
fn insert_without_path() -> ScriptStep {
    let mut step = ScriptStep::new(131);
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(5).unwrap(),
            StoredKey::from_id(20).unwrap(),
            0,
        )),
    );
    step
}

/// On the dialog: send what `meanwhile` makes of the peer's window (other
/// windows' requests, served while the dialog waits), then choose `path`.
/// The dialog as shown, and the UI's end of the worker's replies: it must
/// stay open, or the worker takes the UI for gone.
fn answer(
    c: &mut Client,
    path: std::path::PathBuf,
    meanwhile: fn(u32) -> Vec<(u32, Request)>,
) -> std::thread::JoinHandle<(ContainerFileDialog, mpsc::Receiver<Response>)> {
    let events = std::mem::replace(&mut c.events, mpsc::channel().1);
    let jobs = c._keep_jobs.clone();
    std::thread::spawn(move || {
        let mut peer = None;
        while let Ok(response) = events.recv_timeout(std::time::Duration::from_secs(30)) {
            let send = |window, request| {
                jobs.send(Job {
                    epoch: 1,
                    window,
                    ctx: Default::default(),
                    request,
                })
                .unwrap()
            };
            match response.reply {
                worker::Reply::Script(ScriptEvent::FileOpened(file)) => {
                    peer = Some(file.window.id);
                }
                worker::Reply::Script(ScriptEvent::Prompt {
                    id,
                    prompt: Prompt::ContainerFile(dialog),
                }) => {
                    for (window, request) in meanwhile(peer.expect("the peer is open")) {
                        send(window, request);
                    }
                    let chosen = ContainerFileDialog {
                        path: path.display().to_string(),
                        ..dialog.clone()
                    };
                    send(
                        1,
                        Request::Answer {
                            prompt: id,
                            answer: Answer::ContainerFile(Some(chosen)),
                        },
                    );
                    return (dialog, events);
                }
                _ => {}
            }
        }
        panic!("the container dialog was not shown");
    })
}

fn client(path: &std::path::Path) -> Client {
    std::fs::write(path, b"hello").unwrap();
    let c = Client::new(
        vec![
            open("Peer", true),
            insert_without_path(),
            exit("Get ( LastError ) & \"|\" & Get ( FileName )"),
        ],
        vec![],
    );
    c.server.lock().unwrap().containers = true;
    c
}

fn uploads(c: &Client) -> Vec<String> {
    let server = c.server.lock().unwrap();
    server
        .events
        .iter()
        .filter(|e| e.contains(":upload:"))
        .cloned()
        .collect()
}

#[test]
fn a_dialog_answer_inserts_into_its_own_file_after_the_worker_served_a_colliding_peer() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("note.txt");
    let mut c = client(&path);
    let shown = answer(&mut c, path, |peer| {
        // The peer's identically numbered layout and record, written
        // while Root's dialog is open.
        vec![(
            peer,
            Request::UpdateRecord {
                layout_id: 1,
                record_id: 1,
                values: [("Text".to_owned(), Some("peer edit".to_owned()))].into(),
            },
        )]
    });
    c.run();
    let (dialog, _events) = shown.join().unwrap();
    assert_eq!(
        (dialog.title.as_str(), dialog.field.as_str(), dialog.export),
        ("Insert File", "Rows::Photo", false)
    );
    let events = c.server.lock().unwrap().events.clone();
    assert_eq!(c.result(), "0|Root", "{events:?}");
    assert!(
        events.contains(&"Peer.fmp12:write".to_owned()),
        "{events:?}"
    );
    // Only Root's field took the file; the peer took no bytes.
    assert_eq!(uploads(&c), ["Root.fmp12:upload:Photo:note.txt:5"]);
}

#[test]
fn a_dialog_answer_changes_nothing_once_its_field_was_read_again() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("note.txt");
    let mut c = client(&path);
    let shown = answer(&mut c, path, |_| {
        // Manage Database in another window of Root: the schema the
        // field was resolved in is read again.
        vec![
            (
                1,
                Request::ChangeSchema(SchemaChange::ChangeTable {
                    table_id: 5,
                    name: "Renamed".into(),
                    comment: String::new(),
                }),
            ),
            (1, Request::CommitSchema),
        ]
    });
    c.run();
    let _events = shown.join().unwrap();
    let events = c.server.lock().unwrap().events.clone();
    assert_eq!(c.result(), "3|Root", "{events:?}");
    assert_eq!(uploads(&c), Vec::<String>::new());
}
