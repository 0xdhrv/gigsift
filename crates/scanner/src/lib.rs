//! Parallel directory walk with honest size accounting.
//!
//! The walk is read-only and does not follow symlinks. It counts plain files as
//! it goes and sets aside anything that may share storage. Those are charged
//! afterwards in path order, so a directory's total does not depend on which
//! thread reached a shared file first.

use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gigsift_core::probe::{CloneInfo, FileId, FileProbe};
use gigsift_core::{
    CompletenessStatus, EntryKind, FileIdentity, InaccessiblePath, InaccessibleReason, PathKind,
    RecycleError, Recycler, ScanCompleteness, ScanMeta, Totals, Warning, WarningCode,
};
use jiff::Timestamp;

#[cfg(unix)]
use gigsift_platform_macos as platform;
#[cfg(windows)]
use gigsift_platform_windows as platform;

#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Files smaller than this still count toward totals, but are not kept as
    /// individual entries. Directories are always kept.
    pub keep_files_from_bytes: u64,
}

#[derive(Debug)]
pub struct Scan {
    pub root: PathBuf,
    pub totals: Totals,
    pub meta: ScanMeta,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

impl Scan {
    /// Entries at or above `min_allocated_bytes`, largest first, ties by path.
    pub fn largest(&self, kind: Option<EntryKind>, min_allocated_bytes: u64) -> Vec<&Entry> {
        let mut found: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.allocated_bytes >= min_allocated_bytes)
            .filter(|e| kind.is_none_or(|k| e.kind == k))
            .collect();
        found.sort_by(|a, b| {
            b.allocated_bytes
                .cmp(&a.allocated_bytes)
                .then_with(|| a.path.cmp(&b.path))
        });
        found
    }
}

#[derive(Debug)]
pub struct RootError {
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot scan {}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for RootError {}

pub fn scan(root: &Path, options: &ScanOptions) -> Result<Scan, RootError> {
    let root_error = |message: String| RootError {
        path: root.to_owned(),
        message,
    };
    let root = std::path::absolute(root).map_err(|e| root_error(e.to_string()))?;
    let meta = fs::metadata(&root).map_err(|e| root_error(e.to_string()))?;
    if !meta.is_dir() {
        return Err(root_error("not a directory".to_owned()));
    }
    if platform::is_placeholder(&meta) {
        return Err(root_error(
            "cloud placeholder folder; listing it would download it".to_owned(),
        ));
    }
    fs::read_dir(&root).map_err(|e| root_error(e.to_string()))?;

    let walker = Walker {
        volume: stay_on_volume(&root, &meta),
        excluded: excluded_roots(&root),
        keep_from: options.keep_files_from_bytes,
        state: Mutex::new(WalkState::default()),
    };
    walker.push_dir(&root, None);
    rayon::scope(|s| walker.visit(s, 0, root.clone()));
    let state = walker.state.into_inner().expect("walker thread panicked");
    Ok(finish(root, state, options.keep_files_from_bytes))
}

/// What one path is and how much it holds. Symlinks are described, not followed.
#[derive(Debug)]
pub struct Inspection {
    pub kind: PathKind,
    pub symlink_target: Option<PathBuf>,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub exclusive_bytes: u64,
    pub hard_links: u32,
    pub shares_blocks: bool,
    pub cloud_placeholder: bool,
    pub meta: ScanMeta,
}

pub fn inspect(path: &Path) -> Result<Inspection, RootError> {
    let root_error = |message: String| RootError {
        path: path.to_owned(),
        message,
    };
    let meta = fs::symlink_metadata(path).map_err(|e| root_error(e.to_string()))?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        let target = fs::read_link(path).map_err(|e| root_error(e.to_string()))?;
        return Ok(Inspection {
            symlink_target: Some(target),
            ..single_entry(PathKind::Symlink)
        });
    }
    if kind.is_dir() {
        if platform::is_placeholder(&meta) {
            return Ok(Inspection {
                cloud_placeholder: true,
                ..single_entry(PathKind::Directory)
            });
        }
        let result = scan(path, &ScanOptions::default())?;
        return Ok(Inspection {
            logical_bytes: result.totals.logical_bytes,
            allocated_bytes: result.totals.allocated_bytes,
            exclusive_bytes: result.totals.exclusive_bytes,
            shares_blocks: result.totals.clones > 0,
            meta: result.meta,
            ..single_entry(PathKind::Directory)
        });
    }
    let probe = platform::probe_file(path, &meta);
    let shares_blocks = probe
        .clone
        .is_some_and(|clone| clone.private_bytes < probe.allocated_bytes);
    let path_kind = if kind.is_file() {
        PathKind::File
    } else {
        PathKind::Other
    };
    let exclusive_bytes = match probe.clone {
        _ if probe.links > 1 => 0,
        Some(clone) => clone.private_bytes,
        None => probe.allocated_bytes,
    };
    Ok(Inspection {
        logical_bytes: meta.len(),
        allocated_bytes: probe.allocated_bytes,
        exclusive_bytes,
        hard_links: probe.links,
        shares_blocks,
        cloud_placeholder: probe.placeholder,
        ..single_entry(path_kind)
    })
}

/// The OS's own Trash or Recycle Bin. See each platform crate for how it refuses.
pub struct NativeRecycler;

impl Recycler for NativeRecycler {
    fn recycle(&self, path: &Path) -> Result<Option<PathBuf>, RecycleError> {
        platform::move_to_trash(path)
    }
}

/// The fingerprint a plan records, so apply can tell if `path` was replaced.
pub fn file_identity(path: &Path, allocated_bytes: u64) -> Option<FileIdentity> {
    let meta = fs::symlink_metadata(path).ok()?;
    let id = platform::probe_file(path, &meta).id?;
    Some(FileIdentity {
        volume: id.volume,
        file: id.file,
        modified_at: meta
            .modified()
            .ok()
            .and_then(|t| Timestamp::try_from(t).ok()),
        allocated_bytes,
    })
}

fn single_entry(kind: PathKind) -> Inspection {
    Inspection {
        kind,
        symlink_target: None,
        logical_bytes: 0,
        allocated_bytes: 0,
        exclusive_bytes: 0,
        hard_links: 1,
        shares_blocks: false,
        cloud_placeholder: false,
        meta: ScanMeta {
            scanned_at: Timestamp::now(),
            scan_completeness: ScanCompleteness {
                status: CompletenessStatus::Complete,
                directories_visited: 0,
                directories_unreadable: 0,
                entries_unreadable: 0,
                mount_points_skipped: 0,
            },
            inaccessible_paths: Vec::new(),
            warnings: Vec::new(),
        },
    }
}

/// `/` on macOS spans the system volume and the firmlinked data volume, so there
/// the exclusion list does the job a volume check does everywhere else.
fn stay_on_volume(root: &Path, meta: &Metadata) -> Option<u64> {
    if root.parent().is_none() && !platform::EXCLUDED_ROOTS.is_empty() {
        return None;
    }
    platform::volume_id(root, meta)
}

fn excluded_roots(root: &Path) -> Vec<PathBuf> {
    if root.parent().is_some() {
        return Vec::new();
    }
    platform::EXCLUDED_ROOTS.iter().map(PathBuf::from).collect()
}

struct Walker {
    volume: Option<u64>,
    excluded: Vec<PathBuf>,
    keep_from: u64,
    state: Mutex<WalkState>,
}

#[derive(Default)]
struct WalkState {
    dirs: Vec<DirNode>,
    kept_files: Vec<Entry>,
    shared_files: Vec<SharedFile>,
    inaccessible: Vec<InaccessiblePath>,
    non_unicode_paths: u64,
    vanished: u64,
    dirs_unreadable: u64,
    entries_unreadable: u64,
    mounts_skipped: u64,
    files: u64,
    symlinks: u64,
    placeholders: u64,
    /// Allocated bytes of files that share nothing, so they are always exclusive.
    unshared_bytes: u64,
}

struct DirNode {
    path: PathBuf,
    /// Always a lower index than this node, which makes roll-up one reverse pass.
    parent: Option<u32>,
    logical_bytes: u64,
    allocated_bytes: u64,
}

/// A file that may share storage with another path.
struct SharedFile {
    dir: u32,
    path: PathBuf,
    logical_bytes: u64,
    probe: FileProbe,
}

/// Everything one directory contributes, merged in a single lock.
#[derive(Default)]
struct DirBatch {
    logical_bytes: u64,
    allocated_bytes: u64,
    files: u64,
    symlinks: u64,
    placeholders: u64,
    kept_files: Vec<Entry>,
    shared_files: Vec<SharedFile>,
    failures: Vec<(PathBuf, io::Error)>,
    mounts_skipped: u64,
    non_unicode_paths: u64,
}

impl Walker {
    fn state(&self) -> std::sync::MutexGuard<'_, WalkState> {
        self.state.lock().expect("walker thread panicked")
    }

    fn push_dir(&self, path: &Path, parent: Option<u32>) -> u32 {
        let mut state = self.state();
        let id = u32::try_from(state.dirs.len()).expect("more than 2^32 directories");
        state.dirs.push(DirNode {
            path: path.to_owned(),
            parent,
            logical_bytes: 0,
            allocated_bytes: 0,
        });
        id
    }

    fn visit<'s>(&'s self, scope: &rayon::Scope<'s>, id: u32, path: PathBuf) {
        let read = match fs::read_dir(&path) {
            Ok(read) => read,
            Err(error) => {
                let mut state = self.state();
                state.dirs_unreadable += 1;
                state.record_failure(path, &error);
                return;
            }
        };

        let mut batch = DirBatch::default();
        for entry in read {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    batch.failures.push((path.clone(), error));
                    continue;
                }
            };
            if entry.file_name().to_str().is_none() {
                batch.non_unicode_paths += 1;
            }
            let entry_path = entry.path();
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(error) => {
                    batch.failures.push((entry_path, error));
                    continue;
                }
            };
            let kind = meta.file_type();
            if kind.is_symlink() {
                batch.symlinks += 1;
            } else if kind.is_dir() {
                if self.is_boundary(&entry_path, &meta) {
                    batch.mounts_skipped += 1;
                } else if platform::is_placeholder(&meta) {
                    batch.placeholders += 1;
                } else {
                    let child = self.push_dir(&entry_path, Some(id));
                    scope.spawn(move |s| self.visit(s, child, entry_path));
                }
            } else {
                batch.add_file(id, entry_path, &meta, self.keep_from);
            }
        }
        self.state().merge(id, batch);
    }

    fn is_boundary(&self, path: &Path, meta: &Metadata) -> bool {
        if self.excluded.iter().any(|excluded| excluded == path) {
            return true;
        }
        self.volume
            .is_some_and(|root| platform::volume_id(path, meta).is_some_and(|v| v != root))
    }
}

impl DirBatch {
    fn add_file(&mut self, dir: u32, path: PathBuf, meta: &Metadata, keep_from: u64) {
        let probe = platform::probe_file(&path, meta);
        let logical_bytes = meta.len();
        if probe.placeholder {
            self.placeholders += 1;
        }
        if probe.links > 1 || probe.clone.is_some() {
            self.shared_files.push(SharedFile {
                dir,
                path,
                logical_bytes,
                probe,
            });
            return;
        }
        self.files += 1;
        self.logical_bytes += logical_bytes;
        self.allocated_bytes += probe.allocated_bytes;
        if probe.allocated_bytes >= keep_from {
            self.kept_files.push(Entry {
                path,
                kind: EntryKind::File,
                logical_bytes,
                allocated_bytes: probe.allocated_bytes,
            });
        }
    }
}

impl WalkState {
    fn merge(&mut self, id: u32, batch: DirBatch) {
        let node = &mut self.dirs[id as usize];
        node.logical_bytes += batch.logical_bytes;
        node.allocated_bytes += batch.allocated_bytes;
        self.unshared_bytes += batch.allocated_bytes;
        self.files += batch.files;
        self.symlinks += batch.symlinks;
        self.placeholders += batch.placeholders;
        self.mounts_skipped += batch.mounts_skipped;
        self.non_unicode_paths += batch.non_unicode_paths;
        self.kept_files.extend(batch.kept_files);
        self.shared_files.extend(batch.shared_files);
        for (path, error) in batch.failures {
            self.entries_unreadable += 1;
            self.record_failure(path, &error);
        }
    }

    fn record_failure(&mut self, path: PathBuf, error: &io::Error) {
        let reason = match error.kind() {
            io::ErrorKind::PermissionDenied => InaccessibleReason::PermissionDenied,
            io::ErrorKind::NotFound => InaccessibleReason::NotFound,
            io::ErrorKind::InvalidFilename => InaccessibleReason::PathTooLong,
            _ => InaccessibleReason::Other,
        };
        if reason == InaccessibleReason::NotFound {
            self.vanished += 1;
        }
        self.inaccessible.push(InaccessiblePath {
            path: path.to_string_lossy().into_owned(),
            reason,
            detail: error.to_string(),
        });
    }
}

/// Clone groups charge each file's private bytes, plus the group's shared bytes once.
#[derive(Default)]
struct CloneCharges {
    shared_charged: HashMap<u64, u64>,
    diverged: bool,
    count: u64,
}

impl CloneCharges {
    fn charge(&mut self, clone: CloneInfo, allocated: u64) -> u64 {
        let shared = allocated.saturating_sub(clone.private_bytes);
        if shared == 0 {
            return allocated;
        }
        self.count += 1;
        // A lone id with shared blocks is a clone modified after cloning. Its
        // partners have a different id, so those blocks can't be matched up.
        self.diverged |= clone.references <= 1;
        let charged = self.shared_charged.entry(clone.id).or_default();
        let extra = shared.saturating_sub(*charged);
        *charged += extra;
        clone.private_bytes + extra
    }
}

/// Works out what would be freed by removing the whole tree. Shared storage only
/// counts once every path that reaches it has been seen inside the tree.
#[derive(Default)]
struct ExclusiveBytes {
    bytes: u64,
    /// By file: links seen, links in total, and the bytes that file would free.
    hard_links: HashMap<FileId, (u32, u32, u64)>,
    /// By clone id: members seen, live references, and largest shared size.
    clones: HashMap<u64, (u32, u32, u64)>,
}

impl ExclusiveBytes {
    fn add(&mut self, probe: &FileProbe) {
        let own = probe
            .clone
            .map_or(probe.allocated_bytes, |c| c.private_bytes);
        match probe.id.filter(|_| probe.links > 1) {
            Some(id) => {
                let entry = self.hard_links.entry(id).or_insert((0, probe.links, own));
                entry.0 += 1;
                if entry.0 > 1 {
                    return;
                }
            }
            None => self.bytes += own,
        }
        if let Some(clone) = probe.clone {
            let shared = probe.allocated_bytes.saturating_sub(clone.private_bytes);
            let entry = self
                .clones
                .entry(clone.id)
                .or_insert((0, clone.references, 0));
            entry.0 += 1;
            entry.2 = entry.2.max(shared);
        }
    }

    fn total(&self) -> u64 {
        let links: u64 = self
            .hard_links
            .values()
            .filter(|(seen, total, _)| seen >= total)
            .map(|(_, _, bytes)| bytes)
            .sum();
        // One reference means a modified clone whose partners can't be identified.
        let clones: u64 = self
            .clones
            .values()
            .filter(|(seen, references, _)| *references > 1 && seen >= references)
            .map(|(_, _, shared)| shared)
            .sum();
        self.bytes + links + clones
    }
}

fn finish(root: PathBuf, mut state: WalkState, keep_from: u64) -> Scan {
    let mut totals = Totals {
        files: state.files,
        symlinks: state.symlinks,
        directories: state.dirs.len() as u64,
        cloud_placeholders: state.placeholders,
        ..Totals::default()
    };

    let mut shared = std::mem::take(&mut state.shared_files);
    shared.sort_by(|a, b| a.path.cmp(&b.path));
    let mut seen: HashSet<FileId> = HashSet::new();
    let mut clones = CloneCharges::default();
    let mut exclusive = ExclusiveBytes::default();
    for file in shared {
        exclusive.add(&file.probe);
        if file.probe.links > 1 && file.probe.id.is_some_and(|id| !seen.insert(id)) {
            totals.hard_link_duplicates += 1;
            continue;
        }
        totals.files += 1;
        let allocated = file.probe.allocated_bytes;
        let charged = match file.probe.clone {
            Some(clone) => clones.charge(clone, allocated),
            None => allocated,
        };
        let node = &mut state.dirs[file.dir as usize];
        node.logical_bytes += file.logical_bytes;
        node.allocated_bytes += charged;
        if allocated >= keep_from {
            state.kept_files.push(Entry {
                path: file.path,
                kind: EntryKind::File,
                logical_bytes: file.logical_bytes,
                allocated_bytes: allocated,
            });
        }
    }
    totals.clones = clones.count;
    totals.exclusive_bytes = state.unshared_bytes + exclusive.total();

    for index in (1..state.dirs.len()).rev() {
        let (logical, allocated, parent) = {
            let node = &state.dirs[index];
            (node.logical_bytes, node.allocated_bytes, node.parent)
        };
        if let Some(parent) = parent {
            let parent = &mut state.dirs[parent as usize];
            parent.logical_bytes += logical;
            parent.allocated_bytes += allocated;
        }
    }
    totals.logical_bytes = state.dirs[0].logical_bytes;
    totals.allocated_bytes = state.dirs[0].allocated_bytes;

    let mut entries = std::mem::take(&mut state.kept_files);
    let dirs = std::mem::take(&mut state.dirs);
    let visited = dirs.len() as u64 - state.dirs_unreadable;
    entries.extend(dirs.into_iter().map(|node| Entry {
        path: node.path,
        kind: EntryKind::Directory,
        logical_bytes: node.logical_bytes,
        allocated_bytes: node.allocated_bytes,
    }));

    state.inaccessible.sort_by(|a, b| a.path.cmp(&b.path));
    let warnings = warnings(&state, &clones, &totals);
    let status = if state.dirs_unreadable + state.entries_unreadable == 0 {
        CompletenessStatus::Complete
    } else {
        CompletenessStatus::Partial
    };
    Scan {
        root,
        totals,
        entries,
        meta: ScanMeta {
            scanned_at: Timestamp::now(),
            scan_completeness: ScanCompleteness {
                status,
                directories_visited: visited,
                directories_unreadable: state.dirs_unreadable,
                entries_unreadable: state.entries_unreadable,
                mount_points_skipped: state.mounts_skipped,
            },
            inaccessible_paths: state.inaccessible,
            warnings,
        },
    }
}

fn warnings(state: &WalkState, clones: &CloneCharges, totals: &Totals) -> Vec<Warning> {
    let mut warnings = Vec::new();
    let mut push = |code, message: String| warnings.push(Warning { code, message });

    let unreadable = state.inaccessible.len();
    if unreadable > 0 {
        push(
            WarningCode::IncompleteScan,
            format!("{unreadable} paths could not be read, so totals are a lower bound"),
        );
    }
    let denied = state
        .inaccessible
        .iter()
        .any(|p| p.reason == InaccessibleReason::PermissionDenied);
    if denied && cfg!(target_os = "macos") {
        push(
            WarningCode::FullDiskAccessMissing,
            "Some paths need Full Disk Access. Grant it to your terminal in System Settings > \
             Privacy & Security > Full Disk Access"
                .to_owned(),
        );
    }
    if state.vanished > 0 {
        push(
            WarningCode::ChangedDuringScan,
            format!("{} paths disappeared during the scan", state.vanished),
        );
    }
    if state.non_unicode_paths > 0 {
        push(
            WarningCode::NonUnicodePaths,
            format!(
                "{} paths are not valid Unicode and are shown lossily",
                state.non_unicode_paths
            ),
        );
    }
    if totals.cloud_placeholders > 0 {
        push(
            WarningCode::CloudPlaceholders,
            format!(
                "{} items are cloud placeholders. Their contents are not stored locally, and \
                 placeholder folders were not opened",
                totals.cloud_placeholders
            ),
        );
    }
    if clones.diverged {
        push(
            WarningCode::DivergedClones,
            "Some cloned files were modified after cloning. Blocks they still share can't be \
             attributed, so allocated bytes may overcount"
                .to_owned(),
        );
    }
    warnings
}
