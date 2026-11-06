//! The dashboard's logo: "déjàview" in lowercase pixel letters, two square
//! pixels to a terminal cell through half blocks, with two layers of depth
//! cast to the lower right. The letters shade from mint at the top to deep
//! teal at the foot. The accents are the one warm colour, and a mirrored
//! pair: the acute over the e and the grave over the a, with the j's dot
//! centred between them.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::tui::theme::Theme;

/// Pixel rows in a letter. Accents and dots take the top two, the x-height
/// rows 3 to 7, and the j's tail the last two.
const LETTER_ROWS: usize = 10;

/// Layers of depth below and to the right of each pixel.
const DEPTH: usize = 2;

/// Pixel rows in the logo, two to a terminal row.
const PIXEL_ROWS: usize = LETTER_ROWS + DEPTH;

/// Terminal rows the logo takes.
pub const HEIGHT: u16 = (PIXEL_ROWS / 2) as u16;

/// Terminal columns the logo takes, its depth included.
pub const WIDTH: u16 = 39;

/// A letter's pixels, top row first: `#` is ink, `*` an accent, `.` empty.
/// `kern` moves the letter that many columns left of where it would stand,
/// so the j's tail can tuck under the e.
struct Glyph {
    rows: [&'static str; LETTER_ROWS],
    kern: usize,
}

const fn glyph(rows: [&'static str; LETTER_ROWS]) -> Glyph {
    Glyph { rows, kern: 0 }
}

const D: Glyph = glyph([
    "...#", "...#", "...#", ".###", "#..#", "#..#", "#..#", ".###", "....", "....",
]);
const E_ACUTE: Glyph = glyph([
    "..*.", ".*..", "....", ".##.", "#..#", "####", "#...", ".###", "....", "....",
]);
const J: Glyph = Glyph {
    rows: [
        "...", "..#", "...", "..#", "..#", "..#", "..#", "..#", "..#", "##.",
    ],
    kern: 2,
};
const A_GRAVE: Glyph = glyph([
    ".*..", "..*.", "....", ".##.", "...#", ".###", "#..#", ".###", "....", "....",
]);
const V: Glyph = glyph([
    ".....", ".....", ".....", "#...#", "#...#", "#...#", ".#.#.", "..#..", ".....", ".....",
]);
const I: Glyph = glyph([".", "#", ".", "#", "#", "#", "#", "#", ".", "."]);
const E: Glyph = glyph([
    "....", "....", "....", ".##.", "#..#", "####", "#...", ".###", "....", "....",
]);
/// Two v's that share their middle stroke.
const W: Glyph = glyph([
    ".......", ".......", ".......", "#.....#", "#..#..#", "#..#..#", ".#.#.#.", "..#.#..",
    ".......", ".......",
]);

const LETTERS: [&Glyph; 8] = [&D, &E_ACUTE, &J, &A_GRAVE, &V, &I, &E, &W];

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Pixel {
    Empty,
    Ink,
    Accent,
}

/// The letters set side by side, one empty column between them less any
/// kerning, as rows of pixels.
fn letter_pixels() -> [Vec<Pixel>; LETTER_ROWS] {
    let mut rows: [Vec<Pixel>; LETTER_ROWS] = std::array::from_fn(|_| Vec::new());
    let mut x = 0usize;
    for (index, letter) in LETTERS.iter().enumerate() {
        if index > 0 {
            x = (x + 1).saturating_sub(letter.kern);
        }
        let width = letter.rows[0].len();
        for (row, pattern) in rows.iter_mut().zip(letter.rows) {
            if row.len() < x + width {
                row.resize(x + width, Pixel::Empty);
            }
            for (column, mark) in pattern.bytes().enumerate() {
                let pixel = match mark {
                    b'#' => Pixel::Ink,
                    b'*' => Pixel::Accent,
                    _ => continue,
                };
                row[x + column] = pixel;
            }
        }
        x += width;
    }
    rows
}

/// Every pixel's colour: the depth layers first, the farthest underneath,
/// then the letters over them.
fn colours() -> Vec<Vec<Option<Color>>> {
    let letters = letter_pixels();
    let width = letters[0].len() + DEPTH;
    let mut grid = vec![vec![None; width]; PIXEL_ROWS];
    for layer in (1..=DEPTH).rev() {
        let colour = Theme::wordmark_depth(layer).fg;
        for (y, row) in letters.iter().enumerate() {
            for (x, pixel) in row.iter().enumerate() {
                if *pixel != Pixel::Empty {
                    grid[y + layer][x + layer] = colour;
                }
            }
        }
    }
    for (y, row) in letters.iter().enumerate() {
        for (x, pixel) in row.iter().enumerate() {
            match pixel {
                Pixel::Empty => {}
                Pixel::Ink => grid[y][x] = Theme::wordmark_face(y).fg,
                Pixel::Accent => grid[y][x] = Theme::wordmark_accent().fg,
            }
        }
    }
    grid
}

/// One cell from its top and bottom pixel: the upper half block carries the
/// top pixel in its foreground and the bottom one in its background, so the
/// two can differ.
fn cell(top: Option<Color>, bottom: Option<Color>) -> (&'static str, Style) {
    let canvas = Theme::canvas();
    match (top, bottom) {
        (None, None) => (" ", canvas),
        (Some(top), None) => ("▀", canvas.fg(top)),
        (None, Some(bottom)) => ("▄", canvas.fg(bottom)),
        (Some(top), Some(bottom)) if top == bottom => ("█", canvas.fg(top)),
        (Some(top), Some(bottom)) => ("▀", canvas.fg(top).bg(bottom)),
    }
}

/// Rows of coloured pixels as lines of half blocks, two pixel rows to a
/// line, each run of cells that share a style in one span. An odd last row
/// is paired with an empty one.
pub(crate) fn half_blocks(pixels: &[Vec<Option<Color>>]) -> Vec<Line<'static>> {
    pixels
        .chunks(2)
        .map(|pair| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (x, &top) in pair[0].iter().enumerate() {
                let bottom = pair.get(1).and_then(|row| row.get(x).copied()).flatten();
                let (symbol, style) = cell(top, bottom);
                match spans.last_mut() {
                    Some(span) if span.style == style => span.content.to_mut().push_str(symbol),
                    _ => spans.push(Span::styled(symbol.to_string(), style)),
                }
            }
            Line::from(spans)
        })
        .collect()
}

/// The logo as [`HEIGHT`] lines of [`WIDTH`] cells.
pub(crate) fn lines() -> Vec<Line<'static>> {
    half_blocks(&colours())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> Vec<String> {
        lines().iter().map(Line::to_string).collect()
    }

    #[test]
    fn every_row_of_a_letter_is_as_wide_as_the_letter() {
        for letter in LETTERS {
            let width = letter.rows[0].len();
            assert!(letter.rows.iter().all(|row| row.len() == width));
        }
    }

    #[test]
    fn the_logo_is_as_big_as_it_says() {
        let rows = text();
        assert_eq!(rows.len(), usize::from(HEIGHT));
        for row in &rows {
            assert_eq!(row.chars().count(), usize::from(WIDTH), "{row:?}");
        }
    }

    #[test]
    fn the_accents_mirror_each_other_about_the_dot_of_the_j() {
        let rows = letter_pixels();
        let marks = |pixel: Pixel, row: usize| -> Vec<usize> {
            rows[row]
                .iter()
                .enumerate()
                .filter(|(_, p)| **p == pixel)
                .map(|(x, _)| x)
                .collect()
        };
        let (top, bottom) = (marks(Pixel::Accent, 0), marks(Pixel::Accent, 1));
        assert_eq!(top.len(), 2);
        assert_eq!(bottom.len(), 2);
        // The acute rises to the right and the grave falls to it.
        assert_eq!(top[0], bottom[0] + 1);
        assert_eq!(top[1] + 1, bottom[1]);
        let dot = marks(Pixel::Ink, 1)
            .into_iter()
            .find(|&x| x > top[0] && x < top[1])
            .expect("the j's dot between the accents");
        assert_eq!(dot - bottom[0], bottom[1] - dot, "{top:?} {bottom:?} {dot}");
    }

    #[test]
    fn only_the_accents_are_in_the_accent_colour() {
        let accent = Theme::wordmark_accent().fg;
        let painted = colours()
            .iter()
            .flatten()
            .filter(|colour| **colour == accent)
            .count();
        assert_eq!(painted, 4, "two pixels to each accent");
    }

    #[test]
    fn the_logo_is_drawn_in_blocks_only() {
        assert!(
            text()
                .concat()
                .chars()
                .all(|c| matches!(c, ' ' | '▀' | '▄' | '█'))
        );
    }
}
