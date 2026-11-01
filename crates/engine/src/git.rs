//! Read-only questions for git. Only fixed subcommands run here, and always with
//! `GIT_OPTIONAL_LOCKS=0`: a plain `git status` refreshes the index on disk, which
//! would break the promise that planning changes nothing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gigsift_core::GitStatus;
use jiff::Timestamp;

/// A failure to get an answer. Callers must treat it as unsafe, never as clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitUnavailable;

/// The working tree a directory belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub top: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepoState {
    pub uncommitted_changes: bool,
    pub unpushed_commits: bool,
    pub last_commit: Option<Timestamp>,
}

impl RepoState {
    pub fn git_status(&self) -> GitStatus {
        match (self.uncommitted_changes, self.unpushed_commits) {
            (true, _) => GitStatus::Dirty,
            (false, true) => GitStatus::UnpushedCommits,
            (false, false) => GitStatus::Clean,
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, GitUnavailable> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|_| GitUnavailable)
}

fn stdout(dir: &Path, args: &[&str]) -> Result<String, GitUnavailable> {
    let output = git(dir, args)?;
    if !output.status.success() {
        return Err(GitUnavailable);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The repo containing `dir`, or `None` when it is not in one. Anything else git
/// reports, such as a repo owned by another user, is an error and not a `None`.
pub fn find_repo(dir: &Path) -> Result<Option<Repo>, GitUnavailable> {
    let output = git(dir, &["rev-parse", "--show-toplevel"])?;
    if output.status.success() {
        let top = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned();
        return Ok(Some(Repo {
            top: PathBuf::from(top),
        }));
    }
    let message = String::from_utf8_lossy(&output.stderr);
    if message.contains("not a git repository") {
        Ok(None)
    } else {
        Err(GitUnavailable)
    }
}

impl Repo {
    pub fn state(&self) -> Result<RepoState, GitUnavailable> {
        let status = stdout(
            &self.top,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )?;
        let remotes = stdout(&self.top, &["remote"])?;
        // With no remote there is nowhere to push, and every commit would count.
        let unpushed = if remotes.trim().is_empty() {
            false
        } else {
            let commits = stdout(
                &self.top,
                &[
                    "log",
                    "--branches",
                    "--not",
                    "--remotes",
                    "--max-count=1",
                    "--format=%H",
                ],
            )?;
            !commits.trim().is_empty()
        };
        Ok(RepoState {
            uncommitted_changes: !status.trim().is_empty(),
            unpushed_commits: unpushed,
            last_commit: self.last_commit(),
        })
    }

    /// `None` for a repo with no commits yet.
    fn last_commit(&self) -> Option<Timestamp> {
        let text = stdout(&self.top, &["log", "--max-count=1", "--format=%ct"]).ok()?;
        Timestamp::from_second(text.trim().parse().ok()?).ok()
    }

    pub fn has_tracked_files(&self, path: &Path) -> Result<bool, GitUnavailable> {
        let listed = stdout(&self.top, &["ls-files", "-z", "--", &self.relative(path)?])?;
        Ok(!listed.is_empty())
    }

    /// True for anything under `path` that git neither tracks nor ignores.
    pub fn has_untracked_files(&self, path: &Path) -> Result<bool, GitUnavailable> {
        let status = stdout(
            &self.top,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=normal",
                "--",
                &self.relative(path)?,
            ],
        )?;
        Ok(status.lines().any(|line| line.starts_with("??")))
    }

    pub fn is_ignored(&self, path: &Path) -> Result<bool, GitUnavailable> {
        let output = git(
            &self.top,
            &["check-ignore", "-q", "--", &self.relative(path)?],
        )?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(GitUnavailable),
        }
    }

    fn relative(&self, path: &Path) -> Result<String, GitUnavailable> {
        let relative = path.strip_prefix(&self.top).map_err(|_| GitUnavailable)?;
        match relative.to_str() {
            Some("") => Ok(".".to_owned()),
            Some(text) => Ok(text.to_owned()),
            None => Err(GitUnavailable),
        }
    }
}
