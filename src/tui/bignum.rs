//! The header's big number, drawn like the logo: digits seven pixels tall,
//! two pixels to a cell through half blocks, with one layer of depth cast to
//! the lower right.

use ratatui::style::Color;
use ratatui::text::Line;

use crate::tui::wordmark::half_blocks;

/// Pixel rows in a digit.
const DIGIT_ROWS: usize = 7;

/// Empty pixel columns between two glyphs, so one's depth never touches the
/// next one's face.
const GAP: usize = 2;

/// Terminal rows the number takes, its depth included.
pub const HEIGHT: u16 = (DIGIT_ROWS as u16 + 1).div_ceil(2);

fn glyph(ch: char) -> Option<[&'static str; DIGIT_ROWS]> {
    Some(match ch {
        '0' => [".##.", "#..#", "#..#", "#..#", "#..#", "#..#", ".##."],
        '1' => [".#..", "##..", ".#..", ".#..", ".#..", ".#..", "###."],
        '2' => [".##.", "#..#", "...#", "..#.", ".#..", "#...", "####"],
        '3' => ["###.", "...#", "...#", ".##.", "...#", "...#", "###."],
        '4' => ["#..#", "#..#", "#..#", "####", "...#", "...#", "...#"],
        '5' => ["####", "#...", "#...", "###.", "...#", "...#", "###."],
        '6' => [".##.", "#...", "#...", "###.", "#..#", "#..#", ".##."],
        '7' => ["####", "...#", "..#.", "..#.", ".#..", ".#..", ".#.."],
        '8' => [".##.", "#..#", "#..#", ".##.", "#..#", "#..#", ".##."],
        '9' => [".##.", "#..#", "#..#", ".###", "...#", "...#", ".##."],
        '.' => [".", ".", ".", ".", ".", ".", "#"],
        _ => return None,
    })
}

/// The pixels of `text` set side by side, or `None` when it holds a
/// character there is no glyph for.
fn pixels(text: &str) -> Option<Vec<Vec<bool>>> {
    let mut rows = vec![Vec::new(); DIGIT_ROWS];
    for (index, ch) in text.chars().enumerate() {
        let glyph = glyph(ch)?;
        for (row, pattern) in rows.iter_mut().zip(glyph) {
            if index > 0 {
                row.extend([false; GAP]);
            }
            row.extend(pattern.bytes().map(|mark| mark == b'#'));
        }
    }
    Some(rows)
}

/// Columns `text` takes, depth included, or `None` when it cannot be drawn.
pub fn width(text: &str) -> Option<u16> {
    let rows = pixels(text)?;
    u16::try_from(rows[0].len() + 1).ok()
}

/// `text` in `face`, with its depth in `depth`, as [`HEIGHT`] lines, or
/// `None` when it holds a character there is no glyph for.
pub fn lines(text: &str, face: Color, depth: Color) -> Option<Vec<Line<'static>>> {
    let rows = pixels(text)?;
    let width = rows[0].len() + 1;
    let mut grid = vec![vec![None; width]; DIGIT_ROWS + 1];
    for (y, row) in rows.iter().enumerate() {
        for (x, &on) in row.iter().enumerate() {
            if on {
                grid[y + 1][x + 1] = Some(depth);
            }
        }
    }
    for (y, row) in rows.iter().enumerate() {
        for (x, &on) in row.iter().enumerate() {
            if on {
                grid[y][x] = Some(face);
            }
        }
    }
    Some(half_blocks(&grid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_is_as_big_as_it_says() {
        let face = Color::Rgb(1, 2, 3);
        let depth = Color::Rgb(4, 5, 6);
        let drawn = lines("56.0", face, depth).expect("digits");
        assert_eq!(drawn.len(), usize::from(HEIGHT));
        let width = usize::from(width("56.0").expect("width"));
        // Three digits of four, a point of one, three gaps of two, and depth.
        assert_eq!(width, 4 * 3 + 1 + 2 * 3 + 1);
        assert!(
            drawn
                .iter()
                .all(|line| line.to_string().chars().count() == width)
        );
    }

    #[test]
    fn a_character_without_a_glyph_is_refused() {
        assert!(width("4 GiB").is_none());
        assert!(lines("12%", Color::Reset, Color::Reset).is_none());
    }
}
