//! Signals that a project is in use, and so that its files should be left alone.

use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Where running processes are working. Best effort: processes owned by another
/// user can't be read, which is fine because their projects aren't in scope.
pub struct RunningProcesses {
    working_dirs: Vec<PathBuf>,
    names: Vec<String>,
}

impl RunningProcesses {
    pub fn snapshot() -> Self {
        let mut system = System::new();
        let refresh = ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always);
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
        let own_pid = sysinfo::get_current_pid().ok();
        let others = || {
            system
                .processes()
                .iter()
                .filter(|(pid, _)| Some(**pid) != own_pid)
                .map(|(_, process)| process)
        };
        let working_dirs = others()
            .filter_map(|process| process.cwd())
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(Path::to_owned)
            .collect();
        let names = others()
            .map(|process| process.name().to_string_lossy().to_lowercase())
            .collect();
        Self {
            working_dirs,
            names,
        }
    }

    pub fn none() -> Self {
        Self {
            working_dirs: Vec::new(),
            names: Vec::new(),
        }
    }

    /// Windows process names end in `.exe`; recipes name the app without it.
    pub fn runs(&self, app: &str) -> bool {
        let app = app.to_lowercase();
        self.names
            .iter()
            .any(|name| name.strip_suffix(".exe").unwrap_or(name) == app)
    }

    pub fn uses(&self, project: &Path) -> bool {
        self.working_dirs.iter().any(|dir| dir.starts_with(project))
    }
}

/// The newest file change in `root`, leaving out `.git` and the `skip` directories.
/// Artifacts are skipped because a dev server rewriting `.next` says nothing a
/// source edit wouldn't, and an install can touch thousands of files at once.
pub fn last_file_change(root: &Path, skip: &[PathBuf]) -> Option<Timestamp> {
    let mut newest = None;
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = entry.path();
            if meta.is_dir() {
                if entry.file_name() != ".git" && !skip.contains(&path) {
                    pending.push(path);
                }
                continue;
            }
            let modified = meta.modified().ok();
            let time = modified.and_then(|t| Timestamp::try_from(t).ok());
            newest = newest.max(time);
        }
    }
    newest
}
