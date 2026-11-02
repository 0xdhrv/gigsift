use std::path::Path;

use gigsift_core::{EntryKind, Envelope, ScanData, SizedEntry, TopData};
use gigsift_scanner::{Entry, RootError, ScanOptions, scan};

use crate::path_string;

const SUMMARY_DIRECTORIES: usize = 20;

/// Totals for `root`, with its largest folders.
pub fn storage_summary(root: &Path) -> Result<Envelope<ScanData>, RootError> {
    // Only directories are listed, so no individual file needs to be kept.
    let options = ScanOptions {
        keep_files_from_bytes: u64::MAX,
    };
    let result = scan(root, &options)?;
    let largest_directories = result
        .largest(Some(EntryKind::Directory), 0)
        .into_iter()
        .take(SUMMARY_DIRECTORIES)
        .map(sized_entry)
        .collect();
    let data = ScanData {
        root: path_string(&result.root),
        totals: result.totals,
        largest_directories,
    };
    Ok(Envelope::new("scan", result.meta, data))
}

/// Files and folders under `root` using at least `min_size` on disk.
pub fn largest_entries(
    root: &Path,
    min_size: u64,
    limit: usize,
) -> Result<Envelope<TopData>, RootError> {
    let options = ScanOptions {
        keep_files_from_bytes: min_size,
    };
    let result = scan(root, &options)?;
    let found = result.largest(None, min_size);
    let truncated = found.len().saturating_sub(limit) as u64;
    let entries = found.into_iter().take(limit).map(sized_entry).collect();
    let data = TopData {
        root: path_string(&result.root),
        min_allocated_bytes: min_size,
        entries,
        truncated,
    };
    Ok(Envelope::new("top", result.meta, data))
}

fn sized_entry(entry: &Entry) -> SizedEntry {
    SizedEntry {
        path: path_string(&entry.path),
        kind: entry.kind,
        logical_bytes: entry.logical_bytes,
        allocated_bytes: entry.allocated_bytes,
    }
}
