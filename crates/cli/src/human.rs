//! Text output. Anything a script might read belongs in `--json` instead.

use std::path::Path;

use anstream::{print, println};
use anstyle::Style;
use gigsift_core::size::format_size;
use gigsift_core::{
    ActionStatus, ApplyData, ApplyOutcome, BlockReason, Candidate, CandidatesData, ChangeKind,
    ChangesData, CompletenessStatus, EntryKind, Envelope, ExplainData, ForecastData, GitStatus,
    HistoryData, NetworkCost, Outlook, PathKind, PlanData, PlanItem, ProjectReport, ProjectsData,
    RebuildTime, Risk, SafetyVerdict, ScanData, SizedEntry, SnapshotData, SpaceSample, TopData,
};
use jiff::Timestamp;

use crate::chart::{self, Point};
use crate::motion;
use crate::ui::{self, ACCENT, BAD, Figure, GOOD, MUTED, STRONG, Stat, WARN, paint};

const SECONDS_PER_DAY: f64 = 86_400.0;

pub fn print_scan(report: &Envelope<ScanData>) {
    let totals = &report.data.totals;
    ui::header("scan", &ui::home(&report.data.root));
    ui::stats(&[
        Stat::new("On disk", Figure::Bytes(totals.allocated_bytes)).styled(ui::brand(0.25).bold()),
        Stat::new("Logical", Figure::Bytes(totals.logical_bytes)),
        Stat::new("Files", Figure::Count(totals.files)),
        Stat::new("Folders", Figure::Count(totals.directories)),
        Stat::new("Symlinks", Figure::Count(totals.symlinks)),
    ]);
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
    print_entries(
        &report.data.largest_directories,
        &report.data.root,
        Some(totals.allocated_bytes),
        true,
    );
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
        let root = Path::new(&report.data.root);
        let total =
            report.data.entries.iter().find_map(|entry| {
                (Path::new(&entry.path) == root).then_some(entry.allocated_bytes)
            });
        print_entries(&report.data.entries, &report.data.root, total, false);
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
    let total: u64 = data
        .projects
        .iter()
        .map(|project| project.reclaimable_bytes_estimate)
        .sum();
    if data.projects.is_empty() {
        ui::note("No projects with reclaimable artifacts here.");
    } else {
        ui::stats(&[
            Stat::new("Reclaimable", Figure::Bytes(total)).styled(ui::brand(0.25).bold()),
            Stat::new("Projects", Figure::Count(data.projects.len() as u64)),
        ]);
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
            "  {}  {}{}{}",
            ui::size(project.reclaimable_bytes_estimate),
            ui::heat_bar(project.reclaimable_bytes_estimate, largest),
            ui::share(project.reclaimable_bytes_estimate, Some(total)),
            ui::link(&project.root, ui::leaf(&ui::home(&project.root)))
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
            ui::link(&artifact.path, ui::under(&artifact.path, &project.root))
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
    let fill = if data.target_met { GOOD } else { WARN };
    let found = data.reclaimable_bytes_estimate;
    let mut stats = vec![
        Stat::new("Found", Figure::Bytes(found)).styled(fill.bold()),
        Stat::new("Target", Figure::Bytes(data.target_bytes)),
    ];
    if !data.target_met {
        stats.push(Stat::new("Short by", Figure::Bytes(data.shortfall_bytes)).styled(WARN));
    }
    ui::stats(&stats);
    print_gauge(found, data.target_bytes, fill);
    ui::field("Plan", &data.plan_id);
    print_breakdown(&data.items);
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
                    ui::link(&blocked.path, ui::leaf(&ui::home(&blocked.path)))
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

/// How full something is, for a terminal. Cards already gave the numbers.
fn print_gauge(value: u64, max: u64, style: Style) {
    if ui::rich() {
        motion::reveal(|progress| {
            vec![format!(
                "  {}{}",
                ui::meter(ui::scaled(value, progress), max, style),
                ui::share(value, Some(max))
            )]
        });
    }
}

/// What a plan is made of, by the recipe that matched it.
fn print_breakdown(items: &[PlanItem]) {
    if !ui::colored() || items.is_empty() {
        return;
    }
    let mut parts: Vec<(&str, u64)> = grouped(items, |item| &item.recipe_id)
        .into_iter()
        .map(|(recipe, group)| {
            let bytes = group
                .iter()
                .map(|item| item.candidate.reclaimable_bytes_estimate)
                .sum();
            (recipe.as_str(), bytes)
        })
        .collect();
    parts.sort_by_key(|part| std::cmp::Reverse(part.1));
    let total = parts.iter().map(|part| part.1).sum();
    parts.truncate(ui::SERIES_COUNT);
    let sizes: Vec<u64> = parts.iter().map(|part| part.1).collect();
    let legend = ui::legend(&parts);
    println!();
    motion::reveal(|progress| {
        let mut rows = vec![ui::stacked(&sizes, total, progress)];
        rows.extend(legend.iter().cloned());
        rows
    });
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
    ui::stats(&[
        Stat::new(
            "Recoverable",
            Figure::Bytes(data.reclaimable_bytes_estimate),
        )
        .styled(ui::brand(0.25).bold()),
        Stat::new("Items", Figure::Count(data.candidates.len() as u64)),
    ]);
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
                    paint(GOOD, format!("{} Moved  ", moved_mark())),
                    ui::size(result.moved_bytes)
                );
            }
            ApplyOutcome::Skipped => {
                println!("  {}  {:9}  {path}", paint(WARN, "– Skipped"), "");
                print_reasons(REASON_INDENT, &result.changed, &result.blocked_by);
            }
            ApplyOutcome::Failed => {
                println!(
                    "  {}  {:9}  {path}",
                    paint(BAD, format!("{} Failed ", failed_mark())),
                    ""
                );
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
    ui::stats(&[
        Stat::new("On disk", Figure::Bytes(data.allocated_bytes)).styled(ui::brand(0.25).bold()),
        Stat::new("Folders", Figure::Count(data.directories_recorded)),
    ]);
    print_trust(report);
}

pub fn print_changes(report: &Envelope<ChangesData>) {
    let data = &report.data;
    ui::header("changes", &ui::home(&data.root));
    if let (Some(baseline), Some(delta)) = (&data.baseline, data.allocated_delta_bytes) {
        ui::stats(&[
            Stat::new("Change", Figure::Text(signed_size(delta))).styled(delta_style(delta).bold()),
            Stat::new(
                "Since",
                Figure::Text(baseline.taken_at.strftime("%Y-%m-%d %H:%M").to_string()),
            ),
        ]);
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
    motion::reveal(|progress| {
        data.changes
            .iter()
            .map(|change| {
                let about = if change.approximate { "~" } else { " " };
                let style = delta_style(change.delta_bytes);
                format!(
                    "  {}{}  {}{}",
                    paint(MUTED, about),
                    paint(style, format!("{:>10}", signed_size(change.delta_bytes))),
                    ui::diverging(
                        ui::scaled(change.delta_bytes.unsigned_abs(), progress),
                        change.delta_bytes < 0,
                        largest,
                        style
                    ),
                    ui::link(&change.path, ui::leaf(&ui::under(&change.path, &data.root)))
                )
            })
            .collect()
    });
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

pub fn print_forecast(data: &ForecastData, samples: &[SpaceSample]) {
    let total = data.volume.total_bytes;
    let used = total.saturating_sub(data.volume.available_bytes);
    let percent = u128::from(used) * 100 / u128::from(total.max(1));
    let fill = match percent {
        90.. => BAD,
        75.. => WARN,
        _ => GOOD,
    };
    ui::header("forecast", "");
    let mut stats = vec![
        Stat::new("Used", Figure::Text(format!("{percent}%"))).styled(fill.bold()),
        Stat::new("Free", Figure::Bytes(data.volume.available_bytes)),
        Stat::new("Disk", Figure::Bytes(total)),
    ];
    if data.outlook == Outlook::Filling {
        let best = data.days_until_full.unwrap_or(0.0);
        stats.push(Stat::new(
            "Full in",
            Figure::Text(format!("About {best:.0} days")),
        ));
    }
    ui::stats(&stats);
    print_gauge(used, total, fill);
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
            let range = data.days_until_full_high.map_or_else(
                || format!("{low:.0} days or later, and it may not fill at all"),
                |high| format!("{low:.0} to {high:.0} days"),
            );
            ui::field("Range", range);
        }
    }
    print_usage_chart(data, samples);
}

/// Disk use over time, with the trend carried forward faintly.
fn print_usage_chart(data: &ForecastData, samples: &[SpaceSample]) {
    let Some(usage) = UsageChart::new(data, samples) else {
        return;
    };
    if ui::rich() {
        println!();
        motion::reveal(|progress| usage.rows(progress));
    }
}

/// Fractions of the disk over time, in seconds.
struct UsageChart {
    history: Vec<(f64, f64)>,
    projection: Option<(f64, f64)>,
    /// `(lowest, highest)` shown.
    range: (f64, f64),
}

impl UsageChart {
    const HEIGHT: usize = 8;
    const GUTTER: usize = 9;
    /// Past this many times the recorded span, a projection is just a guess.
    const HORIZON: f64 = 2.0;

    fn new(data: &ForecastData, samples: &[SpaceSample]) -> Option<Self> {
        let total = data.volume.total_bytes.max(1) as f64;
        let used_share = |available: u64| 1.0 - available as f64 / total;
        let mut history: Vec<(f64, f64)> = samples
            .iter()
            .map(|sample| {
                let seconds = sample.taken_at.as_second() as f64;
                (seconds, used_share(sample.available_bytes))
            })
            .collect();
        history.push((
            Timestamp::now().as_second() as f64,
            used_share(data.volume.available_bytes),
        ));
        let (first, last) = (*history.first()?, *history.last()?);
        let span = last.0 - first.0;
        if history.len() < 3 || span <= 0.0 {
            return None;
        }
        let projection = data
            .days_until_full
            .filter(|_| data.outlook == Outlook::Filling);
        let projection = projection.map(|days| {
            let seconds = (days * SECONDS_PER_DAY).min(span * Self::HORIZON);
            let per_second = data.bytes_used_per_day.unwrap_or(0.0) / total / SECONDS_PER_DAY;
            (last.0 + seconds, (last.1 + per_second * seconds).min(1.0))
        });
        let levels = || {
            history
                .iter()
                .map(|point| point.1)
                .chain(projection.map(|end| end.1))
        };
        let (low, high) = (levels().fold(1.0, f64::min), levels().fold(0.0, f64::max));
        let margin = ((high - low) * 0.15).max(0.005);
        let range = ((low - margin).max(0.0), (high + margin).min(1.0));
        Some(Self {
            history,
            projection,
            range,
        })
    }

    fn width() -> usize {
        ui::window().0.saturating_sub(Self::GUTTER + 3).min(64)
    }

    fn point(&self, (seconds, share): (f64, f64)) -> Point {
        let start = self.history.first().map_or(0.0, |point| point.0);
        let end = self.projection.or(self.history.last().copied());
        let end = end.map_or(1.0, |point| point.0);
        let (low, high) = self.range;
        (
            ((seconds - start) / (end - start)) as f32,
            ((share - low) / (high - low)) as f32,
        )
    }

    fn rows(&self, progress: f32) -> Vec<String> {
        let width = Self::width();
        let history: Vec<Point> = self
            .history
            .iter()
            .map(|point| self.point(*point))
            .collect();
        let projection = self.projection.map(|point| self.point(point));
        let plot = chart::area(&history, projection, width, Self::HEIGHT, progress);
        let last_row = plot.len() - 1;
        let (low, high) = self.range;
        let mut rows: Vec<String> = plot
            .into_iter()
            .enumerate()
            .map(|(row, line)| {
                let label = match row {
                    0 => percent_label(high, high - low),
                    _ if row == last_row => percent_label(low, high - low),
                    _ => String::new(),
                };
                let tick = if label.is_empty() { "│" } else { "┤" };
                format!(
                    "{}{} {line}",
                    paint(MUTED, format!("{label:>width$} ", width = Self::GUTTER - 1)),
                    paint(MUTED, tick)
                )
            })
            .collect();
        rows.push(paint(
            MUTED,
            format!(
                "{:width$}└{}",
                "",
                "─".repeat(width + 1),
                width = Self::GUTTER
            ),
        ));
        rows.push(self.dates(width));
        rows
    }

    /// The first date, today, and the end of the projection, under where they fall.
    fn dates(&self, width: usize) -> String {
        let date = |seconds: f64| {
            Timestamp::from_second(seconds as i64)
                .map(|time| time.strftime("%b %-d").to_string())
                .unwrap_or_default()
        };
        let start = self.history.first().map_or(0.0, |point| point.0);
        let now = self.history.last().map_or(0.0, |point| point.0);
        let mut line = date(start);
        let today = (self.point((now, 0.0)).0 * width as f32) as usize;
        if let Some((end, _)) = self.projection {
            let right = date(end);
            line.push_str(&" ".repeat(today.saturating_sub(line.len() + 2).max(2)));
            line.push_str("today");
            let gap = (width + 1).saturating_sub(line.len() + right.len()).max(2);
            line.push_str(&" ".repeat(gap));
            line.push_str(&right);
        } else {
            let gap = (width + 1).saturating_sub(line.len() + 5).max(2);
            line.push_str(&" ".repeat(gap));
            line.push_str("today");
        }
        paint(MUTED, format!("{:width$} {line}", "", width = Self::GUTTER))
    }
}

/// Close-up charts need the decimal to tell their two ends apart.
fn percent_label(share: f64, range: f64) -> String {
    if range < 0.05 {
        format!("{:.1}%", share * 100.0)
    } else {
        format!("{:.0}%", share * 100.0)
    }
}

pub fn print_history(data: &HistoryData) {
    const REASON_INDENT: usize = 35;
    ui::header("history", "");
    if data.actions.is_empty() {
        ui::note("gigsift has not done anything yet.");
    } else {
        let moved = data
            .actions
            .iter()
            .filter(|action| action.status == ActionStatus::Moved);
        let (count, bytes) = moved.fold((0, 0), |(count, bytes), action| {
            (count + 1, bytes + action.moved_bytes)
        });
        ui::stats(&[
            Stat::new("Moved", Figure::Bytes(bytes)).styled(ui::brand(0.25).bold()),
            Stat::new("Items", Figure::Count(count)),
            Stat::new("Actions", Figure::Count(data.actions.len() as u64)),
        ]);
        println!();
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

fn action_status(status: ActionStatus) -> (Style, String) {
    let (style, mark, word) = match status {
        ActionStatus::Interrupted => (BAD, "!", "interrupted"),
        ActionStatus::Moved => (GOOD, moved_mark(), "moved"),
        ActionStatus::Skipped => (WARN, "–", "skipped"),
        ActionStatus::Failed => (BAD, failed_mark(), "failed"),
    };
    (style, format!("{mark} {word}"))
}

fn moved_mark() -> &'static str {
    ui::glyph("✓", "√")
}

fn failed_mark() -> &'static str {
    ui::glyph("✗", "x")
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
                ui::link(&candidate.path, ui::leaf(&ui::home(&candidate.path)))
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

/// With `split`, a stacked bar shows how the folders divide up the whole.
fn print_entries(entries: &[SizedEntry], root: &str, total: Option<u64>, split: bool) {
    let largest = entries
        .iter()
        .map(|entry| entry.allocated_bytes)
        .max()
        .unwrap_or(0);
    let parts = if split {
        outermost(entries, root)
    } else {
        Vec::new()
    };
    let sizes: Vec<u64> = parts
        .iter()
        .map(|part| entries[*part].allocated_bytes)
        .collect();
    let split = split && ui::colored();
    motion::reveal(|progress| {
        let mut rows = Vec::with_capacity(entries.len() + 2);
        if split {
            rows.push(String::new());
            rows.push(ui::stacked(&sizes, total.unwrap_or(largest), progress));
            rows.push(String::new());
        }
        rows.extend(entries.iter().enumerate().map(|(index, entry)| {
            let marker = match entry.kind {
                EntryKind::Directory => "/",
                EntryKind::File => "",
            };
            let swatch = if split {
                ui::swatch(parts.iter().position(|part| *part == index))
            } else {
                String::new()
            };
            format!(
                "  {}  {swatch}{}{}{}",
                ui::size(entry.allocated_bytes),
                ui::heat_bar(ui::scaled(entry.allocated_bytes, progress), largest),
                ui::share(entry.allocated_bytes, total),
                ui::link(
                    &entry.path,
                    ui::leaf(&format!("{}{marker}", ui::under(&entry.path, root)))
                )
            )
        }));
        rows
    });
}

/// Entries that don't overlap, because nested folders would count twice in a whole.
/// Parents are at least as large as their children, so they come first.
fn outermost(entries: &[SizedEntry], root: &str) -> Vec<usize> {
    let mut chosen: Vec<usize> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let path = Path::new(&entry.path);
        let nested = chosen
            .iter()
            .any(|parent| path.starts_with(&entries[*parent].path));
        if path != Path::new(root) && !nested && chosen.len() < ui::SERIES_COUNT {
            chosen.push(index);
        }
    }
    chosen
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
