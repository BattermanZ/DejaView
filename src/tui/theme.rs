use ratatui::style::{Color, Modifier, Style};

pub struct Theme;

impl Theme {
    const BASE: Color = Color::Rgb(30, 30, 46);
    const CRUST: Color = Color::Rgb(17, 17, 27);
    const MANTLE: Color = Color::Rgb(24, 24, 37);
    const SURFACE0: Color = Color::Rgb(49, 50, 68);
    const SURFACE1: Color = Color::Rgb(69, 71, 90);
    const SURFACE2: Color = Color::Rgb(88, 91, 112);
    const TEXT: Color = Color::Rgb(205, 214, 244);
    const SUBTEXT0: Color = Color::Rgb(166, 173, 200);
    const LAVENDER: Color = Color::Rgb(180, 190, 254);
    const BLUE: Color = Color::Rgb(137, 180, 250);
    const YELLOW: Color = Color::Rgb(249, 226, 175);
    const RED: Color = Color::Rgb(243, 139, 168);

    pub fn app() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::CRUST)
    }

    pub fn base() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::BASE)
    }

    pub fn accent() -> Style {
        Style::default()
            .fg(Self::BLUE)
            .bg(Self::BASE)
            .add_modifier(Modifier::BOLD)
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

    pub fn panel() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::SURFACE0)
    }

    pub fn panel_active() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::SURFACE1)
    }

    pub fn panel_feature() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::SURFACE2)
    }

    pub fn popup() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::MANTLE)
    }

    pub fn title(active: bool) -> Style {
        let color = if active { Self::BLUE } else { Self::LAVENDER };
        Style::default()
            .fg(color)
            .bg(Self::CRUST)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge() -> Style {
        Style::default().fg(Self::TEXT).bg(Self::MANTLE)
    }

    pub fn badge_accent() -> Style {
        Style::default()
            .fg(Self::BLUE)
            .bg(Self::MANTLE)
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
