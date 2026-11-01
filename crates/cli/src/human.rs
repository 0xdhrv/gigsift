//! Text output. Anything a script might read belongs in `--json` instead.

use gigsift_core::size::format_size;
use gigsift_core::{CompletenessStatus, EntryKind, Envelope, ScanData, SizedEntry, TopData};

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
