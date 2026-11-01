//! Text output. Anything a script might read belongs in `--json` instead.

use gigsift_core::size::format_size;
use gigsift_core::{
    ActionStatus, ApplyData, ApplyOutcome, BlockReason, Candidate, ChangeKind, CompletenessStatus,
    EntryKind, Envelope, ExplainData, GitStatus, HistoryData, NetworkCost, PathKind, PlanData,
    ProjectsData, RebuildTime, Risk, SafetyVerdict, ScanData, SizedEntry, TopData,
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
    for reason in &data.safety.blocked_by {
        println!("  Blocked: {}", block_reason(*reason));
    }
    print_trust(report);
}

pub fn print_projects(report: &Envelope<ProjectsData>) {
    let data = &report.data;
    if data.projects.is_empty() {
        println!("No projects with reclaimable artifacts in {}.", data.root);
    }
    for project in &data.projects {
        println!(
            "{:>9}  {}",
            format_size(project.reclaimable_bytes_estimate),
            project.root
        );
        let activity = project.last_activity.map_or_else(
            || "unknown".to_owned(),
            |time| time.strftime("%Y-%m-%d").to_string(),
        );
        println!(
            "           Last activity {activity}, {}",
            git_status(project.git_status)
        );
        for reason in &project.active_because {
            println!("           Active: {}", block_reason(*reason));
        }
        for artifact in &project.artifacts {
            let state = if artifact.blocked_by.is_empty() {
                ""
            } else {
                "  (blocked)"
            };
            println!(
                "           {:>9}  {}{state}",
                format_size(artifact.allocated_bytes),
                artifact.path
            );
        }
    }
    if data.hidden_as_recent > 0 {
        println!(
            "{} more projects were active too recently to list.",
            data.hidden_as_recent
        );
    }
    print_trust(report);
}

pub fn print_plan(report: &Envelope<PlanData>) {
    let data = &report.data;
    println!("Plan {}", data.plan_id);
    println!(
        "Target {}, found {} under {}",
        format_size(data.target_bytes),
        format_size(data.reclaimable_bytes_estimate),
        data.root
    );
    if !data.target_met {
        println!(
            "Short of the target by {}.",
            format_size(data.shortfall_bytes)
        );
    }
    if !data.items.is_empty() {
        println!();
        println!("Would move to Trash");
        for item in &data.items {
            print_candidate(&item.candidate);
        }
    }
    if !data.advisories.is_empty() {
        println!();
        println!("Needs your judgment, not included");
        for candidate in &data.advisories {
            print_candidate(candidate);
        }
    }
    if !data.blocked.is_empty() {
        println!();
        println!("Blocked by a safety check");
        for blocked in &data.blocked {
            let reasons: Vec<&str> = blocked
                .blocked_by
                .iter()
                .map(|r| block_reason(*r))
                .collect();
            println!(
                "  {:>9}  {}",
                format_size(blocked.allocated_bytes),
                blocked.path
            );
            println!("             {}", reasons.join("; "));
        }
    }
    if !data.not_needed.is_empty() {
        println!();
        println!("{} more eligible items not needed.", data.not_needed.len());
    }
    println!();
    println!("Nothing was changed. This is a plan only.");
    print_trust(report);
}

pub fn print_apply(report: &Envelope<ApplyData>) {
    let data = &report.data;
    for result in &data.results {
        match result.outcome {
            ApplyOutcome::Moved => {
                println!(
                    "  Moved    {:>9}  {}",
                    format_size(result.moved_bytes),
                    result.path
                );
            }
            ApplyOutcome::Skipped => {
                println!("  Skipped             {}", result.path);
                print_reasons(&result.changed, &result.blocked_by);
            }
            ApplyOutcome::Failed => {
                println!("  Failed              {}", result.path);
                if let Some(error) = &result.error {
                    println!("                      {error}");
                }
            }
        }
    }
    println!();
    println!(
        "{} moved, {} skipped, {} failed.",
        data.moved_count, data.skipped_count, data.failed_count
    );
    if data.moved_count > 0 {
        println!(
            "{} is now in the Trash. Disk space returns when you empty it.",
            format_size(data.moved_bytes)
        );
    }
    print_trust(report);
}

pub fn print_history(data: &HistoryData) {
    if data.actions.is_empty() {
        println!("gigsift has not done anything yet.");
    }
    for action in &data.actions {
        println!(
            "{}  {:<11}  {}",
            action.started_at.strftime("%Y-%m-%d %H:%M"),
            action_status(action.status),
            action.path
        );
        print_reasons(&action.changed, &action.blocked_by);
        if let Some(error) = &action.error {
            println!("                      {error}");
        }
    }
}

fn print_reasons(changed: &[ChangeKind], blocked_by: &[BlockReason]) {
    let changes = changed.iter().map(|kind| change_kind(*kind));
    let blocks = blocked_by.iter().map(|reason| block_reason(*reason));
    for reason in changes.chain(blocks) {
        println!("                      {reason}");
    }
}

fn action_status(status: ActionStatus) -> &'static str {
    match status {
        ActionStatus::Interrupted => "interrupted",
        ActionStatus::Moved => "moved",
        ActionStatus::Skipped => "skipped",
        ActionStatus::Failed => "failed",
    }
}

fn change_kind(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Missing => "no longer there",
        ChangeKind::NotADirectory => "no longer a folder",
        ChangeKind::DifferentFile => "a different folder is here now",
        ChangeKind::ParentChanged => "a folder above it was replaced",
        ChangeKind::Modified => "changed since the plan",
        ChangeKind::SizeChanged => "size changed since the plan",
        ChangeKind::NoLongerRecognized => "no longer recognized",
        ChangeKind::IdentityUnavailable => "could not be identified",
    }
}

fn print_candidate(candidate: &Candidate) {
    println!(
        "  {:>9}  {}",
        format_size(candidate.reclaimable_bytes_estimate),
        candidate.path
    );
    println!("             {}", candidate.reason);
    println!("             Loses: {}", candidate.loses);
    println!(
        "             Rebuild: {} ({}, {} network)",
        candidate.regenerate,
        rebuild_time(candidate.rebuild_cost.time),
        network_cost(candidate.rebuild_cost.network)
    );
}

fn git_status(status: GitStatus) -> &'static str {
    match status {
        GitStatus::Clean => "git clean",
        GitStatus::Dirty => "uncommitted changes",
        GitStatus::Untracked => "untracked files",
        GitStatus::UnpushedCommits => "unpushed commits",
        GitStatus::NotARepository => "not in git",
        GitStatus::Unknown => "git status unknown",
    }
}

fn block_reason(reason: BlockReason) -> &'static str {
    match reason {
        BlockReason::RecentActivity => "project changed recently",
        BlockReason::RunningProcess => "a process is working in this project",
        BlockReason::UncommittedChanges => "project has uncommitted changes",
        BlockReason::UnpushedCommits => "project has unpushed commits",
        BlockReason::GitTrackedFiles => "git tracks files here",
        BlockReason::UntrackedWork => "holds untracked files",
        BlockReason::NotIgnoredByGit => "git does not ignore this",
        BlockReason::GitUnavailable => "git could not answer",
        BlockReason::AppRunning => "the app that owns this is running",
        BlockReason::ProtectedPath => "protected path",
        BlockReason::NonUnicodePath => "path is not valid Unicode",
        BlockReason::IdentityUnavailable => "the OS would not identify this path",
    }
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
        SafetyVerdict::Blocked => "Blocked",
        SafetyVerdict::ReviewRequired => "Review required",
        SafetyVerdict::Unknown => "Unknown",
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
