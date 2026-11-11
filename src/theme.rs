//! The colour palette ([Catppuccin Mocha](https://catppuccin.com/palette)) and the few styles built from it.
//!
//! Every colour the UI uses lives here, so restyling the app means editing one file.
//! `Color::Rgb` needs a "true colour" terminal; most modern ones (kitty, wezterm, iTerm2,
//! GNOME Terminal, Windows Terminal, tmux with `Tc`) have it.

use ratatui::style::{Color, Modifier, Style};

// Accents.
pub const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
pub const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
pub const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
pub const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const TEAL: Color = Color::Rgb(0x94, 0xe2, 0xd5);
pub const SAPPHIRE: Color = Color::Rgb(0x74, 0xc7, 0xec);
pub const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
pub const LAVENDER: Color = Color::Rgb(0xb4, 0xbe, 0xfe);

// Text, from brightest to faintest.
pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);
pub const OVERLAY1: Color = Color::Rgb(0x7f, 0x84, 0x9c);
pub const OVERLAY0: Color = Color::Rgb(0x6c, 0x70, 0x86);

// Surfaces, from lightest to darkest.
pub const SURFACE2: Color = Color::Rgb(0x58, 0x5b, 0x70);
pub const SURFACE1: Color = Color::Rgb(0x45, 0x47, 0x5a);
pub const SURFACE0: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
pub const CRUST: Color = Color::Rgb(0x11, 0x11, 0x1b);

/// The default text style, also painted over the whole screen as the background.
pub fn base() -> Style {
    Style::new().fg(TEXT).bg(BASE)
}

/// Less important text.
pub fn muted() -> Style {
    Style::new().fg(OVERLAY1)
}

/// Border of a pane; the pane that receives keys is lit up.
pub fn border(focused: bool) -> Style {
    Style::new().fg(if focused { LAVENDER } else { SURFACE2 })
}

pub fn title(focused: bool) -> Style {
    if focused {
        Style::new().fg(LAVENDER).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(SUBTEXT0)
    }
}

/// The row under the cursor.
pub fn selected() -> Style {
    Style::new().bg(SURFACE0).add_modifier(Modifier::BOLD)
}

/// Row-label column of the detail pane ("status", "deps", ...).
pub fn label() -> Style {
    Style::new().fg(OVERLAY1).add_modifier(Modifier::BOLD)
}

/// Which palette is drawn. The UI always paints with the Mocha colours above; the other themes
/// replace them in the finished frame ([`Mode::apply`]), so no widget needs to know about them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Catppuccin Mocha, the dark default.
    #[default]
    Mocha,
    /// Catppuccin Latte, for light terminals.
    Latte,
    /// Mocha with the red/green pair replaced by orange/blue, which stay apart for most
    /// colour-vision deficiencies.
    ColourBlind,
}

impl Mode {
    pub fn next(self) -> Self {
        match self {
            Self::Mocha => Self::Latte,
            Self::Latte => Self::ColourBlind,
            Self::ColourBlind => Self::Mocha,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Mocha => "mocha (dark)",
            Self::Latte => "latte (light)",
            Self::ColourBlind => "colour-blind safe",
        }
    }

    /// Read a theme name from the config file.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "mocha" | "dark" => Some(Self::Mocha),
            "latte" | "light" => Some(Self::Latte),
            "colourblind" | "colorblind" | "colour-blind" | "color-blind" => {
                Some(Self::ColourBlind)
            }
            _ => None,
        }
    }

    /// The colour to draw instead of the Mocha colour `color`.
    pub fn map(self, color: Color) -> Color {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        match self {
            Self::Mocha => color,
            Self::Latte => match color {
                c if c == MAUVE => rgb(0x88, 0x39, 0xef),
                c if c == RED => rgb(0xd2, 0x0f, 0x39),
                c if c == PEACH => rgb(0xfe, 0x64, 0x0b),
                c if c == YELLOW => rgb(0xdf, 0x8e, 0x1d),
                c if c == GREEN => rgb(0x40, 0xa0, 0x2b),
                c if c == TEAL => rgb(0x17, 0x92, 0x99),
                c if c == SAPPHIRE => rgb(0x20, 0x9f, 0xb5),
                c if c == BLUE => rgb(0x1e, 0x66, 0xf5),
                c if c == LAVENDER => rgb(0x72, 0x87, 0xfd),
                c if c == TEXT => rgb(0x4c, 0x4f, 0x69),
                c if c == SUBTEXT0 => rgb(0x6c, 0x6f, 0x85),
                c if c == OVERLAY1 => rgb(0x8c, 0x8f, 0xa1),
                c if c == OVERLAY0 => rgb(0x9c, 0xa0, 0xb0),
                c if c == SURFACE2 => rgb(0xac, 0xb0, 0xbe),
                c if c == SURFACE1 => rgb(0xbc, 0xc0, 0xcc),
                c if c == SURFACE0 => rgb(0xcc, 0xd0, 0xda),
                c if c == BASE => rgb(0xef, 0xf1, 0xf5),
                c if c == CRUST => rgb(0xdc, 0xe0, 0xe8),
                other => other,
            },
            Self::ColourBlind => match color {
                c if c == GREEN => BLUE,
                c if c == RED => PEACH,
                c if c == TEAL => SAPPHIRE,
                other => other,
            },
        }
    }

    /// Recolour everything drawn so far.
    pub fn apply(self, buffer: &mut ratatui::buffer::Buffer) {
        if self == Self::Mocha {
            return;
        }
        for cell in &mut buffer.content {
            cell.fg = self.map(cell.fg);
            cell.bg = self.map(cell.bg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_cycle_and_recolour_only_known_colours() {
        assert_eq!(Mode::Mocha.next().next().next(), Mode::Mocha);
        assert_eq!(Mode::Mocha.map(GREEN), GREEN);
        assert_ne!(Mode::Latte.map(BASE), BASE);
        assert_eq!(Mode::ColourBlind.map(GREEN), BLUE);
        assert_eq!(Mode::Latte.map(Color::Red), Color::Red);
        assert_eq!(Mode::parse("Light"), Some(Mode::Latte));
        assert_eq!(Mode::parse("nope"), None);
    }
}
