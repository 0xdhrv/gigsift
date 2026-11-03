//! The wordmark, and the short sift that draws it before `--help`.
//!
//! Grains fall out of a sieve and pile up into the letters. The last frame is the
//! finished wordmark, so clap prints the help over it without a flicker. The cursor
//! stays visible below the logo, which leaves nothing to restore after Ctrl-C.

use std::thread;
use std::time::Duration;

use anstream::stream::RawStream;
use anstream::{AutoStream, ColorChoice};
use anstyle::Style;

use crate::motion;
use crate::ui::{self, paint};

const ART: [&str; ROWS] = [
    "                        ▄",
    "█▀▀█ ▀ █▀▀█ █▀▀▀ ▀ █▀▀▀ █▀▀",
    "█  █ █ █  █ ▀▀▀█ █ █▀▀  █",
    "▀▀▀█ ▀ ▀▀▀█ ▀▀▀▀ ▀ ▀    ▀▀▀",
    "▀▀▀▀   ▀▀▀▀",
];
const ROWS: usize = 5;
const WIDTH: usize = 27;
/// Each terminal row holds two pixels, drawn with half blocks.
const HEIGHT: usize = ROWS * 2;

const FRAMES: usize = 19;
const FRAME_TIME: Duration = Duration::from_millis(30);
/// The sieve goes away once the last grains are through.
const SIEVE_FRAMES: usize = FRAMES - 3;
const SIEVE: &str = "┈";

/// Plain text when piped, because `anstream` strips the color.
pub fn wordmark() -> String {
    frame(FRAMES)
        .iter()
        .map(|row| row.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Plays on a color terminal only, so scripts and pagers never wait for it.
pub fn sift(stream: impl RawStream) {
    if motion::wanted() && stream.is_terminal() && AutoStream::choice(&stream) != ColorChoice::Never
    {
        play(stream);
    }
}

fn play(mut stream: impl RawStream) {
    let mut output = "\n".repeat(ROWS);
    for tick in 0..=FRAMES {
        output.push_str(&format!("\x1b[{ROWS}A{}\n", frame(tick).join("\n")));
        if stream.write_all(output.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
        output.clear();
        thread::sleep(FRAME_TIME);
    }
    let _ = write!(stream, "\x1b[{ROWS}A");
    let _ = stream.flush();
}

#[derive(Clone, Copy, PartialEq)]
enum Grain {
    Empty,
    Falling,
    Settled,
}

type Canvas = [[Grain; WIDTH]; HEIGHT];

fn frame(tick: usize) -> [String; ROWS] {
    let canvas = canvas(tick);
    std::array::from_fn(|row| {
        (0..WIDTH)
            .map(|x| cell(tick, row, x, canvas[row * 2][x], canvas[row * 2 + 1][x]))
            .collect()
    })
}

fn cell(tick: usize, row: usize, x: usize, top: Grain, bottom: Grain) -> String {
    let glyph = match (top != Grain::Empty, bottom != Grain::Empty) {
        (true, true) => "█",
        (true, false) => "▀",
        (false, true) => "▄",
        (false, false) if row == 0 && tick < SIEVE_FRAMES => {
            return paint(Style::new().dimmed(), SIEVE);
        }
        (false, false) => return " ".to_owned(),
    };
    let color = ui::brand(x as f32 / (WIDTH - 1) as f32);
    if top == Grain::Settled || bottom == Grain::Settled {
        paint(color, glyph)
    } else {
        paint(color.dimmed(), glyph)
    }
}

fn canvas(tick: usize) -> Canvas {
    let mut canvas = [[Grain::Empty; WIDTH]; HEIGHT];
    for (x, y) in pixels() {
        let landing = landing_tick(x, y);
        if tick >= landing {
            canvas[y][x] = Grain::Settled;
        } else if let Some(height) = falling_at(tick, landing, y)
            && canvas[height][x] == Grain::Empty
        {
            canvas[height][x] = Grain::Falling;
        }
    }
    canvas
}

fn pixels() -> impl Iterator<Item = (usize, usize)> {
    ART.iter().enumerate().flat_map(|(row, line)| {
        line.chars().enumerate().flat_map(move |(x, glyph)| {
            let top = matches!(glyph, '▀' | '█').then_some((x, row * 2));
            let bottom = matches!(glyph, '▄' | '█').then_some((x, row * 2 + 1));
            top.into_iter().chain(bottom)
        })
    })
}

/// The pile grows from the bottom, roughly evenly across the letters.
fn landing_tick(x: usize, y: usize) -> usize {
    let jitter = (x * 7 + y * 13 + x * y * 3) % 8;
    let from_bottom = HEIGHT - 1 - y;
    3 + from_bottom + jitter
}

/// Grains start at the sieve and speed up as they fall.
fn falling_at(tick: usize, landing: usize, y: usize) -> Option<usize> {
    let fall = (y as f32).sqrt() * 1.6;
    let progress = (tick as f32 - (landing as f32 - fall)) / fall;
    (0.0..1.0)
        .contains(&progress)
        .then_some((y as f32 * progress * progress) as usize)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn visible(row: &str) -> String {
        let mut text = Vec::new();
        anstream::StripStream::new(&mut text)
            .write_all(row.as_bytes())
            .unwrap();
        String::from_utf8(text).unwrap()
    }

    #[test]
    fn the_last_frame_is_the_art() {
        for (row, art) in frame(FRAMES).iter().zip(ART) {
            assert_eq!(visible(row).trim_end(), art);
        }
    }

    #[test]
    fn the_wordmark_is_the_last_frame_without_trailing_spaces() {
        let plain: Vec<String> = wordmark().lines().map(visible).collect();
        assert_eq!(plain, ART);
    }

    #[test]
    fn every_frame_is_the_same_size() {
        for tick in 0..=FRAMES {
            for row in frame(tick) {
                assert_eq!(visible(&row).chars().count(), WIDTH);
            }
        }
    }

    #[test]
    fn the_sift_starts_empty_and_every_grain_lands_in_time() {
        assert!(pixels().all(|(x, y)| landing_tick(x, y) > 0));
        assert!(canvas(0).iter().flatten().all(|&g| g != Grain::Settled));
        assert!(pixels().all(|(x, y)| canvas(FRAMES)[y][x] == Grain::Settled));
    }

    #[test]
    fn the_art_fits_its_width() {
        assert!(ART.iter().all(|row| row.chars().count() <= WIDTH));
    }
}
