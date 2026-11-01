//! Local SQLite store for plans and the audit log. It lives in gigsift's own data
//! folder, never next to the files it describes.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use gigsift_core::{
    ActionStart, ActionStatus, ApplyResult, AuditError, AuditLog, Envelope, HistoryEntry, PlanData,
};
use jiff::Timestamp;
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS plans (
        plan_id    TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        body       TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS actions (
        action_id      TEXT PRIMARY KEY,
        run_id         TEXT NOT NULL,
        plan_id        TEXT NOT NULL,
        candidate_id   TEXT NOT NULL,
        path           TEXT NOT NULL,
        recipe_id      TEXT NOT NULL,
        started_at     TEXT NOT NULL,
        status         TEXT NOT NULL DEFAULT 'interrupted',
        changed        TEXT NOT NULL DEFAULT '[]',
        blocked_by     TEXT NOT NULL DEFAULT '[]',
        moved_bytes    INTEGER NOT NULL DEFAULT 0,
        trash_location TEXT,
        error          TEXT
    );
    CREATE INDEX IF NOT EXISTS actions_by_time ON actions (started_at DESC);
";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not find a data folder; set GIGSIFT_DATA_DIR")]
    NoDataDir,
    #[error("`{0}` is not a plan id")]
    InvalidPlanId(String),
    #[error("no plan named `{0}`")]
    PlanNotFound(String),
    #[error("plan `{0}` already exists, and plans never change")]
    PlanExists(String),
    #[error("{path}: {source}")]
    Folder {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error("stored data is unreadable: {0}")]
    Corrupt(String),
}

pub struct Store {
    connection: Connection,
}

impl Store {
    /// `GIGSIFT_DATA_DIR`, or the platform's per-user application data folder.
    pub fn for_user() -> Result<Self, StoreError> {
        Self::open(&data_dir()?)
    }

    pub fn open(data_dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(data_dir).map_err(|source| StoreError::Folder {
            path: data_dir.to_owned(),
            source,
        })?;
        let connection = Connection::open(data_dir.join("gigsift.db"))?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self { connection })
    }

    /// Plans are immutable, so saving over an existing id is an error.
    pub fn save_plan(&self, plan: &Envelope<PlanData>) -> Result<(), StoreError> {
        let id = &plan.data.plan_id;
        check_plan_id(id)?;
        let body = serde_json::to_string(plan).map_err(corrupt)?;
        let inserted = self.connection.execute(
            "INSERT INTO plans (plan_id, created_at, body) VALUES (?1, ?2, ?3)",
            params![id, plan.scanned_at.to_string(), body],
        );
        match inserted {
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::ConstraintViolation) => {
                Err(StoreError::PlanExists(id.clone()))
            }
            other => other.map(|_| ()).map_err(StoreError::from),
        }
    }

    pub fn load_plan(&self, plan_id: &str) -> Result<Envelope<PlanData>, StoreError> {
        check_plan_id(plan_id)?;
        let body: Option<String> = self
            .connection
            .query_row(
                "SELECT body FROM plans WHERE plan_id = ?1",
                params![plan_id],
                |row| row.get(0),
            )
            .optional()?;
        let body = body.ok_or_else(|| StoreError::PlanNotFound(plan_id.to_owned()))?;
        serde_json::from_str(&body).map_err(corrupt)
    }

    /// The newest actions, starting with the most recent.
    pub fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>, StoreError> {
        let mut query = self.connection.prepare(
            "SELECT action_id, run_id, plan_id, started_at, path, recipe_id, status, changed,
                    blocked_by, moved_bytes, trash_location, error
             FROM actions ORDER BY rowid DESC LIMIT ?1",
        )?;
        let rows = query.query_map(params![limit], read_row)?;
        rows.map(|row| row.map_err(StoreError::from)?.into_entry())
            .collect()
    }
}

struct Row {
    action_id: String,
    run_id: String,
    plan_id: String,
    started_at: String,
    path: String,
    recipe_id: String,
    status: String,
    changed: String,
    blocked_by: String,
    moved_bytes: i64,
    trash_location: Option<String>,
    error: Option<String>,
}

fn read_row(row: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        action_id: row.get(0)?,
        run_id: row.get(1)?,
        plan_id: row.get(2)?,
        started_at: row.get(3)?,
        path: row.get(4)?,
        recipe_id: row.get(5)?,
        status: row.get(6)?,
        changed: row.get(7)?,
        blocked_by: row.get(8)?,
        moved_bytes: row.get(9)?,
        trash_location: row.get(10)?,
        error: row.get(11)?,
    })
}

impl Row {
    fn into_entry(self) -> Result<HistoryEntry, StoreError> {
        let status: ActionStatus =
            serde_json::from_value(serde_json::Value::String(self.status)).map_err(corrupt)?;
        Ok(HistoryEntry {
            action_id: self.action_id,
            run_id: self.run_id,
            plan_id: self.plan_id,
            started_at: Timestamp::from_str(&self.started_at).map_err(corrupt)?,
            path: self.path,
            recipe_id: self.recipe_id,
            status,
            changed: serde_json::from_str(&self.changed).map_err(corrupt)?,
            blocked_by: serde_json::from_str(&self.blocked_by).map_err(corrupt)?,
            moved_bytes: u64::try_from(self.moved_bytes).map_err(corrupt)?,
            trash_location: self.trash_location,
            error: self.error,
        })
    }
}

impl AuditLog for Store {
    fn record_start(&mut self, action: &ActionStart) -> Result<(), AuditError> {
        self.connection.execute(
            "INSERT INTO actions
                (action_id, run_id, plan_id, candidate_id, path, recipe_id, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                action.action_id,
                action.run_id,
                action.plan_id,
                action.candidate_id,
                action.path,
                action.recipe_id,
                Timestamp::now().to_string(),
            ],
        )?;
        Ok(())
    }

    fn record_result(&mut self, result: &ApplyResult) -> Result<(), AuditError> {
        let status = serde_json::to_value(ActionStatus::from(result.outcome))?;
        let updated = self.connection.execute(
            "UPDATE actions
             SET status = ?2, changed = ?3, blocked_by = ?4, moved_bytes = ?5,
                 trash_location = ?6, error = ?7
             WHERE action_id = ?1",
            params![
                result.action_id,
                status.as_str(),
                serde_json::to_string(&result.changed)?,
                serde_json::to_string(&result.blocked_by)?,
                i64::try_from(result.moved_bytes)?,
                result.trash_location,
                result.error,
            ],
        )?;
        if updated == 1 {
            Ok(())
        } else {
            Err(format!("no audit record for {}", result.action_id).into())
        }
    }
}

fn data_dir() -> Result<PathBuf, StoreError> {
    if let Some(dir) = env::var_os("GIGSIFT_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        env::home_dir().map(|home| home.join("Library").join("Application Support"))
    };
    base.map(|base| base.join("gigsift"))
        .ok_or(StoreError::NoDataDir)
}

/// Ids come from users and agents, so reject anything that isn't one up front.
fn check_plan_id(plan_id: &str) -> Result<(), StoreError> {
    let valid = plan_id
        .strip_prefix("plan_")
        .is_some_and(|ulid| ulid.len() == 26 && ulid.chars().all(|c| c.is_ascii_alphanumeric()));
    if valid {
        Ok(())
    } else {
        Err(StoreError::InvalidPlanId(plan_id.to_owned()))
    }
}

fn corrupt(error: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(error.to_string())
}

#[cfg(test)]
mod tests {
    use gigsift_core::{
        ApplyOutcome, BlockReason, ChangeKind, CompletenessStatus, Profile, ScanCompleteness,
        ScanMeta,
    };
    use tempfile::{TempDir, tempdir};

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
            active_within_seconds: 0,
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

    fn store() -> (TempDir, Store) {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn start(id: &str) -> ActionStart {
        ActionStart {
            action_id: id.to_owned(),
            run_id: "run_1".to_owned(),
            plan_id: ID.to_owned(),
            candidate_id: "cand_1".to_owned(),
            path: "/code/app/node_modules".to_owned(),
            recipe_id: "node-modules".to_owned(),
        }
    }

    fn result(id: &str, outcome: ApplyOutcome) -> ApplyResult {
        ApplyResult {
            action_id: id.to_owned(),
            candidate_id: "cand_1".to_owned(),
            path: "/code/app/node_modules".to_owned(),
            outcome,
            changed: vec![ChangeKind::Modified],
            blocked_by: vec![BlockReason::RecentActivity],
            moved_bytes: 42,
            trash_location: Some("/trash/node_modules".to_owned()),
            error: None,
        }
    }

    #[test]
    fn plans_round_trip() {
        let (_dir, store) = store();
        store.save_plan(&plan()).unwrap();
        assert_eq!(store.load_plan(ID).unwrap(), plan());
    }

    #[test]
    fn plans_survive_reopening_the_database() {
        let dir = tempdir().unwrap();
        Store::open(dir.path()).unwrap().save_plan(&plan()).unwrap();
        assert!(Store::open(dir.path()).unwrap().load_plan(ID).is_ok());
    }

    #[test]
    fn plans_cannot_be_overwritten() {
        let (_dir, store) = store();
        store.save_plan(&plan()).unwrap();
        assert!(matches!(
            store.save_plan(&plan()),
            Err(StoreError::PlanExists(_))
        ));
    }

    #[test]
    fn malformed_ids_are_rejected() {
        let (_dir, store) = store();
        for id in ["../../etc/passwd", "plan_../../x", "plan_", "' OR 1=1 --"] {
            assert!(matches!(
                store.load_plan(id),
                Err(StoreError::InvalidPlanId(_))
            ));
        }
    }

    #[test]
    fn missing_plans_say_so() {
        let (_dir, store) = store();
        assert!(matches!(
            store.load_plan(ID),
            Err(StoreError::PlanNotFound(_))
        ));
    }

    #[test]
    fn a_finished_action_keeps_everything_that_happened() {
        let (_dir, mut store) = store();
        store.record_start(&start("act_1")).unwrap();
        store
            .record_result(&result("act_1", ApplyOutcome::Moved))
            .unwrap();

        let history = store.history(10).unwrap();

        assert_eq!(history.len(), 1);
        let entry = &history[0];
        assert_eq!(entry.status, ActionStatus::Moved);
        assert_eq!(entry.moved_bytes, 42);
        assert_eq!(entry.changed, [ChangeKind::Modified]);
        assert_eq!(entry.blocked_by, [BlockReason::RecentActivity]);
        assert_eq!(entry.trash_location.as_deref(), Some("/trash/node_modules"));
    }

    #[test]
    fn an_action_that_never_finished_shows_as_interrupted() {
        let (_dir, mut store) = store();
        store.record_start(&start("act_1")).unwrap();

        assert_eq!(
            store.history(10).unwrap()[0].status,
            ActionStatus::Interrupted
        );
    }

    #[test]
    fn a_result_without_a_start_is_an_error() {
        let (_dir, mut store) = store();
        assert!(
            store
                .record_result(&result("act_404", ApplyOutcome::Moved))
                .is_err()
        );
    }

    #[test]
    fn history_is_limited_and_newest_first() {
        let (_dir, mut store) = store();
        for id in ["act_1", "act_2", "act_3"] {
            store.record_start(&start(id)).unwrap();
        }

        let history = store.history(2).unwrap();

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].action_id, "act_3");
    }
}
