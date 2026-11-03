//! An area chart built from block characters, which stay solid in any terminal font
//! and give each cell eight levels of height.

use crate::{terminal, ui};

const LEVELS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
/// Consolas only has the half block, so cells fill in halves.
const LEGACY_LEVELS: [&str; 9] = [" ", " ", " ", "▄", "▄", "▄", "▄", "█", "█"];

/// A point from 0 to 1 on each axis, with `y` growing upward.
pub type Point = (f32, f32);

struct Column {
    level: f32,
    projected: bool,
}

/// `history` as solid columns, then a faint `projection` from where it ends. Columns
/// past `reach`, a share of the width, stay empty so the chart can sweep in.
pub fn area(
    history: &[Point],
    projection: Option<Point>,
    width: usize,
    height: usize,
    reach: f32,
) -> Vec<String> {
    let known = history.last().map_or(0.0, |point| point.0);
    let line: Vec<Point> = history.iter().copied().chain(projection).collect();
    let columns: Vec<Option<Column>> = (0..width)
        .map(|column| {
            let x = (column as f32 + 0.5) / width as f32;
            let level = level_at(&line, x).filter(|_| x <= reach)?;
            Some(Column {
                level,
                projected: x > known,
            })
        })
        .collect();
    (0..height)
        .map(|row| {
            let from_bottom = height - 1 - row;
            columns
                .iter()
                .map(|column| cell(column.as_ref(), from_bottom, height))
                .collect()
        })
        .collect()
}

/// Reads the line at `x`, which runs left to right.
fn level_at(line: &[Point], x: f32) -> Option<f32> {
    let segment = line
        .windows(2)
        .find(|pair| (pair[0].0..=pair[1].0).contains(&x))?;
    let (from, to) = (segment[0], segment[1]);
    let run = to.0 - from.0;
    let along = if run > 0.0 { (x - from.0) / run } else { 0.0 };
    Some(from.1 + (to.1 - from.1) * along)
}

/// Hotter toward the top, where the disk is fuller.
fn cell(column: Option<&Column>, from_bottom: usize, height: usize) -> String {
    let Some(column) = column else {
        return " ".to_owned();
    };
    let filled =
        (column.level.clamp(0.0, 1.0) * height as f32 - from_bottom as f32).clamp(0.0, 1.0);
    let levels = if terminal::full_glyphs() {
        LEVELS
    } else {
        LEGACY_LEVELS
    };
    let glyph = levels[(filled * 8.0).round() as usize];
    let style = ui::heat((from_bottom + 1) as f32 / height as f32);
    let style = if column.projected {
        style.dimmed()
    } else {
        style
    };
    ui::paint(style, glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| anstream::adapter::strip_str(row).to_string())
            .collect()
    }

    fn filled(rows: &[String]) -> usize {
        plain(rows)
            .concat()
            .chars()
            .filter(|glyph| *glyph != ' ')
            .count()
    }

    #[test]
    fn the_fallback_levels_are_all_in_the_console_font() {
        let glyphs = LEGACY_LEVELS.concat();
        assert!(
            glyphs
                .chars()
                .all(|g| terminal::CONSOLAS_BLOCKS.contains(g))
        );
    }

    #[test]
    fn it_returns_one_row_per_height_at_full_width() {
        let rows = plain(&area(&[(0.0, 0.5), (1.0, 0.5)], None, 10, 4, 1.0));
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|row| row.chars().count() == 10));
    }

    #[test]
    fn a_rising_line_is_short_on_the_left_and_tall_on_the_right() {
        let rows = plain(&area(&[(0.0, 0.0), (1.0, 1.0)], None, 10, 3, 1.0));
        assert_eq!(rows[0].chars().next(), Some(' '));
        assert_ne!(rows[0].chars().last(), Some(' '));
        assert_eq!(rows[2].chars().last(), Some('█'));
    }

    #[test]
    fn a_full_column_is_solid_and_an_empty_one_is_blank() {
        let rows = plain(&area(&[(0.0, 1.0), (1.0, 1.0)], None, 4, 2, 1.0));
        assert_eq!(rows, ["████", "████"]);
        assert_eq!(filled(&area(&[(0.0, 0.0), (1.0, 0.0)], None, 4, 2, 1.0)), 0);
    }

    #[test]
    fn a_level_between_rows_uses_a_partial_block() {
        let rows = plain(&area(&[(0.0, 0.25), (1.0, 0.25)], None, 2, 2, 1.0));
        assert_eq!(rows, ["  ", "▄▄"]);
    }

    #[test]
    fn a_single_point_draws_nothing_and_does_not_panic() {
        assert_eq!(filled(&area(&[(0.5, 0.5)], None, 10, 3, 1.0)), 0);
    }

    #[test]
    fn levels_outside_the_chart_stay_on_it() {
        assert!(filled(&area(&[(0.0, 2.0), (1.0, -1.0)], None, 6, 2, 1.0)) > 0);
    }

    #[test]
    fn reach_holds_the_columns_back() {
        let line = [(0.0, 1.0), (1.0, 1.0)];
        assert_eq!(filled(&area(&line, None, 20, 1, 0.5)), 10);
        assert_eq!(filled(&area(&line, None, 20, 1, 0.0)), 0);
    }

    #[test]
    fn the_projection_carries_the_line_past_the_history() {
        let rows = plain(&area(
            &[(0.0, 1.0), (0.5, 1.0)],
            Some((1.0, 1.0)),
            4,
            1,
            1.0,
        ));
        assert_eq!(rows, ["████"]);
    }
}
