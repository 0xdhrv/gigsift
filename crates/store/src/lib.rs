//! Local persistence. Plans are the only thing gigsift writes today, and they go
//! to its own data folder, never next to the files they describe.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use gigsift_core::{Envelope, PlanData};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not find a data folder; set GIGSIFT_DATA_DIR")]
    NoDataDir,
    #[error("`{0}` is not a plan id")]
    InvalidPlanId(String),
    #[error("no plan named `{0}`")]
    PlanNotFound(String),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}: {source}")]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },
}

pub struct PlanStore {
    dir: PathBuf,
}

impl PlanStore {
    /// `GIGSIFT_DATA_DIR`, or the platform's per-user application data folder.
    pub fn for_user() -> Result<Self, StoreError> {
        if let Some(dir) = env::var_os("GIGSIFT_DATA_DIR") {
            return Ok(Self::at(PathBuf::from(dir)));
        }
        let base = if cfg!(windows) {
            env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else {
            env::home_dir().map(|home| home.join("Library/Application Support"))
        };
        base.map(|base| Self::at(base.join("gigsift")))
            .ok_or(StoreError::NoDataDir)
    }

    pub fn at(data_dir: PathBuf) -> Self {
        Self {
            dir: data_dir.join("plans"),
        }
    }

    /// Plans are immutable, so saving over an existing id is an error.
    pub fn save(&self, plan: &Envelope<PlanData>) -> Result<PathBuf, StoreError> {
        let path = self.path_for(&plan.data.plan_id)?;
        fs::create_dir_all(&self.dir).map_err(|source| io_error(&self.dir, source))?;
        let text = serde_json::to_string_pretty(plan).map_err(|source| StoreError::Corrupt {
            path: path.clone(),
            source,
        })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        file.write_all(text.as_bytes())
            .map_err(|source| io_error(&path, source))?;
        Ok(path)
    }

    pub fn load(&self, plan_id: &str) -> Result<Envelope<PlanData>, StoreError> {
        let path = self.path_for(plan_id)?;
        let text = fs::read_to_string(&path).map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => StoreError::PlanNotFound(plan_id.to_owned()),
            _ => io_error(&path, source),
        })?;
        serde_json::from_str(&text).map_err(|source| StoreError::Corrupt { path, source })
    }

    /// Ids come from users and agents, so one must never be able to name a path.
    fn path_for(&self, plan_id: &str) -> Result<PathBuf, StoreError> {
        let valid = plan_id.strip_prefix("plan_").is_some_and(|ulid| {
            ulid.len() == 26 && ulid.chars().all(|c| c.is_ascii_alphanumeric())
        });
        if !valid {
            return Err(StoreError::InvalidPlanId(plan_id.to_owned()));
        }
        Ok(self.dir.join(format!("{plan_id}.json")))
    }
}

fn io_error(path: &Path, source: io::Error) -> StoreError {
    StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use gigsift_core::{CompletenessStatus, Profile, ScanCompleteness, ScanMeta};
    use jiff::Timestamp;
    use tempfile::tempdir;

    use super::*;

    const ID: &str = "plan_01JABCDEFGHJKMNPQRSTVWXYZ0";

    fn plan() -> Envelope<PlanData> {
        let meta = ScanMeta {
            scanned_at: Timestamp::UNIX_EPOCH,
            scan_completeness: ScanCompleteness {
                status: CompletenessStatus::Complete,
                directories_visited: 1,
                directories_unreadable: 0,
                entries_unreadable: 0,
                mount_points_skipped: 0,
            },
            inaccessible_paths: Vec::new(),
            warnings: Vec::new(),
        };
        let data = PlanData {
            plan_id: ID.to_owned(),
            root: "/code".to_owned(),
            profile: Profile::Developer,
            target_bytes: 1,
            items: Vec::new(),
            reclaimable_bytes_estimate: 0,
            target_met: false,
            shortfall_bytes: 1,
            not_needed: Vec::new(),
            advisories: Vec::new(),
            blocked: Vec::new(),
        };
        Envelope::new("clean plan", meta, data)
    }

    #[test]
    fn plans_round_trip() {
        let dir = tempdir().unwrap();
        let store = PlanStore::at(dir.path().to_owned());
        store.save(&plan()).unwrap();
        assert_eq!(store.load(ID).unwrap(), plan());
    }

    #[test]
    fn plans_cannot_be_overwritten() {
        let dir = tempdir().unwrap();
        let store = PlanStore::at(dir.path().to_owned());
        store.save(&plan()).unwrap();
        assert!(matches!(store.save(&plan()), Err(StoreError::Io { .. })));
    }

    #[test]
    fn ids_cannot_name_a_path() {
        let dir = tempdir().unwrap();
        let store = PlanStore::at(dir.path().to_owned());
        for id in ["../../etc/passwd", "plan_../../x", "plan_", ""] {
            assert!(matches!(store.load(id), Err(StoreError::InvalidPlanId(_))));
        }
    }

    #[test]
    fn missing_plans_say_so() {
        let dir = tempdir().unwrap();
        let store = PlanStore::at(dir.path().to_owned());
        assert!(matches!(store.load(ID), Err(StoreError::PlanNotFound(_))));
    }
}
