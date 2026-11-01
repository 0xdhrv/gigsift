mod human;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use gigsift_core::age::parse_age;
use gigsift_core::size::parse_size;
use gigsift_core::{
    EntryKind, Envelope, ErrorBody, ErrorCode, ErrorResponse, Profile, SCHEMA_VERSION, ScanData,
    SizedEntry, TopData,
};
use gigsift_engine::{Analyzer, create_plan, explain, find_projects};
use gigsift_recipes::RecipeSet;
use gigsift_scanner::{Entry, ScanOptions, scan};
use gigsift_store::{PlanStore, StoreError};
use serde::Serialize;

const SCAN_DIRECTORIES_SHOWN: usize = 20;
const DEFAULT_ACTIVE_WITHIN: &str = "14d";

#[derive(Parser)]
#[command(name = "gigsift", version, about = "Understand every gigabyte.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a directory and summarize what it holds. Read-only.
    Scan {
        /// Directory to scan. Defaults to your home directory.
        path: Option<PathBuf>,
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List the largest files and folders. Read-only.
    Top {
        /// Directory to scan. Defaults to your home directory.
        path: Option<PathBuf>,
        /// Only show entries using at least this much disk, like 500mb or 1gib.
        #[arg(long, default_value = "100mb", value_parser = parse_size)]
        min_size: u64,
        /// Show at most this many entries.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Say what a path is, what it belongs to, and whether it is safe. Read-only.
    Explain {
        /// File or directory to explain.
        path: PathBuf,
        /// Treat a project as active if it changed this recently, like 14d or 2w.
        #[arg(long, default_value = DEFAULT_ACTIVE_WITHIN, value_parser = parse_age)]
        active_within: u64,
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List projects holding reclaimable artifacts. Read-only.
    Projects {
        /// Directory to search. Defaults to your home directory.
        path: Option<PathBuf>,
        /// Only show projects idle at least this long, like 30d.
        #[arg(long, value_parser = parse_age)]
        stale: Option<u64>,
        /// Treat a project as active if it changed this recently, like 14d or 2w.
        #[arg(long, default_value = DEFAULT_ACTIVE_WITHIN, value_parser = parse_age)]
        active_within: u64,
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Plan a cleanup and review it. Nothing here removes anything.
    Clean {
        #[command(subcommand)]
        command: CleanCommand,
    },
}

#[derive(Subcommand)]
enum CleanCommand {
    /// Build a plan to recover space, and save it for review.
    Plan {
        /// Directory to search. Defaults to your home directory.
        path: Option<PathBuf>,
        /// What kind of data to consider.
        #[arg(long, value_enum, default_value_t = ProfileArg::Developer)]
        profile: ProfileArg,
        /// How much to recover, like 10gb.
        #[arg(long, value_parser = parse_size)]
        target: u64,
        /// Treat a project as active if it changed this recently, like 14d or 2w.
        #[arg(long, default_value = DEFAULT_ACTIVE_WITHIN, value_parser = parse_age)]
        active_within: u64,
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show a saved plan.
    Show {
        /// The plan's id, such as plan_01J...
        plan_id: String,
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ProfileArg {
    Developer,
}

impl From<ProfileArg> for Profile {
    fn from(profile: ProfileArg) -> Self {
        match profile {
            ProfileArg::Developer => Self::Developer,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (name, json) = match &cli.command {
        Command::Scan { json, .. } => ("scan", *json),
        Command::Top { json, .. } => ("top", *json),
        Command::Explain { json, .. } => ("explain", *json),
        Command::Projects { json, .. } => ("projects", *json),
        Command::Clean {
            command: CleanCommand::Plan { json, .. },
        } => ("clean plan", *json),
        Command::Clean {
            command: CleanCommand::Show { json, .. },
        } => ("clean show", *json),
    };
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_failure(name, json, &error);
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Scan { path, json } => run_scan(path, json),
        Command::Top {
            path,
            min_size,
            limit,
            json,
        } => run_top(path, min_size, limit, json),
        Command::Explain {
            path,
            active_within,
            json,
        } => run_explain(&path, active_within, json),
        Command::Projects {
            path,
            stale,
            active_within,
            json,
        } => run_projects(path, stale, active_within, json),
        Command::Clean { command } => match command {
            CleanCommand::Plan {
                path,
                profile,
                target,
                active_within,
                json,
            } => run_clean_plan(path, profile.into(), target, active_within, json),
            CleanCommand::Show { plan_id, json } => run_clean_show(&plan_id, json),
        },
    }
}

fn run_scan(path: Option<PathBuf>, json: bool) -> Result<()> {
    let root = resolve_root(path)?;
    // Only directories are listed, so no individual file needs to be kept.
    let options = ScanOptions {
        keep_files_from_bytes: u64::MAX,
    };
    let result = scan(&root, &options)?;
    let largest_directories = result
        .largest(Some(EntryKind::Directory), 0)
        .into_iter()
        .take(SCAN_DIRECTORIES_SHOWN)
        .map(sized_entry)
        .collect();
    let data = ScanData {
        root: display_path(&result.root),
        totals: result.totals,
        largest_directories,
    };
    let envelope = Envelope::new("scan", result.meta, data);
    if json {
        print_json(&envelope)
    } else {
        human::print_scan(&envelope);
        Ok(())
    }
}

fn run_top(path: Option<PathBuf>, min_size: u64, limit: usize, json: bool) -> Result<()> {
    let root = resolve_root(path)?;
    let options = ScanOptions {
        keep_files_from_bytes: min_size,
    };
    let result = scan(&root, &options)?;
    let found = result.largest(None, min_size);
    let truncated = found.len().saturating_sub(limit) as u64;
    let entries = found.into_iter().take(limit).map(sized_entry).collect();
    let data = TopData {
        root: display_path(&result.root),
        min_allocated_bytes: min_size,
        entries,
        truncated,
    };
    let envelope = Envelope::new("top", result.meta, data);
    if json {
        print_json(&envelope)
    } else {
        human::print_top(&envelope);
        Ok(())
    }
}

fn run_explain(path: &Path, active_within: u64, json: bool) -> Result<()> {
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::for_user(&recipes, active_within);
    let envelope = explain(path, &analyzer)?;
    if json {
        print_json(&envelope)
    } else {
        human::print_explain(&envelope);
        Ok(())
    }
}

fn run_projects(
    path: Option<PathBuf>,
    stale: Option<u64>,
    active_within: u64,
    json: bool,
) -> Result<()> {
    let root = resolve_root(path)?;
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::for_user(&recipes, active_within);
    let envelope = find_projects(&root, stale, &analyzer)?;
    if json {
        print_json(&envelope)
    } else {
        human::print_projects(&envelope);
        Ok(())
    }
}

fn run_clean_plan(
    path: Option<PathBuf>,
    profile: Profile,
    target: u64,
    active_within: u64,
    json: bool,
) -> Result<()> {
    let root = resolve_root(path)?;
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::for_user(&recipes, active_within);
    let envelope = create_plan(&root, target, profile, &analyzer)?;
    PlanStore::for_user()?.save(&envelope)?;
    if json {
        print_json(&envelope)
    } else {
        human::print_plan(&envelope);
        Ok(())
    }
}

fn run_clean_show(plan_id: &str, json: bool) -> Result<()> {
    let envelope = PlanStore::for_user()?.load(plan_id)?;
    if json {
        print_json(&envelope)
    } else {
        human::print_plan(&envelope);
        Ok(())
    }
}

fn resolve_root(path: Option<PathBuf>) -> Result<PathBuf> {
    match path {
        Some(path) => Ok(path),
        None => std::env::home_dir().context("could not determine your home directory"),
    }
}

fn sized_entry(entry: &Entry) -> SizedEntry {
    SizedEntry {
        path: display_path(&entry.path),
        kind: entry.kind,
        logical_bytes: entry.logical_bytes,
        allocated_bytes: entry.allocated_bytes,
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Consumers of `--json` read stdout only, so failures must arrive there too.
fn report_failure(command: &str, json: bool, error: &anyhow::Error) {
    if !json {
        eprintln!("gigsift: {error:#}");
        return;
    }
    let response = ErrorResponse {
        schema_version: SCHEMA_VERSION,
        command: command.to_owned(),
        error: ErrorBody {
            code: error_code(error),
            message: format!("{error:#}"),
        },
    };
    if let Err(print_error) = print_json(&response) {
        eprintln!("gigsift: {print_error:#}");
    }
}

fn error_code(error: &anyhow::Error) -> ErrorCode {
    match error.downcast_ref::<StoreError>() {
        Some(StoreError::InvalidPlanId(_)) => ErrorCode::InvalidPlanId,
        Some(StoreError::PlanNotFound(_)) => ErrorCode::PlanNotFound,
        Some(_) => ErrorCode::StorageFailed,
        None => ErrorCode::RootNotReadable,
    }
}
