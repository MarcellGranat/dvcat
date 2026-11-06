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
