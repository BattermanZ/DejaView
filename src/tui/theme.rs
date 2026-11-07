use ratatui::style::{Color, Modifier, Style};

/// The dashboard's colours. Each has one job:
///
/// - teal is DejaView itself and whatever has focus;
/// - orange (peach) is what needs the person: planned pairs, the main key,
///   the space still to free;
/// - green is a hardlink, amber a symlink, red a failure;
/// - the greys rank text from what matters to what can be skimmed.
pub struct Theme;

/// The big number's colour, by what it counts.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tone {
    /// Space a link run would free.
    ToFree,
    /// Space link runs freed.
    Freed,
    /// A run's progress.
    Working,
    /// Nothing to count.
    Quiet,
}

impl Theme {
    const BASE: Color = Color::Rgb(11, 13, 24);
    const MANTLE: Color = Color::Rgb(17, 19, 32);
    const SURFACE0: Color = Color::Rgb(26, 29, 46);
    const SURFACE1: Color = Color::Rgb(36, 40, 61);
    const LINE: Color = Color::Rgb(52, 56, 78);

    const TEXT: Color = Color::Rgb(205, 214, 244);
    const MUTED: Color = Color::Rgb(166, 173, 200);
    const DIM: Color = Color::Rgb(127, 132, 156);
    const FAINT: Color = Color::Rgb(92, 97, 124);

    const TEAL: Color = Color::Rgb(148, 226, 213);
    const PEACH: Color = Color::Rgb(250, 179, 135);
    const GREEN: Color = Color::Rgb(166, 227, 161);
    const AMBER: Color = Color::Rgb(249, 226, 175);
    const RED: Color = Color::Rgb(243, 139, 168);

    // The logo's shades of teal, lightest to darkest.
    const MINT: Color = Color::Rgb(206, 247, 240);
    const TEAL_DEEP: Color = Color::Rgb(104, 196, 186);
    const TEAL_DEEPER: Color = Color::Rgb(80, 168, 160);
    const TEAL_SHADE: Color = Color::Rgb(52, 98, 106);
    const TEAL_SHADE_FAR: Color = Color::Rgb(30, 52, 64);

    // The big number's depth, one per tone.
    const PEACH_SHADE: Color = Color::Rgb(92, 58, 42);
    const GREEN_SHADE: Color = Color::Rgb(52, 84, 52);
    const FAINT_SHADE: Color = Color::Rgb(36, 40, 56);

    fn fg(color: Color) -> Style {
        Style::default().fg(color).bg(Self::BASE)
    }

    /// The screen behind everything: text on the base colour.
    pub fn canvas() -> Style {
        Self::fg(Self::TEXT)
    }

    pub fn text() -> Style {
        Self::fg(Self::TEXT)
    }

    pub fn strong() -> Style {
        Self::text().add_modifier(Modifier::BOLD)
    }

    pub fn muted() -> Style {
        Self::fg(Self::MUTED)
    }

    pub fn dim() -> Style {
        Self::fg(Self::DIM)
    }

    pub fn faint() -> Style {
        Self::fg(Self::FAINT)
    }

    /// Rules and the borders of what does not have focus.
    pub fn rule() -> Style {
        Self::fg(Self::LINE)
    }

    /// The border of a dialog, which has focus.
    pub fn focus() -> Style {
        Self::fg(Self::TEAL)
    }

    /// A column's or a section's name.
    pub fn heading() -> Style {
        Self::fg(Self::TEAL).add_modifier(Modifier::BOLD)
    }

    /// A key in the footer or a dialog.
    pub fn key() -> Style {
        Self::fg(Self::TEAL).add_modifier(Modifier::BOLD)
    }

    /// A key on a chip, where it sits on a border.
    pub fn key_chip() -> Style {
        Style::default()
            .fg(Self::TEXT)
            .bg(Self::SURFACE1)
            .add_modifier(Modifier::BOLD)
    }

    /// The key for what the screen is there to do next.
    pub fn key_primary() -> Style {
        Style::default()
            .fg(Self::BASE)
            .bg(Self::PEACH)
            .add_modifier(Modifier::BOLD)
    }

    /// The selected row's background.
    pub fn selected() -> Style {
        Style::default().bg(Self::SURFACE0)
    }

    /// The name of the library folder a library file is in, when there are
    /// several.
    pub fn library_tag() -> Style {
        Self::fg(Self::TEAL)
    }

    /// The bar at the selected row's left edge.
    pub fn marker() -> Style {
        Style::default().fg(Self::TEAL).bg(Self::SURFACE0)
    }

    pub fn popup() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::MANTLE)
    }

    /// A colour behind an open dialog, pushed back toward the base so the
    /// dialog stands in front.
    pub fn behind_dialog(color: Color, foreground: bool) -> Color {
        let (Color::Rgb(r, g, b), Color::Rgb(br, bg, bb)) = (color, Self::BASE) else {
            return color;
        };
        let keep = if foreground { 0.4 } else { 0.6 };
        let mix =
            |c: u8, base: u8| (f64::from(c) * keep + f64::from(base) * (1.0 - keep)).round() as u8;
        Color::Rgb(mix(r, br), mix(g, bg), mix(b, bb))
    }

    /// `style`'s colour on a dialog's background.
    pub fn on_popup(style: Style) -> Style {
        Self::popup().fg(style.fg.unwrap_or(Self::TEXT))
    }

    pub fn popup_strong() -> Style {
        Self::popup().add_modifier(Modifier::BOLD)
    }

    pub fn popup_muted() -> Style {
        Self::on_popup(Self::muted())
    }

    pub fn popup_dim() -> Style {
        Self::on_popup(Self::dim())
    }

    pub fn popup_key() -> Style {
        Self::on_popup(Self::key()).add_modifier(Modifier::BOLD)
    }

    /// The text cursor in a field being typed in.
    pub fn cursor() -> Style {
        Self::input(true).fg(Self::TEAL)
    }

    pub fn input(active: bool) -> Style {
        if active {
            Style::default().fg(Self::TEXT).bg(Self::SURFACE1)
        } else {
            Style::default().fg(Self::MUTED).bg(Self::SURFACE0)
        }
    }

    /// A pair not linked yet.
    pub fn planned() -> Style {
        Self::fg(Self::PEACH)
    }

    /// A pair that is one file under two names.
    pub fn hardlinked() -> Style {
        Self::fg(Self::GREEN)
    }

    /// A pair whose download points at the library file.
    pub fn symlinked() -> Style {
        Self::fg(Self::AMBER)
    }

    pub fn failed() -> Style {
        Self::fg(Self::RED)
    }

    /// A run's progress.
    pub fn progress() -> Style {
        Self::fg(Self::TEAL)
    }

    /// The part of a bar not filled yet.
    pub fn track() -> Style {
        Self::fg(Self::SURFACE1)
    }

    /// The big number's face and depth.
    pub fn number(tone: Tone) -> (Color, Color) {
        match tone {
            Tone::ToFree => (Self::PEACH, Self::PEACH_SHADE),
            Tone::Freed => (Self::GREEN, Self::GREEN_SHADE),
            Tone::Working => (Self::TEAL, Self::TEAL_SHADE),
            Tone::Quiet => (Self::FAINT, Self::FAINT_SHADE),
        }
    }

    /// The big number's unit beside it, in its colour.
    pub fn number_unit(tone: Tone) -> Style {
        Self::fg(Self::number(tone).0).add_modifier(Modifier::BOLD)
    }

    /// The logo's letters at pixel row `row` of ten: mint at the top,
    /// shading to deep teal at the foot.
    pub fn wordmark_face(row: usize) -> Style {
        let color = match row {
            0..=3 => Self::MINT,
            4..=5 => Self::TEAL,
            6..=7 => Self::TEAL_DEEP,
            _ => Self::TEAL_DEEPER,
        };
        Self::fg(color)
    }

    /// The logo's depth, `layer` pixels behind its letters: the farther,
    /// the darker.
    pub fn wordmark_depth(layer: usize) -> Style {
        Self::fg(if layer <= 1 {
            Self::TEAL_SHADE
        } else {
            Self::TEAL_SHADE_FAR
        })
    }

    /// The logo's two accents, its one warm colour.
    pub fn wordmark_accent() -> Style {
        Self::fg(Self::PEACH)
    }
}
