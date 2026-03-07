use ratatui::text::{Line, Span};

use crate::tui::theme::Theme;

const GLYPH_HEIGHT: usize = 5;

type Glyph = [&'static str; GLYPH_HEIGHT];

const D_GLYPH: Glyph = ["████ ", "█   █", "█   █", "█   █", "████ "];
const E_GLYPH: Glyph = ["█████", "█    ", "████ ", "█    ", "█████"];
const J_GLYPH: Glyph = ["█████", "   █ ", "   █ ", "█  █ ", " ██  "];
const A_GLYPH: Glyph = [" ███ ", "█   █", "█████", "█   █", "█   █"];
const V_GLYPH: Glyph = ["█   █", "█   █", "█   █", " █ █ ", "  █  "];
const I_GLYPH: Glyph = ["█████", "  █  ", "  █  ", "  █  ", "█████"];
const W_GLYPH: Glyph = ["█   █", "█   █", "█ █ █", "██ ██", "█   █"];

fn glyph_for(ch: char) -> Glyph {
    match ch {
        'D' => D_GLYPH,
        'E' => E_GLYPH,
        'J' => J_GLYPH,
        'A' => A_GLYPH,
        'V' => V_GLYPH,
        'I' => I_GLYPH,
        'W' => W_GLYPH,
        _ => ["    ", "    ", "    ", "    ", "    "],
    }
}

pub(crate) fn filled_block_rows() -> [String; GLYPH_HEIGHT] {
    let word = "DEJAVIEW";
    let mut rows = std::array::from_fn(|_| String::new());
    let total = word.chars().count();
    for (index, ch) in word.chars().enumerate() {
        let glyph = glyph_for(ch);
        for row in 0..GLYPH_HEIGHT {
            rows[row].push_str(glyph[row]);
            if index + 1 < total {
                rows[row].push(' ');
            }
        }
    }
    rows
}

pub(crate) fn filled_block_lines() -> Vec<Line<'static>> {
    let rows = filled_block_rows();
    let mut lines = Vec::with_capacity(GLYPH_HEIGHT + 1);
    for (index, row) in rows.iter().enumerate() {
        let style = if index == 2 {
            Theme::wordmark_highlight()
        } else {
            Theme::wordmark()
        };
        lines.push(Line::from(Span::styled(row.clone(), style)));
    }
    let shadow = format!("  {}", rows[GLYPH_HEIGHT - 1].replace('█', "░"));
    lines.push(Line::from(Span::styled(shadow, Theme::wordmark_shadow())));
    lines
}
