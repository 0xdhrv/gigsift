//! Signals that a project is in use, and so that its files should be left alone.

use std::path::{Path, PathBuf};

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
        Self::from_parts(Vec::new(), Vec::new())
    }

    /// A fixed set of processes. `names` must be lowercase.
    pub fn from_parts(working_dirs: Vec<PathBuf>, names: Vec<String>) -> Self {
        Self {
            working_dirs,
            names,
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
