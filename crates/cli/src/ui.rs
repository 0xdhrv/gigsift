//! Terminal styling. Commands name a role here rather than picking colors, and
//! every role is a plain ANSI color, so text follows the user's terminal theme on
//! light and dark backgrounds alike. Graphs and brand marks go further with mid-tone
//! colors that read on both, stepping down to 256 colors and then to the ANSI roles.
//!
//! `anstream` drops the styling for pipes and `NO_COLOR`. Graphs, shortened paths and
//! cards need more care: they only appear on a terminal, so piped text stays plain.
//! What a terminal can draw beyond that, links and glyphs, comes from `terminal`.

use std::env;
use std::fmt::Display;
use std::io::{self, IsTerminal};
use std::path::{Path, is_separator};
use std::sync::LazyLock;

use anstream::{AutoStream, ColorChoice, eprintln, println};
use anstyle::{Ansi256Color, AnsiColor, Color, RgbColor, Style};
use gigsift_core::size::format_size;
use terminal_size::{Height, Width, terminal_size};

use crate::{motion, terminal};

pub const ACCENT: Style = AnsiColor::Cyan.on_default();
pub const GOOD: Style = AnsiColor::Green.on_default();
pub const WARN: Style = AnsiColor::Yellow.on_default();
pub const BAD: Style = AnsiColor::Red.on_default();
pub const MUTED: Style = Style::new().dimmed();
pub const STRONG: Style = Style::new().bold();

/// Column where a row's details start, under its size.
pub const DETAIL_INDENT: usize = 13;

/// Graphs stop growing here, so they stay readable on a wide window.
const MAX_WIDTH: usize = 78;
const DEFAULT_WINDOW: (usize, usize) = (80, 24);

/// Cool for small, hot for large. The logo uses the cool end.
const HEAT: [RgbColor; 5] = [
    RgbColor(0x10, 0xB5, 0xA0),
    RgbColor(0x12, 0x9C, 0xD8),
    RgbColor(0x52, 0x74, 0xF0),
    RgbColor(0xA3, 0x5C, 0xE6),
    RgbColor(0xE0, 0x50, 0x9E),
];
const BRAND_STOPS: usize = 3;

/// Told apart by hue, for parts of one whole.
const SERIES: [(RgbColor, AnsiColor); 6] = [
    (RgbColor(0x10, 0xB5, 0xA0), AnsiColor::Cyan),
    (RgbColor(0x3A, 0x8F, 0xF0), AnsiColor::Blue),
    (RgbColor(0xA3, 0x5C, 0xE6), AnsiColor::Magenta),
    (RgbColor(0xE0, 0x50, 0x9E), AnsiColor::BrightMagenta),
    (RgbColor(0xE5, 0xA0, 0x24), AnsiColor::BrightCyan),
    (RgbColor(0x5D, 0xB8, 0x4A), AnsiColor::BrightBlue),
];

/// A stacked bar shows this many parts, one per color.
pub const SERIES_COUNT: usize = SERIES.len();

const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
/// Consolas has no narrow blocks, so every partial cell is half full.
const LEGACY_EIGHTHS: [&str; 8] = ["", "▌", "▌", "▌", "▌", "▌", "▌", "▌"];

#[derive(Clone, Copy, PartialEq)]
enum Depth {
    Ansi,
    Palette,
    Truecolor,
}

static RICH: LazyLock<bool> = LazyLock::new(|| io::stdout().is_terminal());
static DEPTH: LazyLock<Depth> = LazyLock::new(|| {
    let term = env::var("TERM").unwrap_or_default();
    match env::var_os("COLORTERM") {
        Some(value) if value == "truecolor" || value == "24bit" => Depth::Truecolor,
        _ if terminal::current().truecolor() => Depth::Truecolor,
        _ if term.contains("256color") => Depth::Palette,
        _ => Depth::Ansi,
    }
});
/// A link names a file on this machine, so it would open the wrong one over SSH.
static LINKS: LazyLock<bool> = LazyLock::new(|| {
    colored()
        && terminal::current().links()
        && env::var_os("SSH_CONNECTION").is_none()
        && env::var_os("SSH_TTY").is_none()
});
static WINDOW: LazyLock<(usize, usize)> = LazyLock::new(|| {
    terminal_size().map_or(DEFAULT_WINDOW, |(Width(columns), Height(lines))| {
        (usize::from(columns), usize::from(lines))
    })
});

/// Whether output is going to a terminal someone is reading.
pub fn rich() -> bool {
    *RICH
}

/// Whether color reaches the reader. Parts of a stacked bar are told apart by it.
pub fn colored() -> bool {
    *RICH && AutoStream::choice(&io::stdout()) != ColorChoice::Never
}

/// Columns and lines.
pub fn window() -> (usize, usize) {
    *WINDOW
}

/// Styles after any padding, because escape codes would count toward the width.
pub fn paint(style: Style, text: impl Display) -> String {
    format!("{style}{text}{style:#}")
}

/// `full`, or `legacy` where the font has no such glyph.
pub fn glyph(full: &'static str, legacy: &'static str) -> &'static str {
    if terminal::full_glyphs() {
        full
    } else {
        legacy
    }
}

/// `text` that opens `path` when clicked, in terminals known to support links.
/// Elsewhere it stays plain text, and `anstream` drops the link for pipes.
pub fn link(path: &str, text: impl Display) -> String {
    if *LINKS {
        format!(
            "\x1b]8;;{}\x1b\\{text}\x1b]8;;\x1b\\",
            file_url(Path::new(path))
        )
    } else {
        text.to_string()
    }
}

fn file_url(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let path = absolute.to_string_lossy().replace('\\', "/");
    // A Windows path starts with its drive letter.
    let mut url = String::from(if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'.' | b'_' | b'~' => {
                url.push(char::from(byte));
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// Rough on purpose: anything that could be double width counts as double, so a
/// row is never taken to fit when it does not. Only drawing characters count as one.
pub fn visible_width(text: &str) -> usize {
    anstream::adapter::strip_str(text)
        .to_string()
        .chars()
        .map(|glyph| match glyph {
            '\u{0}'..'\u{1100}' | '\u{2000}'..'\u{2E80}' => 1,
            _ => 2,
        })
        .sum()
}

/// A point on the logo's ramp, from 0 to 1.
pub fn brand(at: f32) -> Style {
    ramp_style(&HEAT[..BRAND_STOPS], at)
}

/// A point on the size ramp, from 0 to 1.
pub fn heat(at: f32) -> Style {
    ramp_style(&HEAT, at)
}

fn ramp_style(stops: &[RgbColor], at: f32) -> Style {
    if *DEPTH == Depth::Ansi {
        ACCENT
    } else {
        Style::new().fg_color(Some(ink(ramp(stops, at))))
    }
}

/// One of several parts of a whole.
pub fn series(index: usize) -> Style {
    let (color, plain) = SERIES[index % SERIES.len()];
    let color = if *DEPTH == Depth::Ansi {
        plain.into()
    } else {
        ink(color)
    };
    Style::new().fg_color(Some(color))
}

fn ink(color: RgbColor) -> Color {
    if *DEPTH == Depth::Truecolor {
        color.into()
    } else {
        nearest_in_palette(color).into()
    }
}

/// The 256-color palette holds a 6 by 6 by 6 cube from index 16.
fn nearest_in_palette(RgbColor(red, green, blue): RgbColor) -> Ansi256Color {
    let step = |channel: u8| match channel {
        ..48 => 0,
        48..115 => 1,
        _ => (channel - 35) / 40,
    };
    Ansi256Color(16 + 36 * step(red) + 6 * step(green) + step(blue))
}

fn ramp(stops: &[RgbColor], at: f32) -> RgbColor {
    let spans = (stops.len() - 1) as f32;
    let position = at.clamp(0.0, 1.0) * spans;
    let span = (position as usize).min(stops.len() - 2);
    let (from, to) = (stops[span], stops[span + 1]);
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
    let line = format!(
        "  {}  {}",
        paint(badge(), format!(" {} {command} ", glyph("◆", "♦"))),
        leaf(subject)
    );
    println!("{line}{}", rule(&line));
    println!();
}

fn badge() -> Style {
    let (text, fill): (Color, Color) = if *DEPTH == Depth::Ansi {
        (AnsiColor::Black.into(), AnsiColor::Cyan.into())
    } else {
        (
            ink(RgbColor(0x04, 0x18, 0x1C)),
            ink(ramp(&HEAT[..BRAND_STOPS], 0.25)),
        )
    };
    Style::new()
        .fg_color(Some(text))
        .bg_color(Some(fill))
        .bold()
}

pub fn heading(title: &str, meta: impl Display) {
    let line = format!("  {}  {}", paint(STRONG, title), paint(MUTED, meta));
    println!();
    println!("{line}{}", rule(&line));
}

/// Fills what is left of a line, up to the width graphs use.
fn rule(line: &str) -> String {
    let width = window().0.min(MAX_WIDTH);
    match width.checked_sub(visible_width(line.trim_end()) + 1) {
        Some(length) if *RICH && length > 0 => paint(MUTED, format!(" {}", "─".repeat(length))),
        _ => String::new(),
    }
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
    let rule = paint(style, glyph("┃", "│"));
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

/// Right-aligned. Color stands in for magnitude where there's depth for it, and
/// weight where there isn't.
pub fn size(bytes: u64) -> String {
    let style = match bytes {
        ..1_000_000 => MUTED,
        _ if *DEPTH != Depth::Ansi => heat(magnitude(bytes)).bold(),
        1_000_000_000.. => STRONG,
        _ => Style::new(),
    };
    paint(style, format!("{:>9}", format_size(bytes)))
}

/// From 1 MB at the cool end to 1 TB at the hot end.
fn magnitude(bytes: u64) -> f32 {
    ((bytes as f32).log10() - 6.0) / 6.0
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

/// Shrinks in a narrow window to leave the path room.
fn bar_cells() -> usize {
    match window().0 {
        ..70 => 12,
        70..100 => 20,
        _ => 28,
    }
}

/// The share of `value` to draw partway through a reveal.
pub fn scaled(value: u64, progress: f32) -> u64 {
    (value as f64 * f64::from(progress)) as u64
}

/// A bar that carries no verdict, only size, so it runs up the size ramp.
pub fn heat_bar(value: u64, max: u64) -> String {
    if !*RICH {
        return String::new();
    }
    let cells = bar_cells();
    let (fill, empty) = bar_parts(value, max, cells);
    let fill: String = fill
        .chars()
        .enumerate()
        .map(|(cell, block)| paint(heat(cell as f32 / (cells - 1) as f32), block))
        .collect();
    format!("{fill}{}  ", " ".repeat(empty))
}

/// Grows out from a center line: shrinkage to the left, growth to the right.
pub fn diverging(value: u64, shrinking: bool, max: u64, style: Style) -> String {
    if !*RICH {
        return String::new();
    }
    let side = bar_cells() / 2;
    let (fill, empty) = bar_parts(value, max, side);
    let axis = paint(MUTED, "│");
    if shrinking {
        // No partial block hugs the right edge of a cell except the half.
        let whole = fill.matches('█').count();
        let part = if fill.chars().count() > whole {
            "▐"
        } else {
            ""
        };
        let bar = paint(style, format!("{part}{}", "█".repeat(whole)));
        format!("{}{bar}{axis}{}  ", " ".repeat(empty), " ".repeat(side))
    } else {
        let bar = paint(style, fill);
        format!("{}{axis}{bar}{}  ", " ".repeat(side), " ".repeat(empty))
    }
}

/// A meter also draws the part it has not reached, so the scale is visible.
pub fn meter(value: u64, max: u64, style: Style) -> String {
    if !*RICH {
        return String::new();
    }
    let (fill, empty) = bar_parts(value, max, bar_cells());
    let track = paint(MUTED, "░".repeat(empty));
    format!("{}{track}  ", paint(style, fill))
}

/// `value` as a share of `total`, or nothing without a total to compare with.
pub fn share(value: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if *RICH && total > 0 => {
            let percent = u128::from(value) * 100 / u128::from(total);
            paint(MUTED, format!("{percent:>3}%  "))
        }
        _ => String::new(),
    }
}

/// Marks a row as one of the parts of a `stacked` bar.
pub fn swatch(part: Option<usize>) -> String {
    match part {
        _ if !*RICH => String::new(),
        Some(index) => format!("{} ", paint(series(index), "■")),
        None => "  ".to_owned(),
    }
}

/// One bar split into `parts`, with any remainder of `total` as a faint track.
pub fn stacked(parts: &[u64], total: u64, progress: f32) -> String {
    let width = window().0.min(MAX_WIDTH).saturating_sub(4);
    let mut bar = String::new();
    let mut drawn = 0;
    let mut sum = 0;
    for (index, part) in parts.iter().enumerate() {
        sum += scaled(*part, progress);
        let edge = (u128::from(sum) * width as u128 / u128::from(total.max(1))) as usize;
        let edge = edge.min(width);
        bar.push_str(&paint(series(index), "█".repeat(edge - drawn)));
        drawn = edge;
    }
    format!("  {bar}{}", paint(MUTED, "░".repeat(width - drawn)))
}

/// Names the parts of a `stacked` bar, wrapped to fit the window.
pub fn legend(parts: &[(&str, u64)]) -> Vec<String> {
    let width = window().0.min(MAX_WIDTH);
    let mut rows = vec![String::from(" ")];
    for (index, (label, bytes)) in parts.iter().enumerate() {
        let entry = format!(
            " {} {label} {}",
            paint(series(index), "■"),
            paint(MUTED, format_size(*bytes))
        );
        let used = rows.last().map_or(0, |row| visible_width(row));
        if used > 1 && used + visible_width(&entry) >= width {
            rows.push(String::from(" "));
        }
        if let Some(row) = rows.last_mut() {
            row.push_str(&entry);
            row.push(' ');
        }
    }
    rows
}

fn bar_parts(value: u64, max: u64, cells: usize) -> (String, usize) {
    let eighths = u128::from(value) * (cells as u128 * 8) / u128::from(max.max(1));
    // A sliver still shows, so nothing above zero reads as empty.
    let eighths = (eighths as usize).clamp(usize::from(value > 0), cells * 8);
    let (full, part) = (eighths / 8, eighths % 8);
    let used = full + usize::from(part > 0);
    ("█".repeat(full) + partial_cells()[part], cells - used)
}

fn partial_cells() -> [&'static str; 8] {
    if terminal::full_glyphs() {
        EIGHTHS
    } else {
        LEGACY_EIGHTHS
    }
}

/// One number on a card. Sizes and counts roll up to their value on a terminal.
pub enum Figure {
    Bytes(u64),
    Count(u64),
    Text(String),
}

impl Figure {
    fn at(&self, progress: f32) -> String {
        match self {
            Self::Bytes(bytes) => format_size(scaled(*bytes, progress)),
            Self::Count(value) => count(scaled(*value, progress)),
            Self::Text(text) => text.clone(),
        }
    }
}

pub struct Stat {
    label: &'static str,
    figure: Figure,
    style: Style,
}

impl Stat {
    pub fn new(label: &'static str, figure: Figure) -> Self {
        Self {
            label,
            figure,
            style: STRONG,
        }
    }

    pub fn styled(self, style: Style) -> Self {
        Self { style, ..self }
    }

    /// Room for the label, and for the widest size on the way up to the value.
    fn width(&self) -> usize {
        const MIN_WIDTH: usize = 11;
        let figure = self.figure.at(1.0).chars().count() + 2;
        figure.max(self.label.len() + 3).max(MIN_WIDTH)
    }
}

/// A row of cards, or plain fields if they won't fit or nobody is looking.
pub fn stats(stats: &[Stat]) {
    let widths: Vec<usize> = stats.iter().map(Stat::width).collect();
    let total: usize = 2 + widths.iter().map(|width| width + 3).sum::<usize>();
    if !*RICH || total > window().0 {
        for stat in stats {
            field(stat.label, paint(stat.style, stat.figure.at(1.0)));
        }
        return;
    }
    motion::reveal(|progress| cards(stats, &widths, progress));
}

fn cards(stats: &[Stat], widths: &[usize], progress: f32) -> Vec<String> {
    let border = |text: String| paint(MUTED, text);
    // Consolas has no rounded corners.
    let [top_left, top_right, bottom_left, bottom_right] = if terminal::full_glyphs() {
        ["╭", "╮", "╰", "╯"]
    } else {
        ["┌", "┐", "└", "┘"]
    };
    let mut rows = [String::from("  "), String::from("  "), String::from("  ")];
    for (stat, width) in stats.iter().zip(widths) {
        let line = width - stat.label.len() - 3;
        rows[0].push_str(&format!(
            "{}{}{} ",
            border(format!("{top_left}─ ")),
            paint(MUTED, stat.label),
            border(format!(" {}{top_right}", "─".repeat(line))),
        ));
        rows[1].push_str(&format!(
            "{} {} {} ",
            border("│".to_owned()),
            paint(
                stat.style,
                format!("{:<1$}", stat.figure.at(progress), width - 2)
            ),
            border("│".to_owned()),
        ));
        rows[2].push_str(&format!(
            "{} ",
            border(format!("{bottom_left}{}{bottom_right}", "─".repeat(*width)))
        ));
    }
    rows.into_iter()
        .map(|row| row.trim_end().to_owned())
        .collect()
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
    if *RICH && name.ends_with(is_separator) {
        format!("{}{}", paint(MUTED, parent), paint(brand(0.6), name))
    } else {
        format!("{}{name}", paint(MUTED, parent))
    }
}

fn leaf_start(path: &str) -> usize {
    path.trim_end_matches(is_separator)
        .rfind(is_separator)
        .map_or(0, |index| index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELLS: usize = 20;

    #[test]
    fn a_full_bar_fills_every_cell() {
        assert_eq!(bar_parts(10, 10, CELLS), ("█".repeat(CELLS), 0));
    }

    #[test]
    fn a_half_bar_leaves_the_rest_empty() {
        assert_eq!(bar_parts(5, 10, CELLS), ("█".repeat(CELLS / 2), CELLS / 2));
    }

    #[test]
    fn a_partial_cell_counts_toward_the_width() {
        let (fill, empty) = bar_parts(1, 40, CELLS);
        assert_eq!(fill, "▌");
        assert_eq!(empty, CELLS - 1);
    }

    #[test]
    fn a_tiny_value_still_draws_a_sliver() {
        let sliver = partial_cells()[1].to_owned();
        assert_eq!(bar_parts(1, u64::MAX, CELLS), (sliver, CELLS - 1));
    }

    #[test]
    fn zero_draws_nothing_even_with_no_maximum() {
        assert_eq!(bar_parts(0, 0, CELLS), (String::new(), CELLS));
    }

    #[test]
    fn a_value_past_the_maximum_stops_at_full() {
        assert_eq!(bar_parts(30, 10, CELLS), ("█".repeat(CELLS), 0));
    }

    #[test]
    fn palette_colors_land_on_the_cube() {
        assert_eq!(nearest_in_palette(RgbColor(0, 0, 0)), Ansi256Color(16));
        assert_eq!(
            nearest_in_palette(RgbColor(255, 255, 255)),
            Ansi256Color(231)
        );
        assert_eq!(nearest_in_palette(RgbColor(255, 0, 0)), Ansi256Color(196));
    }

    #[test]
    fn the_ramp_starts_and_ends_on_its_stops() {
        assert_eq!(ramp(&HEAT, 0.0), HEAT[0]);
        assert_eq!(ramp(&HEAT, 1.0), HEAT[4]);
        assert_eq!(ramp(&HEAT, 2.0), HEAT[4]);
    }

    #[test]
    fn styling_does_not_count_toward_width() {
        assert_eq!(visible_width(&paint(heat(0.5), "█ abc")), 5);
    }

    #[test]
    fn the_fallback_bar_is_all_in_the_console_font() {
        let glyphs = LEGACY_EIGHTHS.concat();
        assert!(
            glyphs
                .chars()
                .all(|g| terminal::CONSOLAS_BLOCKS.contains(g))
        );
    }

    #[test]
    fn a_link_does_not_count_toward_width() {
        let url = file_url(Path::new("/tmp"));
        assert_eq!(
            visible_width(&format!("\x1b]8;;{url}\x1b\\name\x1b]8;;\x1b\\")),
            4
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_url_escapes_what_a_url_cannot_hold() {
        assert_eq!(
            file_url(Path::new("/tmp/a b/ü#1")),
            "file:///tmp/a%20b/%C3%BC%231"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_file_url_starts_with_its_drive() {
        assert_eq!(
            file_url(Path::new(r"C:\Users\a b")),
            "file:///C:/Users/a%20b"
        );
    }

    #[test]
    fn wide_text_counts_double_so_it_never_seems_to_fit() {
        assert_eq!(visible_width("日本"), 4);
    }

    #[test]
    fn counting_up_stays_within_the_card() {
        let stat = Stat::new("On disk", Figure::Bytes(1_000_000_000));
        let room = stat.width() - 2;
        for step in 0..=100 {
            let text = stat.figure.at(step as f32 / 100.0);
            assert!(text.chars().count() <= room, "{text}");
        }
    }

    #[test]
    fn cards_line_up_top_to_bottom() {
        let stats = [
            Stat::new("On disk", Figure::Bytes(12_300_000_000)),
            Stat::new("Files", Figure::Count(1_234_567)),
            Stat::new("Since", Figure::Text("2026-11-03 09:30".to_owned())),
        ];
        let widths: Vec<usize> = stats.iter().map(Stat::width).collect();
        let rows = cards(&stats, &widths, 1.0);
        let lengths: Vec<usize> = rows.iter().map(|row| visible_width(row)).collect();
        assert_eq!(lengths[0], lengths[1]);
        assert_eq!(lengths[1], lengths[2]);
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
