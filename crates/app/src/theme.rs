//! The two looks: Dark and Midnight.

use std::rc::Rc;

use gpui_kit::App;
use gpui_kit::component::theme::{Theme, ThemeMode, ThemeSet};

/// The UI font, and the code editor's font unless fixed-width is switched on.
pub const SANS: &str = "Inter";
/// The fixed-width font: always used by the terminal and diffs, where
/// columns must line up, and by the code editor when switched on.
pub const MONO: &str = "JetBrains Mono";

/// Which look is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Dark,
    Midnight,
}

impl Look {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Midnight => "Midnight",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Dark => Self::Midnight,
            Self::Midnight => Self::Dark,
        }
    }

    pub fn from_name(name: &str) -> Self {
        if name == "Midnight" {
            Self::Midnight
        } else {
            Self::Dark
        }
    }
}

const THEMES: &str = include_str!("themes.json");

/// Applies a look to every window.
pub fn apply(look: Look, cx: &mut App) {
    let set: ThemeSet = serde_json::from_str(THEMES).expect("bundled themes parse");
    let config = set
        .themes
        .into_iter()
        .find(|theme| theme.name.as_ref() == look.name())
        .expect("bundled theme exists");
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.apply_config(&Rc::new(config));
        theme.font_family = SANS.into();
        theme.mono_font_family = MONO.into();
    });
    cx.refresh_windows();
}
