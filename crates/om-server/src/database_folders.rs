//! The folders OpenMaker Server finds files to host in at startup, as
//! FileMaker Server opens the databases of its default and additional
//! database folders (Claris: "Database folders", "Additional database
//! folders"): files at a folder's root and one subfolder deep, never
//! deeper. Only regular `.fmp12` files (any case) are hosted. Links are not
//! followed, so a file or folder linked from elsewhere is never found:
//! another location is chosen by naming it as a folder. Hidden entries
//! (named with a leading `.`) and anything inside the folders named in
//! `exclude` (the backup and log folders) are passed over. At most
//! [`MAX_ENTRIES`] entries are read in all and [`MAX_FILES`] files found,
//! so that a large tree cannot hold startup.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Entries read in all, every folder's together.
pub const MAX_ENTRIES: usize = 200_000;

/// Files found in all.
pub const MAX_FILES: usize = 10_000;

/// The `.fmp12` files under `folders`, each folder's in name order: its
/// own files first, then those of its subfolders, each subfolder in name
/// order. A folder that cannot be read is skipped, logged; one inside a
/// folder of `exclude` is not looked into.
pub fn discover(folders: &[PathBuf], exclude: &[PathBuf]) -> Vec<PathBuf> {
    let excluded: Vec<PathBuf> = exclude.iter().map(|p| canonical(p)).collect();
    let inside_excluded = |path: &Path| {
        let path = canonical(path);
        excluded.iter().any(|e| path.starts_with(e))
    };
    let mut found = Vec::new();
    let mut budget = MAX_ENTRIES;
    let add = |files: Vec<PathBuf>, found: &mut Vec<PathBuf>| {
        for file in files {
            if found.len() == MAX_FILES {
                log::warn!(
                    "database folders: more than {MAX_FILES} files; the rest are not hosted"
                );
                return false;
            }
            found.push(file);
        }
        true
    };
    for folder in folders {
        // The folder as named resolves through links once, here; below it
        // only what is not a link is used, so every path found is a
        // resolved one.
        let folder = &canonical(folder);
        if inside_excluded(folder) {
            continue;
        }
        let (files, subfolders) = match entries(folder, &mut budget) {
            Ok(entries) => entries,
            Err(e) => {
                log::warn!("database folder {}: {e}", folder.display());
                continue;
            }
        };
        if !add(files, &mut found) {
            break;
        }
        for subfolder in subfolders {
            if inside_excluded(&subfolder) {
                continue;
            }
            match entries(&subfolder, &mut budget) {
                // One level deep: the subfolder's own subfolders are not
                // looked into.
                Ok((files, _)) => {
                    if !add(files, &mut found) {
                        return found;
                    }
                }
                Err(e) => log::warn!("database folder {}: {e}", subfolder.display()),
            }
        }
    }
    found
}

/// The regular `.fmp12` files and the subfolders of `folder` (neither a
/// link), each in name order, reading no more entries than `budget` has
/// left and taking those read from it.
fn entries(folder: &Path, budget: &mut usize) -> std::io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut files: Vec<(OsString, PathBuf)> = Vec::new();
    let mut subfolders: Vec<(OsString, PathBuf)> = Vec::new();
    if *budget == 0 {
        // The limit was reached in an earlier folder, perhaps right at its
        // end: this one is passed over, and says so.
        log::warn!(
            "database folders: more than {MAX_ENTRIES} entries; {} is not read",
            folder.display()
        );
        return Ok((Vec::new(), Vec::new()));
    }
    for entry in std::fs::read_dir(folder)? {
        if *budget == 0 {
            log::warn!(
                "database folders: more than {MAX_ENTRIES} entries; {} and later folders are not read in full",
                folder.display()
            );
            break;
        }
        *budget -= 1;
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        // The entry itself, not what a link names.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            subfolders.push((name, path));
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("fmp12"))
        {
            files.push((name, path));
        } else if kind.is_symlink() {
            log::info!("{}: a link, not hosted", path.display());
        }
    }
    files.sort();
    subfolders.sort();
    Ok((
        files.into_iter().map(|(_, p)| p).collect(),
        subfolders.into_iter().map(|(_, p)| p).collect(),
    ))
}

/// `path` resolved when it exists, else as given.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files at the root and one subfolder deep are found, in name order;
    /// a deeper file, another extension, a hidden file and an excluded
    /// folder's files are not.
    #[test]
    fn finds_databases_at_the_root_and_one_folder_down() {
        let root = std::env::temp_dir().join(format!("om-db-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let databases = root.join("Databases");
        for dir in ["Sales", "Sales/Archive", "Backups", "Empty"] {
            std::fs::create_dir_all(databases.join(dir)).unwrap();
        }
        // Found paths are resolved ones (a temporary folder may be reached
        // through a link, as on macOS).
        let databases = databases.canonicalize().unwrap();
        for file in [
            "b.fmp12",
            "A.FMP12",
            "notes.txt",
            ".hidden.fmp12",
            "Sales/Orders.fmp12",
            "Sales/Archive/Old.fmp12",
            "Backups/Copy.fmp12",
        ] {
            std::fs::write(databases.join(file), b"").unwrap();
        }
        // A link to a file and to a folder elsewhere, and a FIFO named as
        // a database, are not hosted.
        #[cfg(unix)]
        {
            let elsewhere = root.join("Elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            std::fs::write(elsewhere.join("Outside.fmp12"), b"").unwrap();
            std::os::unix::fs::symlink(
                elsewhere.join("Outside.fmp12"),
                databases.join("Link.fmp12"),
            )
            .unwrap();
            std::os::unix::fs::symlink(&elsewhere, databases.join("Linked")).unwrap();
            let fifo = databases.join("Pipe.fmp12");
            let status = std::process::Command::new("mkfifo").arg(&fifo).status();
            assert!(status.is_ok_and(|s| s.success()), "mkfifo");
        }
        let found = discover(
            &[databases.clone(), root.join("Missing")],
            &[databases.join("Backups")],
        );
        let names: Vec<String> = found
            .iter()
            .map(|p| {
                p.strip_prefix(&databases)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["A.FMP12", "b.fmp12", "Sales/Orders.fmp12"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
