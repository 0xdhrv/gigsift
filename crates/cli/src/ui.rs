//! Terminal styling. Commands name a role here rather than picking colors, and
//! every role is a plain ANSI color, so text follows the user's terminal theme on
//! light and dark backgrounds alike. Only the brand marks go past that: the logo,
//! size bars and spinner use a truecolor ramp, or the accent where that's missing.
//!
//! `anstream` drops the styling for pipes and `NO_COLOR`. Shortened paths and bars
//! need more care: they only appear on a terminal, so piped text keeps real paths.

use std::fmt::Display;
use std::io::{self, IsTerminal};
use std::path::{Path, is_separator};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use anstream::{eprint, eprintln, println};
use anstyle::{AnsiColor, RgbColor, Style};
use gigsift_core::size::format_size;

pub const ACCENT: Style = AnsiColor::Cyan.on_default();
pub const GOOD: Style = AnsiColor::Green.on_default();
pub const WARN: Style = AnsiColor::Yellow.on_default();
pub const BAD: Style = AnsiColor::Red.on_default();
pub const MUTED: Style = Style::new().dimmed();
pub const STRONG: Style = Style::new().bold();

/// Column where a row's details start, under its size.
pub const DETAIL_INDENT: usize = 13;

/// Mid-tones, so the ramp reads on light and dark backgrounds alike.
const BRAND: [RgbColor; 3] = [
    RgbColor(0x10, 0xB5, 0xA0),
    RgbColor(0x12, 0x9C, 0xD8),
    RgbColor(0x52, 0x74, 0xF0),
];

const BAR_CELLS: usize = 20;
const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
/// Sand filling a cell and draining out again.
const SPINNER: [&str; 35] = [
    "⠁", "⠂", "⠄", "⡀", "⡈", "⡐", "⡠", "⣀", "⣁", "⣂", "⣄", "⣌", "⣔", "⣤", "⣥", "⣦", "⣮", "⣶", "⣷",
    "⣿", "⡿", "⠿", "⢟", "⠟", "⡛", "⠛", "⠫", "⢋", "⠋", "⠍", "⡉", "⠉", "⠑", "⠡", "⢁",
];

static RICH: LazyLock<bool> = LazyLock::new(|| io::stdout().is_terminal());
static TRUECOLOR: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("COLORTERM").is_some_and(|value| value == "truecolor" || value == "24bit")
});

/// Styles after any padding, because escape codes would count toward the width.
pub fn paint(style: Style, text: impl Display) -> String {
    format!("{style}{text}{style:#}")
}

/// A point on the brand ramp, from 0 to 1.
pub fn brand(at: f32) -> Style {
    if *TRUECOLOR {
        ramp(at).on_default()
    } else {
        ACCENT
    }
}

fn ramp(at: f32) -> RgbColor {
    let spans = (BRAND.len() - 1) as f32;
    let position = at.clamp(0.0, 1.0) * spans;
    let span = (position as usize).min(BRAND.len() - 2);
    let (from, to) = (BRAND[span], BRAND[span + 1]);
    let blend = |a: u8, b: u8| {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * (position - span as f32)) as u8
    };
    RgbColor(
        blend(from.0, to.0),
        blend(from.1, to.1),
        blend(from.2, to.2),
    )
}

pub fn joined<T: Display>(parts: impl IntoIterator<Item = T>) -> String {
    let parts: Vec<String> = parts.into_iter().map(|part| part.to_string()).collect();
    parts.join(&paint(MUTED, " · "))
}

pub fn header(command: &str, subject: &str) {
    println!();
    println!(
        "  {}  {}",
        paint(badge(), format!(" ◆ {command} ")),
        leaf(subject)
    );
    println!();
}

fn badge() -> Style {
    let (ink, fill) = if *TRUECOLOR {
        (RgbColor(0x04, 0x18, 0x1C).into(), ramp(0.25).into())
    } else {
        (AnsiColor::Black.into(), AnsiColor::Cyan.into())
    };
    Style::new().fg_color(Some(ink)).bg_color(Some(fill)).bold()
}

pub fn heading(title: &str, meta: impl Display) {
    println!();
    println!("  {}  {}", paint(STRONG, title), paint(MUTED, meta));
}

pub fn field(label: &str, value: impl Display) {
    println!("  {}  {value}", paint(MUTED, format!("{label:<11}")));
}

pub fn detail(text: impl Display) {
    println!("{:DETAIL_INDENT$}{text}", "");
}

pub fn note(text: impl Display) {
    println!("  {}", paint(MUTED, text));
}

/// A verdict or warning, held apart from the numbers by a colored rule.
pub fn callout(style: Style, title: &str, body: &str, reasons: &[&str]) {
    let rule = paint(style, "┃");
    println!();
    println!("  {rule} {}  {body}", paint(style.bold(), title));
    for reason in reasons {
        println!("  {rule} {}", paint(MUTED, format!("· {reason}")));
    }
}

pub fn warning(message: &str) {
    eprintln!("{} {message}", paint(WARN.bold(), "warning:"));
}

pub fn failure(message: impl Display) {
    eprintln!("{} {message}", paint(BAD.bold(), "gigsift:"));
}

/// Right-aligned, with weight standing in for magnitude.
pub fn size(bytes: u64) -> String {
    let style = match bytes {
        1_000_000_000.. => STRONG,
        ..1_000_000 => MUTED,
        _ => Style::new(),
    };
    paint(style, format!("{:>9}", format_size(bytes)))
}

pub fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

pub fn bar(value: u64, max: u64, style: Style) -> String {
    if !*RICH {
        return String::new();
    }
    let (fill, empty) = bar_parts(value, max);
    format!("{}{}  ", paint(style, fill), " ".repeat(empty))
}

/// A bar that carries no verdict, only size, so it can wear the brand.
pub fn brand_bar(value: u64, max: u64) -> String {
    if !*RICH {
        return String::new();
    }
    let (fill, empty) = bar_parts(value, max);
    let fill: String = fill
        .chars()
        .enumerate()
        .map(|(cell, block)| paint(brand(cell as f32 / (BAR_CELLS - 1) as f32), block))
        .collect();
    format!("{fill}{}  ", " ".repeat(empty))
}

/// A meter also draws the part it has not reached, so the scale is visible.
pub fn meter(value: u64, max: u64, style: Style) -> String {
    if !*RICH {
        return String::new();
    }
    let (fill, empty) = bar_parts(value, max);
    let track = paint(MUTED, "░".repeat(empty));
    format!("{}{track}  ", paint(style, fill))
}

fn bar_parts(value: u64, max: u64) -> (String, usize) {
    let eighths = u128::from(value) * (BAR_CELLS as u128 * 8) / u128::from(max.max(1));
    // A sliver still shows, so nothing above zero reads as empty.
    let eighths = (eighths as usize).clamp(usize::from(value > 0), BAR_CELLS * 8);
    let (full, part) = (eighths / 8, eighths % 8);
    let used = full + usize::from(part > 0);
    ("█".repeat(full) + EIGHTHS[part], BAR_CELLS - used)
}

/// The path with home written as `~`.
pub fn home(path: &str) -> String {
    if *RICH {
        tilde(path, std::env::home_dir().as_deref())
    } else {
        path.to_owned()
    }
}

/// The path from `root`, for lists printed under a header naming it.
pub fn under(path: &str, root: &str) -> String {
    if !*RICH {
        return path.to_owned();
    }
    match Path::new(path).strip_prefix(root) {
        Ok(rest) if rest.as_os_str().is_empty() => ".".to_owned(),
        Ok(rest) => rest.display().to_string(),
        Err(_) => home(path),
    }
}

fn tilde(path: &str, home: Option<&Path>) -> String {
    let rest = home.and_then(|home| Path::new(path).strip_prefix(home).ok());
    match rest {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => Path::new("~").join(rest).display().to_string(),
        None => path.to_owned(),
    }
}

/// Dims everything above the last component, where the eye should land.
pub fn leaf(path: &str) -> String {
    let (parent, name) = path.split_at(leaf_start(path));
    format!("{}{name}", paint(MUTED, parent))
}

fn leaf_start(path: &str) -> usize {
    path.trim_end_matches(is_separator)
        .rfind(is_separator)
        .map_or(0, |index| index + 1)
}

/// Runs `task` with a spinner on stderr. Quiet for `--json` and when stderr is not
/// a terminal, so neither scripts nor logs ever see it.
pub fn working<T>(label: &str, json: bool, task: impl FnOnce() -> T) -> T {
    if json || !io::stderr().is_terminal() {
        return task();
    }
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        let spinner = scope.spawn(|| spin(label, &done));
        let _stop = StopSpinner {
            done: &done,
            spinner: spinner.thread(),
        };
        task()
    })
}

/// Stops the spinner on panics too, because the scope waits for it either way.
struct StopSpinner<'a> {
    done: &'a AtomicBool,
    spinner: &'a Thread,
}

impl Drop for StopSpinner<'_> {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        self.spinner.unpark();
    }
}

fn spin(label: &str, done: &AtomicBool) {
    let started = Instant::now();
    // Quick commands finish first and never flicker.
    thread::park_timeout(Duration::from_millis(150));
    if done.load(Ordering::Acquire) {
        return;
    }
    for (index, frame) in SPINNER.iter().enumerate().cycle() {
        if done.load(Ordering::Acquire) {
            break;
        }
        let seconds = started.elapsed().as_secs();
        eprint!(
            "\r  {} {label}  {}",
            paint(brand(index as f32 / SPINNER.len() as f32), frame),
            paint(MUTED, format!("{seconds}s"))
        );
        thread::park_timeout(Duration::from_millis(80));
    }
    // Spaces rather than an erase code, which NO_COLOR stripping would drop.
    eprint!("\r{:width$}\r", "", width = label.len() + 16);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_bar_fills_every_cell() {
        assert_eq!(bar_parts(10, 10), ("█".repeat(BAR_CELLS), 0));
    }

    #[test]
    fn a_half_bar_leaves_the_rest_empty() {
        assert_eq!(bar_parts(5, 10), ("█".repeat(BAR_CELLS / 2), BAR_CELLS / 2));
    }

    #[test]
    fn a_partial_cell_counts_toward_the_width() {
        let (fill, empty) = bar_parts(1, 40);
        assert_eq!(fill, "▌");
        assert_eq!(empty, BAR_CELLS - 1);
    }

    #[test]
    fn a_tiny_value_still_draws_a_sliver() {
        assert_eq!(bar_parts(1, u64::MAX), ("▏".to_owned(), BAR_CELLS - 1));
    }

    #[test]
    fn zero_draws_nothing_even_with_no_maximum() {
        assert_eq!(bar_parts(0, 0), (String::new(), BAR_CELLS));
    }

    #[test]
    fn a_value_past_the_maximum_stops_at_full() {
        assert_eq!(bar_parts(30, 10), ("█".repeat(BAR_CELLS), 0));
    }

    #[test]
    fn counts_group_thousands() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn home_becomes_a_tilde() {
        let home = Path::new("/Users/someone");
        let path = Path::new("~").join("Projects").display().to_string();
        assert_eq!(tilde("/Users/someone/Projects", Some(home)), path);
        assert_eq!(tilde("/Users/someone", Some(home)), "~");
    }

    #[test]
    fn a_sibling_of_home_keeps_its_full_path() {
        let home = Path::new("/Users/someone");
        assert_eq!(
            tilde("/Users/someone-else", Some(home)),
            "/Users/someone-else"
        );
        assert_eq!(tilde("/tmp/x", None), "/tmp/x");
    }

    #[test]
    fn the_leaf_is_the_last_component_even_with_a_trailing_slash() {
        assert_eq!(&"a/b/c"[leaf_start("a/b/c")..], "c");
        assert_eq!(&"a/b/c/"[leaf_start("a/b/c/")..], "c/");
        assert_eq!(&"c/"[leaf_start("c/")..], "c/");
        assert_eq!(&"."[leaf_start(".")..], ".");
    }
}
