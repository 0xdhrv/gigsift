use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gigsift_core::{CompletenessStatus, EntryKind, WarningCode};
use gigsift_scanner::{Entry, Scan, ScanOptions, scan};
use tempfile::tempdir;

const KIB_64: usize = 64 * 1024;

fn write(root: &Path, relative: &str, bytes: usize) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![7u8; bytes]).unwrap();
    path
}

fn scan_all(root: &Path) -> Scan {
    scan(root, &ScanOptions::default()).unwrap()
}

fn entry<'a>(scan: &'a Scan, path: &Path) -> &'a Entry {
    scan.entries
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no entry for {}", path.display()))
}

fn has_warning(scan: &Scan, code: WarningCode) -> bool {
    scan.meta.warnings.iter().any(|w| w.code == code)
}

#[test]
fn sums_a_plain_tree_and_rolls_up_directories() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a/one.bin", KIB_64);
    write(dir.path(), "a/b/two.bin", KIB_64);
    write(dir.path(), "c/three.bin", KIB_64);

    let result = scan_all(dir.path());

    assert_eq!(result.totals.files, 3);
    assert_eq!(result.totals.directories, 4);
    assert_eq!(result.totals.logical_bytes, 3 * KIB_64 as u64);
    assert!(result.totals.allocated_bytes >= 3 * KIB_64 as u64);
    assert_eq!(
        entry(&result, &dir.path().join("a")).logical_bytes,
        2 * KIB_64 as u64
    );
    assert_eq!(
        entry(&result, dir.path()).allocated_bytes,
        result.totals.allocated_bytes
    );
    assert_eq!(
        result.meta.scan_completeness.status,
        CompletenessStatus::Complete
    );
    assert!(result.meta.inaccessible_paths.is_empty());
}

#[test]
fn hard_links_count_once_against_the_first_path() {
    let dir = tempdir().unwrap();
    let original = write(dir.path(), "b/original.bin", KIB_64);
    fs::create_dir(dir.path().join("a")).unwrap();
    fs::hard_link(&original, dir.path().join("a/link.bin")).unwrap();
    fs::create_dir(dir.path().join("c")).unwrap();
    fs::hard_link(&original, dir.path().join("c/link.bin")).unwrap();

    let result = scan_all(dir.path());

    assert_eq!(result.totals.files, 1);
    assert_eq!(result.totals.hard_link_duplicates, 2);
    assert_eq!(result.totals.logical_bytes, KIB_64 as u64);
    assert_eq!(
        entry(&result, &dir.path().join("a")).logical_bytes,
        KIB_64 as u64
    );
    assert_eq!(entry(&result, &dir.path().join("b")).logical_bytes, 0);
    assert_eq!(entry(&result, &dir.path().join("c")).logical_bytes, 0);
}

#[test]
fn keep_threshold_affects_entries_but_not_totals() {
    let dir = tempdir().unwrap();
    write(dir.path(), "small.bin", 10);
    let big = write(dir.path(), "big.bin", KIB_64);

    let options = ScanOptions {
        keep_files_from_bytes: KIB_64 as u64,
    };
    let result = scan(dir.path(), &options).unwrap();

    assert_eq!(result.totals.files, 2);
    assert_eq!(result.totals.logical_bytes, KIB_64 as u64 + 10);
    let files = result.largest(Some(EntryKind::File), 0);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, big);
}

#[test]
fn largest_orders_by_size_then_path() {
    let dir = tempdir().unwrap();
    let tied_b = write(dir.path(), "b.bin", KIB_64);
    let tied_a = write(dir.path(), "a.bin", KIB_64);
    let biggest = write(dir.path(), "z.bin", 4 * KIB_64);

    let result = scan_all(dir.path());
    let files: Vec<_> = result
        .largest(Some(EntryKind::File), 0)
        .iter()
        .map(|e| e.path.clone())
        .collect();

    assert_eq!(files, vec![biggest, tied_a, tied_b]);
}

#[test]
fn sparse_files_report_logical_above_allocated() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("sparse.bin");
    let file = fs::File::create(&path).unwrap();
    file.set_len(1 << 30).unwrap();
    drop(file);

    let result = scan_all(dir.path());

    assert_eq!(result.totals.logical_bytes, 1 << 30);
    if cfg!(unix) {
        assert!(result.totals.allocated_bytes < 1 << 20);
    }
}

#[test]
fn unicode_names_survive() {
    let dir = tempdir().unwrap();
    let path = write(dir.path(), "プロジェクト/naïve 🦀.bin", KIB_64);
    let result = scan_all(dir.path());
    assert_eq!(result.largest(Some(EntryKind::File), 1)[0].path, path);
    assert!(!has_warning(&result, WarningCode::NonUnicodePaths));
}

#[test]
fn deep_trees_do_not_overflow_the_stack() {
    let dir = tempdir().unwrap();
    let mut relative = String::new();
    for _ in 0..150 {
        relative.push_str("d/");
    }
    relative.push_str("leaf.bin");
    write(dir.path(), &relative, KIB_64);

    let result = scan_all(dir.path());

    assert_eq!(result.totals.files, 1);
    assert_eq!(result.totals.directories, 151);
    assert_eq!(result.totals.logical_bytes, KIB_64 as u64);
}

#[test]
fn a_missing_root_is_an_error_not_an_empty_result() {
    let dir = tempdir().unwrap();
    assert!(scan(&dir.path().join("nope"), &ScanOptions::default()).is_err());
}

#[test]
fn a_file_root_is_an_error() {
    let dir = tempdir().unwrap();
    let file = write(dir.path(), "file.bin", 10);
    assert!(scan(&file, &ScanOptions::default()).is_err());
}

/// Names, sizes, and modification times for everything under `root`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut seen = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        seen.insert(path.clone(), (meta.len(), meta.modified().unwrap()));
        if meta.is_dir() {
            pending.extend(fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        }
    }
    seen
}

#[test]
fn scanning_changes_nothing() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a/one.bin", KIB_64);
    write(dir.path(), "a/b/two.bin", 10);
    let before = snapshot(dir.path());

    scan_all(dir.path());

    assert_eq!(snapshot(dir.path()), before);
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use gigsift_core::InaccessibleReason;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn symlink_loops_end_and_targets_are_not_followed() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        write(outside.path(), "huge.bin", 8 * KIB_64);
        write(dir.path(), "a/one.bin", KIB_64);
        symlink(dir.path().join("a"), dir.path().join("a/loop")).unwrap();
        symlink(outside.path(), dir.path().join("elsewhere")).unwrap();

        let result = scan_all(dir.path());

        assert_eq!(result.totals.symlinks, 2);
        assert_eq!(result.totals.files, 1);
        assert_eq!(result.totals.logical_bytes, KIB_64 as u64);
    }

    /// Restores access on drop so the temp dir can be removed if the test fails.
    struct Locked(PathBuf);

    impl Locked {
        fn new(path: PathBuf) -> Self {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            Self(path)
        }
    }

    impl Drop for Locked {
        fn drop(&mut self) {
            fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn unreadable_directories_are_reported_and_mark_the_scan_partial() {
        let dir = tempdir().unwrap();
        write(dir.path(), "open/one.bin", KIB_64);
        write(dir.path(), "locked/secret.bin", KIB_64);
        let locked = Locked::new(dir.path().join("locked"));

        let result = scan_all(dir.path());

        assert_eq!(
            result.meta.scan_completeness.status,
            CompletenessStatus::Partial
        );
        assert_eq!(result.meta.scan_completeness.directories_unreadable, 1);
        assert_eq!(result.meta.inaccessible_paths.len(), 1);
        let inaccessible = &result.meta.inaccessible_paths[0];
        assert_eq!(inaccessible.path, locked.0.to_string_lossy());
        assert_eq!(inaccessible.reason, InaccessibleReason::PermissionDenied);
        assert_eq!(result.totals.logical_bytes, KIB_64 as u64);
        assert!(has_warning(&result, WarningCode::IncompleteScan));
    }

    /// `PATH_MAX` is 1024 on macOS and neither chain alone exceeds it, so the
    /// tree can only exist by renaming one chain into the other.
    fn tree_beyond_path_max() -> TempDir {
        let dir = tempdir().unwrap();
        let segment = "d".repeat(100);
        let chain = |name: &str| {
            let mut path = dir.path().join(name);
            for _ in 0..6 {
                path.push(&segment);
            }
            fs::create_dir_all(&path).unwrap();
            path
        };
        let first = chain("first");
        let second = chain("second");
        fs::write(second.join("buried.bin"), vec![7u8; KIB_64]).unwrap();
        fs::rename(dir.path().join("second"), first.join("second")).unwrap();
        dir
    }

    #[test]
    fn paths_beyond_the_limit_are_reported_as_too_long() {
        let dir = tree_beyond_path_max();

        let result = scan_all(dir.path());

        assert_eq!(
            result.meta.scan_completeness.status,
            CompletenessStatus::Partial
        );
        assert!(
            result
                .meta
                .inaccessible_paths
                .iter()
                .any(|p| p.reason == InaccessibleReason::PathTooLong),
            "{:?}",
            result.meta.inaccessible_paths
        );
    }
}

#[cfg(target_os = "macos")]
mod apfs {
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    fn clone_file(from: &Path, to: &Path) -> bool {
        Command::new("/bin/cp")
            .arg("-c")
            .args([from, to])
            .status()
            .unwrap()
            .success()
    }

    /// Temp dirs on a non-APFS volume can't clone; those runs prove nothing.
    fn cloned_tree() -> Option<(TempDir, u64)> {
        let dir = tempdir().unwrap();
        let original = write(dir.path(), "a.bin", 16 * KIB_64);
        for name in ["b.bin", "c.bin"] {
            if !clone_file(&original, &dir.path().join(name)) {
                return None;
            }
        }
        let single = scan_all(dir.path())
            .largest(Some(EntryKind::File), 0)
            .first()?
            .allocated_bytes;
        Some((dir, single))
    }

    #[test]
    fn identical_clones_count_their_shared_blocks_once() {
        let Some((dir, single)) = cloned_tree() else {
            return;
        };

        let result = scan_all(dir.path());

        assert_eq!(result.totals.files, 3);
        assert_eq!(result.totals.clones, 3);
        assert_eq!(result.totals.logical_bytes, 3 * 16 * KIB_64 as u64);
        assert_eq!(result.totals.allocated_bytes, single);
        assert!(!has_warning(&result, WarningCode::DivergedClones));
    }

    #[test]
    fn modified_clones_warn_that_allocated_may_overcount() {
        let Some((dir, single)) = cloned_tree() else {
            return;
        };
        // Rewrite the first block in place, keeping the rest shared.
        use std::io::{Seek, SeekFrom, Write};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("b.bin"))
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&vec![9u8; KIB_64]).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let result = scan_all(dir.path());

        assert!(result.totals.allocated_bytes > single);
        assert!(has_warning(&result, WarningCode::DivergedClones));
    }
}
