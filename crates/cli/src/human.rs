//! Text output. Anything a script might read belongs in `--json` instead.

use anstream::{print, println};
use anstyle::Style;
use gigsift_core::size::format_size;
use gigsift_core::{
    ActionStatus, ApplyData, ApplyOutcome, BlockReason, Candidate, CandidatesData, ChangeKind,
    ChangesData, CompletenessStatus, EntryKind, Envelope, ExplainData, ForecastData, GitStatus,
    HistoryData, NetworkCost, Outlook, PathKind, PlanData, ProjectReport, ProjectsData,
    RebuildTime, Risk, SafetyVerdict, ScanData, SizedEntry, SnapshotData, TopData,
};
use jiff::Timestamp;

use crate::ui::{self, ACCENT, BAD, GOOD, MUTED, STRONG, WARN, paint};

pub fn print_scan(report: &Envelope<ScanData>) {
    let totals = &report.data.totals;
    ui::header("scan", &ui::home(&report.data.root));
    ui::field(
        "On disk",
        paint(STRONG, format_size(totals.allocated_bytes)),
    );
    ui::field("Logical", format_size(totals.logical_bytes));
    ui::field(
        "Contents",
        ui::joined([
            format!("{} files", ui::count(totals.files)),
            format!("{} folders", ui::count(totals.directories)),
            format!("{} symlinks", ui::count(totals.symlinks)),
        ]),
    );
    if totals.hard_link_duplicates > 0 {
        ui::field(
            "Hard links",
            format!(
                "{} extra names not counted again",
                ui::count(totals.hard_link_duplicates)
            ),
        );
    }
    if totals.clones > 0 {
        ui::field(
            "Clones",
            format!("{} files share blocks", ui::count(totals.clones)),
        );
    }
    ui::heading("Largest folders", "");
    print_entries(&report.data.largest_directories, &report.data.root);
    print_trust(report);
}

pub fn print_top(report: &Envelope<TopData>) {
    ui::header("top", &ui::home(&report.data.root));
    if report.data.entries.is_empty() {
        ui::note(format!(
            "Nothing here uses {} or more.",
            format_size(report.data.min_allocated_bytes)
        ));
    } else {
        print_entries(&report.data.entries, &report.data.root);
    }
    if report.data.truncated > 0 {
        ui::note(format!(
            "...and {} more. Raise --limit.",
            ui::count(report.data.truncated)
        ));
    }
    print_trust(report);
}

pub fn print_explain(report: &Envelope<ExplainData>) {
    let data = &report.data;
    ui::header("explain", &ui::home(&data.path));
    ui::field("Is", describe_kind(data));
    if let Some(target) = &data.symlink_target {
        ui::field("Points to", target);
    }
    if data.kind != PathKind::Symlink {
        ui::field(
            "Size",
            ui::joined([
                format!(
                    "{} on disk",
                    paint(STRONG, format_size(data.allocated_bytes))
                ),
                format!("{} logical", format_size(data.logical_bytes)),
            ]),
        );
    }
    if data.hard_links > 1 {
        ui::field(
            "Links",
            format!(
                "{} hard links. Removing one name frees nothing",
                data.hard_links
            ),
        );
    }
    if data.shares_blocks {
        ui::field(
            "Shared",
            "Shares blocks with a clone, so removal frees less",
        );
    }
    if let Some(project) = &data.project {
        ui::field(
            "Project",
            format!(
                "{}  {}",
                ui::home(&project.root),
                paint(MUTED, &project.marker)
            ),
        );
    }
    if let Some(known) = &data.recognized {
        ui::field(
            "What",
            format!("{}  {}", known.name, paint(MUTED, &known.recipe_id)),
        );
        if known.matched_path != data.path {
            ui::field("Inside", ui::home(&known.matched_path));
        }
        ui::field("Why", &known.reason);
        ui::field("Loses", &known.loses);
        ui::field(
            "Rebuild",
            rebuild(
                &known.regenerate,
                known.rebuild_cost.time,
                known.rebuild_cost.network,
            ),
        );
        ui::field("Risk", risk(known.risk));
    }
    let (style, title) = verdict(data.safety.verdict);
    let reasons: Vec<&str> = data
        .safety
        .blocked_by
        .iter()
        .map(|reason| block_reason(*reason))
        .collect();
    ui::callout(style, title, &data.safety.summary, &reasons);
    print_trust(report);
}

pub fn print_projects(report: &Envelope<ProjectsData>) {
    let data = &report.data;
    ui::header("projects", &ui::home(&data.root));
    if data.projects.is_empty() {
        ui::note("No projects with reclaimable artifacts here.");
    } else {
        let total: u64 = data
            .projects
            .iter()
            .map(|project| project.reclaimable_bytes_estimate)
            .sum();
        ui::field("Projects", ui::count(data.projects.len() as u64));
        ui::field("Reclaimable", paint(STRONG, format_size(total)));
    }
    let largest = data
        .projects
        .iter()
        .map(|project| project.reclaimable_bytes_estimate)
        .max()
        .unwrap_or(0);
    for project in &data.projects {
        println!();
        println!(
            "  {}  {}{}",
            ui::size(project.reclaimable_bytes_estimate),
            ui::bar(project.reclaimable_bytes_estimate, largest, ACCENT),
            ui::leaf(&ui::home(&project.root))
        );
        print_project_details(project);
    }
    if data.hidden_as_recent > 0 {
        println!();
        ui::note(format!(
            "{} more projects were active too recently to list.",
            ui::count(data.hidden_as_recent)
        ));
    }
    print_trust(report);
}

fn print_project_details(project: &ProjectReport) {
    let activity = project
        .last_activity
        .map_or_else(|| "Unknown activity".to_owned(), days_ago);
    ui::detail(paint(
        MUTED,
        format!("{activity}, {}", git_status(project.git_status)),
    ));
    for reason in &project.active_because {
        ui::detail(paint(WARN, format!("Active: {}", block_reason(*reason))));
    }
    for (index, artifact) in project.artifacts.iter().enumerate() {
        let branch = if index + 1 == project.artifacts.len() {
            "└─"
        } else {
            "├─"
        };
        let shared = if artifact.allocated_bytes == artifact.reclaimable_bytes_estimate {
            String::new()
        } else {
            paint(
                MUTED,
                format!("  of {} on disk", format_size(artifact.allocated_bytes)),
            )
        };
        let state = if artifact.blocked_by.is_empty() {
            String::new()
        } else {
            paint(WARN, "  (blocked)")
        };
        ui::detail(format!(
            "{} {}  {}{shared}{state}",
            paint(MUTED, branch),
            ui::size(artifact.reclaimable_bytes_estimate),
            ui::under(&artifact.path, &project.root)
        ));
    }
}

fn days_ago(time: Timestamp) -> String {
    match time.duration_until(Timestamp::now()).as_secs() / 86_400 {
        ..=0 => "Active today".to_owned(),
        1 => "Last active yesterday".to_owned(),
        days => format!("Last active {days} days ago"),
    }
}

pub fn print_plan(report: &Envelope<PlanData>) {
    let data = &report.data;
    ui::header("clean plan", &ui::home(&data.root));
    ui::field("Plan", &data.plan_id);
    let fill = if data.target_met { GOOD } else { WARN };
    ui::field(
        "Found",
        format!(
            "{}{} of {} target",
            ui::meter(data.reclaimable_bytes_estimate, data.target_bytes, fill),
            paint(STRONG, format_size(data.reclaimable_bytes_estimate)),
            format_size(data.target_bytes)
        ),
    );
    if !data.target_met {
        ui::field("Short by", paint(WARN, format_size(data.shortfall_bytes)));
    }
    if !data.items.is_empty() {
        ui::heading("Would move to Trash", items(data.items.len()));
        print_candidates_by_advice(data.items.iter().map(|item| &item.candidate));
    }
    if !data.advisories.is_empty() {
        ui::heading(
            "Needs your judgment, not included",
            items(data.advisories.len()),
        );
        print_candidates_by_advice(data.advisories.iter());
    }
    if !data.blocked.is_empty() {
        ui::heading("Blocked by a safety check", items(data.blocked.len()));
        for (blocked_by, group) in grouped(&data.blocked, |blocked| &blocked.blocked_by) {
            println!();
            let reasons = blocked_by.iter().map(|reason| block_reason(*reason));
            println!("  {}", paint(WARN, ui::joined(reasons)));
            for blocked in group {
                println!(
                    "  {}  {}",
                    ui::size(blocked.allocated_bytes),
                    ui::leaf(&ui::home(&blocked.path))
                );
            }
        }
    }
    println!();
    if !data.not_needed.is_empty() {
        ui::note(format!(
            "{} more eligible items not needed.",
            ui::count(data.not_needed.len() as u64)
        ));
    }
    ui::note("Nothing was changed. This is a plan only.");
    print_trust(report);
}

/// The step after reading a plan, left out when it is on its way to being applied.
pub fn print_apply_hint(plan: &PlanData) {
    if !plan.items.is_empty() {
        println!(
            "  {} {}",
            paint(MUTED, "Next"),
            paint(ACCENT, format!("gigsift clean apply {}", plan.plan_id))
        );
    }
}

fn items(count: usize) -> String {
    match count {
        1 => "1 item".to_owned(),
        _ => format!("{} items", ui::count(count as u64)),
    }
}

pub fn print_candidates(report: &Envelope<CandidatesData>) {
    let data = &report.data;
    ui::header("clean candidates", &ui::home(&data.root));
    ui::field(
        "Recoverable",
        paint(STRONG, format_size(data.reclaimable_bytes_estimate)),
    );
    print_candidates_by_advice(data.candidates.iter());
    print_trust(report);
}

pub fn print_apply(report: &Envelope<ApplyData>) {
    const REASON_INDENT: usize = 24;
    let data = &report.data;
    ui::header("clean apply", &data.plan_id);
    for result in &data.results {
        let path = ui::leaf(&ui::home(&result.path));
        match result.outcome {
            ApplyOutcome::Moved => {
                println!(
                    "  {}  {}  {path}",
                    paint(GOOD, "✓ Moved  "),
                    ui::size(result.moved_bytes)
                );
            }
            ApplyOutcome::Skipped => {
                println!("  {}  {:9}  {path}", paint(WARN, "– Skipped"), "");
                print_reasons(REASON_INDENT, &result.changed, &result.blocked_by);
            }
            ApplyOutcome::Failed => {
                println!("  {}  {:9}  {path}", paint(BAD, "✗ Failed "), "");
                if let Some(error) = &result.error {
                    println!("{:REASON_INDENT$}{}", "", paint(BAD, error));
                }
            }
        }
    }
    println!();
    println!(
        "  {}",
        ui::joined([
            tally(data.moved_count, "moved", GOOD),
            tally(data.skipped_count, "skipped", WARN),
            tally(data.failed_count, "failed", BAD),
        ])
    );
    if data.moved_count > 0 {
        ui::note(format!(
            "{} is now in the Trash. Disk space returns when you empty it.",
            format_size(data.moved_bytes)
        ));
    }
    print_trust(report);
}

/// Colored only when it counts something, so zeros fade out.
fn tally(count: u64, label: &str, style: Style) -> String {
    let style = if count > 0 { style } else { MUTED };
    paint(style, format!("{} {label}", ui::count(count)))
}

pub fn print_confirmation(count: usize) {
    println!();
    print!(
        "  {} Recheck {} and move what still qualifies to the Trash? {} ",
        paint(ACCENT, "?"),
        items(count),
        paint(MUTED, "[y/N]")
    );
}

pub fn print_cancelled() {
    ui::note("Cancelled. Nothing was moved.");
}

pub fn print_snapshot(report: &Envelope<SnapshotData>) {
    let data = &report.data;
    ui::header("snapshot", &ui::home(&data.root));
    ui::field("On disk", paint(STRONG, format_size(data.allocated_bytes)));
    ui::field(
        "Recorded",
        format!("{} folders", ui::count(data.directories_recorded)),
    );
    print_trust(report);
}

pub fn print_changes(report: &Envelope<ChangesData>) {
    let data = &report.data;
    ui::header("changes", &ui::home(&data.root));
    if let (Some(baseline), Some(delta)) = (&data.baseline, data.allocated_delta_bytes) {
        ui::field("Since", baseline.taken_at.strftime("%Y-%m-%d %H:%M"));
        ui::field(
            "Change",
            paint(delta_style(delta).bold(), signed_size(delta)),
        );
    }
    if !data.changes.is_empty() {
        println!();
    }
    let largest = data
        .changes
        .iter()
        .map(|change| change.delta_bytes.unsigned_abs())
        .max()
        .unwrap_or(0);
    for change in &data.changes {
        let about = if change.approximate { "~" } else { " " };
        let style = delta_style(change.delta_bytes);
        println!(
            "  {}{}  {}{}",
            paint(MUTED, about),
            paint(style, format!("{:>10}", signed_size(change.delta_bytes))),
            ui::bar(change.delta_bytes.unsigned_abs(), largest, style),
            ui::leaf(&ui::under(&change.path, &data.root))
        );
    }
    if data.truncated > 0 {
        ui::note(format!(
            "...and {} more. Raise --limit.",
            ui::count(data.truncated)
        ));
    }
    print_trust(report);
}

/// Growth is what needs attention here, so it takes the warning color.
fn delta_style(delta: i64) -> Style {
    if delta > 0 { WARN } else { GOOD }
}

fn signed_size(delta: i64) -> String {
    let sign = if delta < 0 { "-" } else { "+" };
    format!("{sign}{}", format_size(delta.unsigned_abs()))
}

pub fn print_forecast(data: &ForecastData) {
    let total = data.volume.total_bytes;
    let used = total.saturating_sub(data.volume.available_bytes);
    let percent = u128::from(used) * 100 / u128::from(total.max(1));
    let fill = match percent {
        90.. => BAD,
        75.. => WARN,
        _ => GOOD,
    };
    ui::header("forecast", "");
    ui::field(
        "Used",
        format!("{}{percent}%", ui::meter(used, total, fill)),
    );
    ui::field(
        "Free",
        format!(
            "{} of {}",
            paint(STRONG, format_size(data.volume.available_bytes)),
            format_size(total)
        ),
    );
    match data.outlook {
        Outlook::InsufficientData => {
            println!();
            ui::note(format!(
                "Not enough history yet: {} snapshots over {:.1} days.",
                data.samples, data.span_days
            ));
            ui::note(
                "Run `gigsift snapshot` regularly. Forecasts start after three spanning a day.",
            );
        }
        Outlook::NotFilling => {
            println!();
            ui::note(format!(
                "Free space isn't shrinking, across {} snapshots over {:.0} days.",
                data.samples, data.span_days
            ));
        }
        Outlook::Filling => {
            let used = data.bytes_used_per_day.unwrap_or(0.0).max(0.0) as u64;
            ui::field("Using", format!("About {} a day", format_size(used)));
            let low = data.days_until_full_low.unwrap_or(0.0);
            let best = data.days_until_full.unwrap_or(0.0);
            let range = data.days_until_full_high.map_or_else(
                || format!("{low:.0} days or later, and it may not fill at all"),
                |high| format!("{low:.0} to {high:.0} days"),
            );
            ui::field("Full in", paint(STRONG, format!("About {best:.0} days")));
            ui::field("Range", range);
        }
    }
}

pub fn print_history(data: &HistoryData) {
    const REASON_INDENT: usize = 35;
    ui::header("history", "");
    if data.actions.is_empty() {
        ui::note("gigsift has not done anything yet.");
    }
    for action in &data.actions {
        let (style, status) = action_status(action.status);
        println!(
            "  {}  {}  {}",
            paint(MUTED, action.started_at.strftime("%Y-%m-%d %H:%M")),
            paint(style, format!("{status:<13}")),
            ui::leaf(&ui::home(&action.path))
        );
        print_reasons(REASON_INDENT, &action.changed, &action.blocked_by);
        if let Some(error) = &action.error {
            println!("{:REASON_INDENT$}{}", "", paint(BAD, error));
        }
    }
}

fn print_reasons(indent: usize, changed: &[ChangeKind], blocked_by: &[BlockReason]) {
    let changes = changed.iter().map(|kind| change_kind(*kind));
    let blocks = blocked_by.iter().map(|reason| block_reason(*reason));
    for reason in changes.chain(blocks) {
        println!("{:indent$}{}", "", paint(MUTED, reason));
    }
}

fn action_status(status: ActionStatus) -> (Style, &'static str) {
    match status {
        ActionStatus::Interrupted => (BAD, "! interrupted"),
        ActionStatus::Moved => (GOOD, "✓ moved"),
        ActionStatus::Skipped => (WARN, "– skipped"),
        ActionStatus::Failed => (BAD, "✗ failed"),
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

/// Items from one recipe read alike, so the advice prints once above them.
fn print_candidates_by_advice<'a>(candidates: impl Iterator<Item = &'a Candidate>) {
    let candidates: Vec<&Candidate> = candidates.collect();
    let advice = |candidate: &&'a Candidate| {
        (
            &candidate.reason,
            &candidate.loses,
            &candidate.regenerate,
            candidate.rebuild_cost,
        )
    };
    for ((reason, loses, regenerate, cost), group) in grouped(&candidates, advice) {
        println!();
        println!("  {reason}");
        ui::field("Loses", loses);
        ui::field("Rebuild", rebuild(regenerate, cost.time, cost.network));
        for candidate in group {
            println!(
                "  {}  {}",
                ui::size(candidate.reclaimable_bytes_estimate),
                ui::leaf(&ui::home(&candidate.path))
            );
        }
    }
}

/// Groups by `key`, keeping the order each key and each item first appeared in.
fn grouped<'a, T, K: PartialEq>(items: &'a [T], key: impl Fn(&'a T) -> K) -> Vec<(K, Vec<&'a T>)> {
    let mut groups: Vec<(K, Vec<&'a T>)> = Vec::new();
    for item in items {
        let item_key = key(item);
        match groups.iter_mut().find(|(group, _)| *group == item_key) {
            Some((_, group)) => group.push(item),
            None => groups.push((item_key, vec![item])),
        }
    }
    groups
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

/// None of these is green: no verdict means safe. Only a plan can establish that.
fn verdict(verdict: SafetyVerdict) -> (Style, &'static str) {
    match verdict {
        SafetyVerdict::Protected => (BAD, "Protected"),
        SafetyVerdict::Blocked => (WARN, "Blocked"),
        SafetyVerdict::ReviewRequired => (ACCENT, "Review required"),
        SafetyVerdict::Unknown => (MUTED, "Unknown"),
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

fn risk(risk: Risk) -> String {
    match risk {
        Risk::Low => paint(GOOD, "Low"),
        Risk::Medium => paint(WARN, "Medium"),
        Risk::High => paint(BAD, "High"),
    }
}

fn rebuild(command: &str, time: RebuildTime, network: NetworkCost) -> String {
    ui::joined([
        paint(ACCENT, command),
        rebuild_time(time).to_owned(),
        format!("{} network", network_cost(network)),
    ])
}

fn print_entries(entries: &[SizedEntry], root: &str) {
    let largest = entries
        .iter()
        .map(|entry| entry.allocated_bytes)
        .max()
        .unwrap_or(0);
    for entry in entries {
        let marker = match entry.kind {
            EntryKind::Directory => "/",
            EntryKind::File => "",
        };
        println!(
            "  {}  {}{}",
            ui::size(entry.allocated_bytes),
            ui::bar(entry.allocated_bytes, largest, ACCENT),
            ui::leaf(&format!("{}{marker}", ui::under(&entry.path, root)))
        );
    }
}

/// Keeps the numbers honest: a partial scan must say so next to them.
fn print_trust<T>(report: &Envelope<T>) {
    if report.scan_completeness.status == CompletenessStatus::Partial {
        ui::callout(
            WARN,
            "Partial scan",
            &format!(
                "{} paths could not be read. Run with --json to list them.",
                ui::count(report.inaccessible_paths.len() as u64)
            ),
            &[],
        );
    }
    for warning in &report.warnings {
        ui::warning(&warning.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_keeps_first_seen_order_and_every_item() {
        let items = ["apple", "bean", "avocado", "cherry", "blueberry"];

        let groups = grouped(&items, |item| item.chars().next());

        let expected = vec![
            (Some('a'), vec![&"apple", &"avocado"]),
            (Some('b'), vec![&"bean", &"blueberry"]),
            (Some('c'), vec![&"cherry"]),
        ];
        assert_eq!(groups, expected);
    }

    #[test]
    fn item_counts_read_naturally() {
        assert_eq!(items(1), "1 item");
        assert_eq!(items(2), "2 items");
        assert_eq!(items(1_200), "1,200 items");
    }
}
