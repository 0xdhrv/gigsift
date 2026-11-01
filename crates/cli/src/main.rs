mod human;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gigsift_core::size::parse_size;
use gigsift_core::{
    EntryKind, Envelope, ErrorBody, ErrorCode, ErrorResponse, SCHEMA_VERSION, ScanData, SizedEntry,
    TopData,
};
use gigsift_engine::explain;
use gigsift_recipes::RecipeSet;
use gigsift_scanner::{Entry, ScanOptions, scan};
use serde::Serialize;

const SCAN_DIRECTORIES_SHOWN: usize = 20;

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
        /// Print the versioned JSON contract instead of text.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (name, json) = match &cli.command {
        Command::Scan { json, .. } => ("scan", *json),
        Command::Top { json, .. } => ("top", *json),
        Command::Explain { json, .. } => ("explain", *json),
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
        Command::Explain { path, json } => run_explain(&path, json),
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

fn run_explain(path: &Path, json: bool) -> Result<()> {
    let envelope = explain(path, &RecipeSet::builtin())?;
    if json {
        print_json(&envelope)
    } else {
        human::print_explain(&envelope);
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
            code: ErrorCode::RootNotReadable,
            message: format!("{error:#}"),
        },
    };
    if let Err(print_error) = print_json(&response) {
        eprintln!("gigsift: {print_error:#}");
    }
}
