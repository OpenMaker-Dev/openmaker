//! The folders OpenMaker Server finds files to host in at startup, as
//! FileMaker Server opens the databases of its default and additional
//! database folders (Claris: "Database folders", "Additional database
//! folders"): files at a folder's root and one subfolder deep, never
//! deeper. Only `.fmp12` files (any case) are hosted; hidden entries
//! (named with a leading `.`) and the folders named in `exclude` (the
//! backup and log folders, should they lie inside one) are passed over.

use std::path::{Path, PathBuf};

/// The `.fmp12` files under `folders`, each folder's in name order: its
/// own files first, then those of its subfolders, each subfolder in name
/// order. A folder that cannot be read is skipped, logged; one in
/// `exclude` is not looked into.
pub fn discover(folders: &[PathBuf], exclude: &[PathBuf]) -> Vec<PathBuf> {
    let excluded: Vec<PathBuf> = exclude.iter().map(|p| canonical(p)).collect();
    let mut found = Vec::new();
    for folder in folders {
        if excluded.contains(&canonical(folder)) {
            continue;
        }
        let (files, subfolders) = match entries(folder) {
            Ok(entries) => entries,
            Err(e) => {
                log::warn!("database folder {}: {e}", folder.display());
                continue;
            }
        };
        found.extend(files);
        for subfolder in subfolders {
            if excluded.contains(&canonical(&subfolder)) {
                continue;
            }
            match entries(&subfolder) {
                // One level deep: the subfolder's own subfolders are not
                // looked into.
                Ok((files, _)) => found.extend(files),
                Err(e) => log::warn!("database folder {}: {e}", subfolder.display()),
            }
        }
    }
    found
}

/// The `.fmp12` files and the subfolders of `folder`, each in name order.
fn entries(folder: &Path) -> std::io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut items: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // Symbolic links are followed, as an administrator's link to a
        // file or folder elsewhere is meant to be hosted.
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        items.push((name, path, metadata.is_dir()));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    let mut files = Vec::new();
    let mut subfolders = Vec::new();
    for (_, path, is_dir) in items {
        if is_dir {
            subfolders.push(path);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("fmp12"))
        {
            files.push(path);
        }
    }
    Ok((files, subfolders))
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
