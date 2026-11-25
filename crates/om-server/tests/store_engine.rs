//! OpenMaker Server hosting files from the store engine (`--engine store`,
//! docs/server-storage.md 5, 11): each file imported into the store folder
//! at startup and served read-only from its tree.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use fixture::{TempFile, FILE, TABLE};
use server::{records_of, start_hosting, start_on_store};

/// A store folder for one test, removed when dropped.
struct StoreDir(std::path::PathBuf);

impl StoreDir {
    fn new(tag: &str) -> StoreDir {
        let path = std::env::temp_dir().join(format!(
            "om-store-engine-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        StoreDir(path)
    }
}

impl Drop for StoreDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_file_hosted_from_the_store_reads_as_from_the_file_engine() {
    let file = TempFile::new("store-engine-reads", &[], true);
    let before = std::fs::read(&file.0).unwrap();
    let store = StoreDir::new("reads");
    let (port, _) = start_on_store(&[(FILE, &file)], &store.0, |s| s);
    let (mut client, _) = server::logged_in(port, "store");
    let stored = records_of(&mut client, TABLE);
    let (port, _) = start_hosting(&[(FILE, &file)], |s| s);
    let (mut client, _) = server::logged_in(port, "file");
    let loaded = records_of(&mut client, TABLE);
    assert!(!stored.is_empty());
    assert_eq!(stored, loaded);
    assert_eq!(std::fs::read(&file.0).unwrap(), before);
}
