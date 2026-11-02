//! Compares a fresh scan with a recorded snapshot, and says what actually grew.

use std::collections::BTreeMap;
use std::path::Path;

use gigsift_core::{
    Baseline, ChangesData, DirectorySize, EntryKind, Envelope, PathChange, Snapshot, SnapshotData,
    Warning, WarningCode,
};
use gigsift_scanner::{RootError, Scan, ScanOptions, scan, volume_space};
use jiff::{SignedDuration, Timestamp};
use ulid::Ulid;

use crate::path_string;

/// Folders below this aren't recorded. A floor keeps a year of daily snapshots small.
pub const SNAPSHOT_MIN_DIRECTORY_BYTES: u64 = 100_000_000;

/// A child that explains this share of its parent's change gets the credit alone.
const ATTRIBUTION_SHARE: f64 = 0.8;

fn folders(scan: &Scan, min_bytes: u64) -> Vec<DirectorySize> {
    scan.entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Directory)
        .filter(|entry| entry.allocated_bytes >= min_bytes)
        .map(|entry| DirectorySize {
            path: path_string(&entry.path),
            allocated_bytes: entry.allocated_bytes,
        })
        .collect()
}

fn fresh_scan(root: &Path) -> Result<Scan, RootError> {
    let options = ScanOptions {
        keep_files_from_bytes: u64::MAX,
    };
    scan(root, &options)
}

/// Scans `root` into a snapshot, ready to save.
pub fn take_snapshot(
    root: &Path,
    min_directory_bytes: u64,
) -> Result<(Snapshot, Envelope<SnapshotData>), RootError> {
    let result = fresh_scan(root)?;
    let volume = volume_space(&result.root);
    let directories = folders(&result, min_directory_bytes);
    let snapshot = Snapshot {
        snapshot_id: format!("snap_{}", Ulid::new()),
        root: path_string(&result.root),
        taken_at: result.meta.scanned_at,
        complete: result.meta.scan_completeness.status
            == gigsift_core::CompletenessStatus::Complete,
        allocated_bytes: result.totals.allocated_bytes,
        logical_bytes: result.totals.logical_bytes,
        files: result.totals.files,
        volume,
        min_directory_bytes,
        directories,
    };
    let data = SnapshotData {
        snapshot_id: snapshot.snapshot_id.clone(),
        root: snapshot.root.clone(),
        allocated_bytes: snapshot.allocated_bytes,
        logical_bytes: snapshot.logical_bytes,
        files: snapshot.files,
        directories_recorded: snapshot.directories.len() as u64,
        volume,
    };
    Ok((snapshot, Envelope::new("snapshot", result.meta, data)))
}

/// Compares `root` now with `baseline`, the snapshot chosen for `since_seconds` ago.
pub fn compare_with_snapshot(
    root: &Path,
    baseline: Option<&Snapshot>,
    since_seconds: u64,
    limit: usize,
) -> Result<Envelope<ChangesData>, RootError> {
    let result = fresh_scan(root)?;
    let root_text = path_string(&result.root);
    let mut meta = result.meta.clone();

    let Some(baseline) = baseline else {
        meta.warnings.push(Warning {
            code: WarningCode::NoBaseline,
            message: "No snapshot of this folder yet. Run `gigsift snapshot` now, and compare \
                      once something has changed"
                .to_owned(),
        });
        let data = ChangesData {
            root: root_text,
            since_seconds,
            baseline: None,
            allocated_delta_bytes: None,
            changes: Vec::new(),
            truncated: 0,
        };
        return Ok(Envelope::new("changes", meta, data));
    };

    let wanted = Timestamp::now() - SignedDuration::from_secs(since_seconds as i64);
    let newer_than_requested = baseline.taken_at > wanted;
    if newer_than_requested {
        meta.warnings.push(Warning {
            code: WarningCode::BaselineTooRecent,
            message: format!(
                "The oldest snapshot is from {}, so this covers less time than asked",
                baseline.taken_at
            ),
        });
    }
    if !baseline.complete {
        meta.warnings.push(Warning {
            code: WarningCode::IncompleteBaseline,
            message: "The snapshot could not read everything, so some growth may be overstated"
                .to_owned(),
        });
    }

    let current = folders(&result, baseline.min_directory_bytes);
    let mut changes = attribute(&baseline.directories, &current);
    let truncated = changes.len().saturating_sub(limit) as u64;
    changes.truncate(limit);
    let data = ChangesData {
        root: root_text,
        since_seconds,
        baseline: Some(Baseline {
            snapshot_id: baseline.snapshot_id.clone(),
            taken_at: baseline.taken_at,
            complete: baseline.complete,
            newer_than_requested,
        }),
        allocated_delta_bytes: Some(delta(
            baseline.allocated_bytes,
            result.totals.allocated_bytes,
        )),
        changes,
        truncated,
    };
    Ok(Envelope::new("changes", meta, data))
}

fn delta(before: u64, after: u64) -> i64 {
    i64::try_from(after).unwrap_or(i64::MAX) - i64::try_from(before).unwrap_or(i64::MAX)
}

/// Lists what changed, leaving out a folder when one child explains its change:
/// `~/code` growing 10 GB says less than `~/code/app/target` growing 9.5 GB.
pub fn attribute(before: &[DirectorySize], after: &[DirectorySize]) -> Vec<PathChange> {
    let mut sides: BTreeMap<&str, (Option<u64>, Option<u64>)> = BTreeMap::new();
    for directory in before {
        sides.entry(&directory.path).or_default().0 = Some(directory.allocated_bytes);
    }
    for directory in after {
        sides.entry(&directory.path).or_default().1 = Some(directory.allocated_bytes);
    }

    let all: Vec<PathChange> = sides
        .into_iter()
        .filter(|(_, (old, new))| old != new)
        .map(|(path, (old, new))| PathChange {
            path: path.to_owned(),
            previous_bytes: old,
            current_bytes: new,
            delta_bytes: delta(old.unwrap_or(0), new.unwrap_or(0)),
            approximate: old.is_none() || new.is_none(),
        })
        .collect();

    let mut kept: Vec<PathChange> = all
        .iter()
        .filter(|change| !explained_by_child(change, &all))
        .cloned()
        .collect();
    kept.sort_by(|a, b| {
        b.delta_bytes
            .abs()
            .cmp(&a.delta_bytes.abs())
            .then_with(|| a.path.cmp(&b.path))
    });
    kept
}

fn explained_by_child(parent: &PathChange, all: &[PathChange]) -> bool {
    all.iter().any(|child| {
        is_inside(&child.path, &parent.path)
            && child.delta_bytes.signum() == parent.delta_bytes.signum()
            && child.delta_bytes.unsigned_abs() as f64
                >= parent.delta_bytes.unsigned_abs() as f64 * ATTRIBUTION_SHARE
    })
}

fn is_inside(path: &str, parent: &str) -> bool {
    Path::new(path) != Path::new(parent) && Path::new(path).starts_with(parent)
}

/// How a folder is keyed in stored snapshots.
pub fn snapshot_key(root: &Path) -> String {
    let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_owned());
    path_string(&absolute)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    fn dir(path: &str, gb: u64) -> DirectorySize {
        DirectorySize {
            path: path.to_owned(),
            allocated_bytes: gb * GB,
        }
    }

    #[test]
    fn identical_snapshots_show_no_changes() {
        let both = [dir("/a", 5), dir("/a/b", 2)];
        assert!(attribute(&both, &both).is_empty());
    }

    #[test]
    fn growth_is_credited_to_the_deepest_folder_that_explains_it() {
        let before = [
            dir("/code", 10),
            dir("/code/app", 10),
            dir("/code/app/target", 4),
        ];
        let after = [
            dir("/code", 20),
            dir("/code/app", 20),
            dir("/code/app/target", 14),
        ];

        let changes = attribute(&before, &after);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "/code/app/target");
        assert_eq!(changes[0].delta_bytes, (10 * GB) as i64);
        assert!(!changes[0].approximate);
    }

    #[test]
    fn a_parent_stays_when_no_single_child_explains_it() {
        let before = [dir("/code", 10), dir("/code/a", 2), dir("/code/b", 2)];
        let after = [dir("/code", 20), dir("/code/a", 7), dir("/code/b", 7)];

        let paths: Vec<_> = attribute(&before, &after)
            .into_iter()
            .map(|change| change.path)
            .collect();

        assert_eq!(paths, ["/code", "/code/a", "/code/b"]);
    }

    #[test]
    fn a_child_moving_the_other_way_does_not_explain_its_parent() {
        let before = [dir("/code", 10), dir("/code/a", 20)];
        let after = [dir("/code", 20), dir("/code/a", 5)];

        assert_eq!(attribute(&before, &after).len(), 2);
    }

    #[test]
    fn a_new_folder_is_marked_approximate() {
        let changes = attribute(&[], &[dir("/new", 3)]);

        assert_eq!(changes[0].previous_bytes, None);
        assert_eq!(changes[0].delta_bytes, (3 * GB) as i64);
        assert!(changes[0].approximate);
    }

    #[test]
    fn a_removed_folder_shows_as_shrinking() {
        let changes = attribute(&[dir("/old", 3)], &[]);

        assert_eq!(changes[0].current_bytes, None);
        assert_eq!(changes[0].delta_bytes, -((3 * GB) as i64));
        assert!(changes[0].approximate);
    }

    #[test]
    fn the_biggest_change_comes_first_whichever_way_it_went() {
        let before = [dir("/a", 10), dir("/b", 10)];
        let after = [dir("/a", 11), dir("/b", 2)];

        let changes = attribute(&before, &after);

        assert_eq!(changes[0].path, "/b");
        assert_eq!(changes[1].path, "/a");
    }

    #[test]
    fn similar_names_are_not_treated_as_children() {
        let before = [dir("/code", 10), dir("/code-old", 10)];
        let after = [dir("/code", 20), dir("/code-old", 20)];

        assert_eq!(attribute(&before, &after).len(), 2);
    }
}
