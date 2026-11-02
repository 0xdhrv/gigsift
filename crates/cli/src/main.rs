mod human;

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use gigsift_core::age::parse_age;
use gigsift_core::size::parse_size;
use gigsift_core::{
    Envelope, ErrorBody, ErrorCode, ErrorResponse, HistoryData, HistoryResponse, PathSharing,
    PlanData, Profile, SCHEMA_VERSION, redact,
};
use gigsift_engine::{
    Analyzer, ApplyError, apply_plan, create_plan, explain, find_candidates, find_projects,
    largest_entries, storage_summary,
};
use gigsift_recipes::RecipeSet;
use gigsift_scanner::NativeRecycler;
use gigsift_store::{Store, StoreError};
use serde::Serialize;

const DEFAULT_ACTIVE_WITHIN: &str = "14d";

/// How much of each path `--json` output carries. Set once, from the command line.
static JSON_SHARING: OnceLock<PathSharing> = OnceLock::new();

#[derive(Parser)]
#[command(name = "gigsift", version, about = "Understand every gigabyte.")]
struct Cli {
    /// How much of each path --json output shows: full, home, or hidden. Use home
    /// or hidden when the output is going to an AI tool.
    #[arg(long, global = true, default_value = "full", value_parser = parse_sharing)]
    share_paths: PathSharing,
    #[command(subcommand)]
    command: Command,
}

fn parse_sharing(text: &str) -> Result<PathSharing, String> {
    text.parse()
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
    /// Plan a cleanup, review it, and move it to the Trash.
    Clean {
        #[command(subcommand)]
        command: CleanCommand,
    },
    /// Run the local MCP server.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Show what gigsift has done, newest first.
    History {
        /// Show at most this many actions.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CleanCommand {
    /// List everything recoverable right now, without saving a plan.
    Candidates {
        /// Directory to search. Defaults to your home directory.
        path: Option<PathBuf>,
        /// Treat a project as active if it changed this recently, like 14d or 2w.
        #[arg(long, default_value = DEFAULT_ACTIVE_WITHIN, value_parser = parse_age)]
        active_within: u64,
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
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
    /// Recheck a plan, then move what still qualifies to the Trash or Recycle Bin.
    Apply {
        /// The plan's id, such as plan_01J...
        plan_id: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Print the versioned JSON contract instead of text. Needs --yes.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve MCP over stdio. Agents can look and plan, never apply.
    Serve {
        /// How much of each path reaches the client: full, home, or hidden.
        #[arg(long, default_value = "home", value_parser = parse_sharing)]
        sharing: PathSharing,
    },
}

/// Failures that come from the CLI itself rather than the engine.
#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("confirmation is required here. Rerun with --yes after reviewing the plan")]
    ConfirmationRequired,
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
    JSON_SHARING.get_or_init(|| cli.share_paths);
    let (name, json) = match &cli.command {
        Command::Scan { json, .. } => ("scan", *json),
        Command::Top { json, .. } => ("top", *json),
        Command::Explain { json, .. } => ("explain", *json),
        Command::Projects { json, .. } => ("projects", *json),
        Command::Clean {
            command: CleanCommand::Candidates { json, .. },
        } => ("clean candidates", *json),
        Command::Clean {
            command: CleanCommand::Plan { json, .. },
        } => ("clean plan", *json),
        Command::Clean {
            command: CleanCommand::Show { json, .. },
        } => ("clean show", *json),
        Command::Clean {
            command: CleanCommand::Apply { json, .. },
        } => ("clean apply", *json),
        Command::History { json, .. } => ("history", *json),
        Command::Mcp { .. } => ("mcp serve", false),
    };
    match run(cli.command) {
        Ok(code) => code,
        Err(error) => {
            report_failure(name, json, &error);
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<ExitCode> {
    let finished = match command {
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
            CleanCommand::Candidates {
                path,
                active_within,
                json,
            } => run_clean_candidates(path, active_within, json),
            CleanCommand::Plan {
                path,
                profile,
                target,
                active_within,
                json,
            } => run_clean_plan(path, profile.into(), target, active_within, json),
            CleanCommand::Show { plan_id, json } => run_clean_show(&plan_id, json),
            CleanCommand::Apply { plan_id, yes, json } => {
                return run_clean_apply(&plan_id, yes, json);
            }
        },
        Command::History { limit, json } => run_history(limit, json),
        Command::Mcp {
            command: McpCommand::Serve { sharing },
        } => gigsift_mcp::serve(sharing).map_err(|error| anyhow::anyhow!("{error}")),
    };
    finished.map(|()| ExitCode::SUCCESS)
}

fn run_scan(path: Option<PathBuf>, json: bool) -> Result<()> {
    let envelope = storage_summary(&resolve_root(path)?)?;
    if json {
        print_json(&envelope)
    } else {
        human::print_scan(&envelope);
        Ok(())
    }
}

fn run_top(path: Option<PathBuf>, min_size: u64, limit: usize, json: bool) -> Result<()> {
    let envelope = largest_entries(&resolve_root(path)?, min_size, limit)?;
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

fn run_clean_candidates(path: Option<PathBuf>, active_within: u64, json: bool) -> Result<()> {
    let root = resolve_root(path)?;
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::for_user(&recipes, active_within);
    let envelope = find_candidates(&root, &analyzer)?;
    if json {
        print_json(&envelope)
    } else {
        human::print_candidates(&envelope);
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
    Store::for_user()?.save_plan(&envelope)?;
    if json {
        print_json(&envelope)
    } else {
        human::print_plan(&envelope);
        Ok(())
    }
}

fn run_clean_show(plan_id: &str, json: bool) -> Result<()> {
    let envelope = Store::for_user()?.load_plan(plan_id)?;
    if json {
        print_json(&envelope)
    } else {
        human::print_plan(&envelope);
        Ok(())
    }
}

/// Exits nonzero when any item failed, so scripts notice. Skips are not failures:
/// leaving a changed item alone is the correct outcome.
fn run_clean_apply(plan_id: &str, yes: bool, json: bool) -> Result<ExitCode> {
    let mut store = Store::for_user()?;
    let plan = store.load_plan(plan_id)?;
    if !yes && (json || !io::stdin().is_terminal()) {
        return Err(CliError::ConfirmationRequired.into());
    }
    if !yes && !confirmed(&plan)? {
        println!("Cancelled. Nothing was moved.");
        return Ok(ExitCode::SUCCESS);
    }

    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::for_user(&recipes, plan.data.active_within_seconds);
    let envelope = apply_plan(&plan, &analyzer, &NativeRecycler, &mut store)?;
    if json {
        print_json(&envelope)?;
    } else {
        human::print_apply(&envelope);
    }
    if envelope.data.failed_count > 0 {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn confirmed(plan: &Envelope<PlanData>) -> Result<bool> {
    human::print_plan(plan);
    println!();
    print!(
        "Recheck these {} items and move what still qualifies to the Trash? [y/N] ",
        plan.data.items.len()
    );
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn run_history(limit: u32, json: bool) -> Result<()> {
    let actions = Store::for_user()?.history(limit)?;
    let response = HistoryResponse {
        schema_version: SCHEMA_VERSION,
        command: "history".to_owned(),
        data: HistoryData { actions },
    };
    if json {
        print_json(&response)
    } else {
        human::print_history(&response.data);
        Ok(())
    }
}

fn resolve_root(path: Option<PathBuf>) -> Result<PathBuf> {
    match path {
        Some(path) => Ok(path),
        None => std::env::home_dir().context("could not determine your home directory"),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let mut value = serde_json::to_value(value)?;
    let sharing = JSON_SHARING.get().copied().unwrap_or(PathSharing::Full);
    redact(&mut value, sharing, std::env::home_dir().as_deref());
    println!("{}", serde_json::to_string_pretty(&value)?);
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
    if error.is::<CliError>() {
        return ErrorCode::ConfirmationRequired;
    }
    if error.is::<ApplyError>() {
        return ErrorCode::ApplyStopped;
    }
    match error.downcast_ref::<StoreError>() {
        Some(StoreError::InvalidPlanId(_)) => ErrorCode::InvalidPlanId,
        Some(StoreError::PlanNotFound(_)) => ErrorCode::PlanNotFound,
        Some(_) => ErrorCode::StorageFailed,
        None => ErrorCode::RootNotReadable,
    }
}
