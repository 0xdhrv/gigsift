//! Text output. Anything a script might read belongs in `--json` instead.

use gigsift_core::size::format_size;
use gigsift_core::{
    CompletenessStatus, EntryKind, Envelope, ExplainData, NetworkCost, PathKind, Protection,
    RebuildTime, Risk, SafetyVerdict, ScanData, SizedEntry, TopData,
};

pub fn print_scan(report: &Envelope<ScanData>) {
    let totals = &report.data.totals;
    println!("{}", report.data.root);
    println!("  On disk   {}", format_size(totals.allocated_bytes));
    println!("  Logical   {}", format_size(totals.logical_bytes));
    println!(
        "  Contents  {} files, {} folders, {} symlinks",
        totals.files, totals.directories, totals.symlinks
    );
    if totals.hard_link_duplicates > 0 {
        println!(
            "  Hard links  {} extra names not counted again",
            totals.hard_link_duplicates
        );
    }
    if totals.clones > 0 {
        println!("  Clones    {} files share blocks", totals.clones);
    }
    println!();
    println!("Largest folders");
    print_entries(&report.data.largest_directories);
    print_trust(report);
}

pub fn print_top(report: &Envelope<TopData>) {
    if report.data.entries.is_empty() {
        println!(
            "Nothing in {} uses {} or more.",
            report.data.root,
            format_size(report.data.min_allocated_bytes)
        );
    } else {
        print_entries(&report.data.entries);
    }
    if report.data.truncated > 0 {
        println!("...and {} more. Raise --limit.", report.data.truncated);
    }
    print_trust(report);
}

pub fn print_explain(report: &Envelope<ExplainData>) {
    let data = &report.data;
    println!("{}", data.path);
    println!("  Is        {}", describe_kind(data));
    if let Some(target) = &data.symlink_target {
        println!("  Points to {target}");
    }
    if data.kind != PathKind::Symlink {
        println!(
            "  Size      {} on disk, {} logical",
            format_size(data.allocated_bytes),
            format_size(data.logical_bytes)
        );
    }
    if data.hard_links > 1 {
        println!(
            "  Links     {} hard links. Removing one name frees nothing",
            data.hard_links
        );
    }
    if data.shares_blocks {
        println!("  Shared    Shares blocks with a clone, so removal frees less");
    }
    if let Some(project) = &data.project {
        println!("  Project   {} ({})", project.root, project.marker);
    }
    if let Some(known) = &data.recognized {
        println!("  What      {} [{}]", known.name, known.recipe_id);
        if known.matched_path != data.path {
            println!("  Inside    {}", known.matched_path);
        }
        println!("  Why       {}", known.reason);
        println!("  Loses     {}", known.loses);
        println!(
            "  Rebuild   {} ({}, {} network)",
            known.regenerate,
            rebuild_time(known.rebuild_cost.time),
            network_cost(known.rebuild_cost.network)
        );
        println!("  Risk      {}", risk(known.risk));
    }
    println!();
    println!("{}: {}", verdict(data.safety.verdict), data.safety.summary);
    for check in &data.safety.checks_pending {
        println!("  Not checked yet: {}", pending_check(*check));
    }
    print_trust(report);
}

fn describe_kind(data: &ExplainData) -> &'static str {
    match (data.kind, data.cloud_placeholder) {
        (PathKind::File, false) => "File",
        (PathKind::File, true) => "Cloud placeholder file",
        (PathKind::Directory, false) => "Folder",
        (PathKind::Directory, true) => "Cloud placeholder folder",
        (PathKind::Symlink, _) => "Symlink",
        (PathKind::Other, _) => "Special file",
    }
}

fn verdict(verdict: SafetyVerdict) -> &'static str {
    match verdict {
        SafetyVerdict::Protected => "Protected",
        SafetyVerdict::ReviewRequired => "Review required",
        SafetyVerdict::Unknown => "Unknown",
    }
}

fn pending_check(check: Protection) -> &'static str {
    match check {
        Protection::InsideActiveProject => "whether the project is in active use",
        Protection::ContainsGitTrackedFiles => "whether git tracks anything here",
        Protection::ContainsUntrackedWork => "whether it holds untracked work",
    }
}

fn rebuild_time(time: RebuildTime) -> &'static str {
    match time {
        RebuildTime::Seconds => "seconds",
        RebuildTime::Minutes => "minutes",
        RebuildTime::Hours => "hours",
    }
}

fn network_cost(cost: NetworkCost) -> &'static str {
    match cost {
        NetworkCost::None => "no",
        NetworkCost::Low => "low",
        NetworkCost::High => "high",
    }
}

fn risk(risk: Risk) -> &'static str {
    match risk {
        Risk::Low => "Low",
        Risk::Medium => "Medium",
        Risk::High => "High",
    }
}

fn print_entries(entries: &[SizedEntry]) {
    for entry in entries {
        let marker = match entry.kind {
            EntryKind::Directory => "/",
            EntryKind::File => "",
        };
        println!(
            "  {:>9}  {}{marker}",
            format_size(entry.allocated_bytes),
            entry.path
        );
    }
}

/// Keeps the numbers honest: a partial scan must say so next to them.
fn print_trust<T>(report: &Envelope<T>) {
    if report.scan_completeness.status == CompletenessStatus::Partial {
        println!();
        println!(
            "Partial scan: {} paths could not be read. Run with --json to list them.",
            report.inaccessible_paths.len()
        );
    }
    for warning in &report.warnings {
        eprintln!("warning: {}", warning.message);
    }
}
