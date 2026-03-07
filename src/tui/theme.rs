use ratatui::style::{Color, Modifier, Style};

pub struct Theme;

impl Theme {
    const BASE: Color = Color::Rgb(11, 13, 24);
    const CRUST: Color = Color::Rgb(17, 17, 27);
    const MANTLE: Color = Color::Rgb(20, 22, 37);
    const SURFACE0: Color = Color::Rgb(28, 31, 49);
    const SURFACE1: Color = Color::Rgb(36, 40, 61);
    const TEXT: Color = Color::Rgb(205, 214, 244);
    const SUBTEXT0: Color = Color::Rgb(166, 173, 200);
    const SUBTEXT1: Color = Color::Rgb(127, 132, 156);
    const LAVENDER: Color = Color::Rgb(180, 190, 254);
    const TEAL: Color = Color::Rgb(148, 226, 213);
    const SKY: Color = Color::Rgb(137, 220, 235);
    const GREEN: Color = Color::Rgb(166, 227, 161);
    const YELLOW: Color = Color::Rgb(249, 226, 175);
    const RED: Color = Color::Rgb(243, 139, 168);
    const LINE: Color = Color::Rgb(84, 89, 115);

    pub fn canvas() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::BASE)
    }

    pub fn base() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::BASE)
    }

    pub fn accent() -> Style {
        Style::default()
            .fg(Self::TEAL)
            .bg(Self::BASE)
            .add_modifier(Modifier::BOLD)
    }

    pub fn accent_soft() -> Style {
        Style::default().fg(Self::SKY).bg(Self::BASE)
    }

    pub fn line() -> Style {
        Style::default().fg(Self::LINE).bg(Self::BASE)
    }

    pub fn warning() -> Style {
        Style::default().fg(Self::YELLOW).bg(Self::BASE)
    }

    pub fn danger() -> Style {
        Style::default()
            .fg(Self::RED)
            .bg(Self::BASE)
            .add_modifier(Modifier::BOLD)
    }

    pub fn muted() -> Style {
        Style::default().fg(Self::SUBTEXT0).bg(Self::BASE)
    }

    pub fn dim() -> Style {
        Style::default().fg(Self::SUBTEXT1).bg(Self::BASE)
    }

    pub fn panel() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::BASE)
    }

    pub fn popup() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::MANTLE)
    }

    pub fn card() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::SURFACE0)
    }

    pub fn result_row() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::BASE)
    }

    pub fn result_row_selected() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::SURFACE0)
    }

    pub fn title(active: bool) -> Style {
        let color = if active { Self::TEAL } else { Self::LAVENDER };
        Style::default()
            .fg(color)
            .bg(Self::CRUST)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::SURFACE0)
    }

    pub fn badge_accent() -> Style {
        Style::default()
            .fg(Self::CRUST)
            .bg(Self::TEAL)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge_lavender() -> Style {
        Style::default()
            .fg(Self::CRUST)
            .bg(Self::LAVENDER)
            .add_modifier(Modifier::BOLD)
    }

    pub fn metric_label() -> Style {
        Style::default().fg(Self::SUBTEXT0).bg(Self::SURFACE0)
    }

    pub fn metric_number() -> Style {
        Style::default()
            .fg(Self::TEXT)
            .bg(Self::SURFACE0)
            .add_modifier(Modifier::BOLD)
    }

    pub fn wordmark() -> Style {
        Style::default()
            .fg(Self::TEAL)
            .bg(Self::BASE)
            .add_modifier(Modifier::BOLD)
    }

    pub fn wordmark_shadow() -> Style {
        Style::default()
            .fg(Self::LINE)
            .bg(Self::BASE)
            .add_modifier(Modifier::BOLD)
    }

    pub fn wordmark_highlight() -> Style {
        Style::default()
            .fg(Self::SKY)
            .bg(Self::BASE)
            .add_modifier(Modifier::BOLD)
    }

    pub fn subtitle() -> Style {
        Style::default().fg(Self::LAVENDER).bg(Self::BASE)
    }

    pub fn inspector_value() -> Style {
        Style::default()
            .fg(Self::TEXT)
            .bg(Self::BASE)
            .add_modifier(Modifier::BOLD)
    }

    pub fn chip_success() -> Style {
        Style::default()
            .fg(Self::CRUST)
            .bg(Self::GREEN)
            .add_modifier(Modifier::BOLD)
    }

    pub fn chip_warning() -> Style {
        Style::default()
            .fg(Self::CRUST)
            .bg(Self::YELLOW)
            .add_modifier(Modifier::BOLD)
    }

    pub fn chip_danger() -> Style {
        Style::default()
            .fg(Self::CRUST)
            .bg(Self::RED)
            .add_modifier(Modifier::BOLD)
    }

    pub fn input(active: bool) -> Style {
        let background = if active {
            Self::SURFACE1
        } else {
            Self::SURFACE0
        };
        let style = Style::default().fg(Self::TEXT).bg(background);
        if active {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }
}
