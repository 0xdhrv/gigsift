//! Everything that moves. A row that cannot be redrawn safely is simply printed,
//! so motion never changes what a command says, only how it arrives.

use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use anstream::{eprint, print, println};
use anstyle::Style;

use crate::ui::{self, MUTED, paint};

const REVEAL_FRAMES: usize = 10;
const REVEAL_FRAME_TIME: Duration = Duration::from_millis(18);

/// Sand filling a cell and draining out again.
const SPINNER: [&str; 35] = [
    "⠁", "⠂", "⠄", "⡀", "⡈", "⡐", "⡠", "⣀", "⣁", "⣂", "⣄", "⣌", "⣔", "⣤", "⣥", "⣦", "⣮", "⣶", "⣷",
    "⣿", "⡿", "⠿", "⢟", "⠟", "⡛", "⠛", "⠫", "⢋", "⠋", "⠍", "⡉", "⠉", "⠑", "⠡", "⢁",
];
/// Letters this close to the moving highlight light up.
const SHIMMER_REACH: usize = 1;
/// How far the highlight travels past the label before it comes round again.
const SHIMMER_PAUSE: usize = 8;

/// Off where cursor movement can't be tested, and for anyone who opts out.
pub fn wanted() -> bool {
    !cfg!(windows) && env::var_os("CI").is_none() && env::var_os("GIGSIFT_NO_ANIMATION").is_none()
}

static REDRAW: LazyLock<bool> = LazyLock::new(|| wanted() && ui::colored());

/// Prints `rows(1.0)`. On a terminal it first plays `rows` from 0 up to 1, so bars
/// grow and figures count up. `rows` has to return the same number of rows each time.
pub fn reveal(rows: impl Fn(f32) -> Vec<String>) {
    let finished = rows(1.0);
    if !*REDRAW || !fits_on_screen(&finished) {
        for row in finished {
            println!("{row}");
        }
        return;
    }
    let count = finished.len();
    for frame in 0..=REVEAL_FRAMES {
        if frame > 0 {
            print!("\x1b[{count}A");
        }
        for row in rows(ease_out(frame as f32 / REVEAL_FRAMES as f32)) {
            print!("{row}\x1b[K\n");
        }
        let _ = io::stdout().flush();
        if frame < REVEAL_FRAMES {
            thread::sleep(REVEAL_FRAME_TIME);
        }
    }
}

/// A row that wraps or scrolls away would put the cursor somewhere unexpected.
fn fits_on_screen(rows: &[String]) -> bool {
    let (columns, lines) = ui::window();
    rows.len() < lines && rows.iter().all(|row| ui::visible_width(row) < columns)
}

fn ease_out(time: f32) -> f32 {
    1.0 - (1.0 - time).powi(3)
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
    let moving = wanted();
    for (tick, frame) in (0..).zip(SPINNER.iter().cycle()) {
        if done.load(Ordering::Acquire) {
            break;
        }
        let seconds = started.elapsed().as_secs();
        let text = if moving {
            shimmer(label, tick)
        } else {
            label.to_owned()
        };
        eprint!(
            "\r  {} {text}  {}",
            paint(ui::brand(ramp_at(tick)), frame),
            paint(MUTED, format!("{seconds}s"))
        );
        thread::park_timeout(Duration::from_millis(80));
    }
    // Spaces rather than an erase code, which NO_COLOR stripping would drop.
    eprint!("\r{:width$}\r", "", width = label.len() + 16);
}

fn ramp_at(tick: usize) -> f32 {
    let at = tick % SPINNER.len();
    at as f32 / SPINNER.len() as f32
}

/// A highlight sweeping across the label, which works in plain ANSI too because
/// the letters around it only dim.
fn shimmer(label: &str, tick: usize) -> String {
    let length = label.chars().count();
    let head = tick % (length + SHIMMER_PAUSE);
    label
        .chars()
        .enumerate()
        .map(|(index, letter)| paint(shimmer_style(index, head, length), letter))
        .collect()
}

fn shimmer_style(index: usize, head: usize, length: usize) -> Style {
    if index.abs_diff(head) <= SHIMMER_REACH {
        ui::brand(index as f32 / length.max(1) as f32).bold()
    } else {
        Style::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> String {
        anstream::adapter::strip_str(text).to_string()
    }

    #[test]
    fn easing_covers_the_whole_range() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5);
    }

    #[test]
    fn the_shimmer_never_changes_the_label() {
        for tick in 0..40 {
            assert_eq!(plain(&shimmer("Scanning", tick)), "Scanning");
        }
    }

    #[test]
    fn the_shimmer_handles_an_empty_label() {
        assert_eq!(shimmer("", 3), "");
    }

    #[test]
    fn the_spinner_color_stays_on_the_ramp() {
        assert!((0..100).map(ramp_at).all(|at| (0.0..1.0).contains(&at)));
    }
}
