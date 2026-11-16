//! How the interface arranges itself for the space it has: which panes sit
//! side by side, and the row of extra keys shown above the on-screen keyboard.
//!
//! This is here, not in the app, so that the rules can be tested without a
//! browser: for example, that every extra key stays big enough to tap on the
//! narrowest phone.

/// One part of the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Files,
    Editor,
    Terminal,
    Changes,
    More,
}

impl Pane {
    pub const ALL: [Self; 5] = [
        Self::Files,
        Self::Editor,
        Self::Terminal,
        Self::Changes,
        Self::More,
    ];

    /// The name on its tab.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Editor => "Editor",
            Self::Terminal => "Terminal",
            Self::Changes => "Changes",
            Self::More => "More",
        }
    }

    /// Whether this pane goes in the third column when there is one.
    #[must_use]
    pub fn is_side(self) -> bool {
        matches!(self, Self::Terminal | Self::Changes | Self::More)
    }
}

/// How many panes fit side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// One pane at a time, with a tab for each: phones, and a folded
    /// foldable.
    Compact,
    /// The file list beside one pane: an open foldable, a tablet held
    /// upright.
    Medium,
    /// The file list, the editor, and a third column for the terminal or the
    /// changes: a tablet on its side.
    Large,
}

/// From this width the file list gets its own column. An open iPhone Duo is
/// about 626 points wide (its 1878 pixels at 3x), so this is below that.
pub const MEDIUM_WIDTH: f32 = 600.0;
/// From this width the terminal gets a column too. An 11-inch iPad Pro on its
/// side is 1210 points wide; a 13-inch one upright (1032) stays at two columns
/// so the editor is not squeezed.
pub const LARGE_WIDTH: f32 = 1100.0;

/// The smallest width and height, in points, of anything meant to be tapped.
pub const MIN_TAP: f32 = 44.0;

impl Layout {
    /// The layout for a visible width in points.
    #[must_use]
    pub fn for_width(width: f32) -> Self {
        if width >= LARGE_WIDTH {
            Self::Large
        } else if width >= MEDIUM_WIDTH {
            Self::Medium
        } else {
            Self::Compact
        }
    }

    /// What to draw when `open` is the pane last chosen and `side` the last
    /// one chosen for the third column: the main pane and, if there is a
    /// third column, what goes in it.
    #[must_use]
    pub fn shown(self, open: Pane, side: Pane) -> (Pane, Option<Pane>) {
        match self {
            // The file list has its own column, so the main one shows the editor.
            Self::Medium if open == Pane::Files => (Pane::Editor, None),
            Self::Compact | Self::Medium => (open, None),
            Self::Large if open.is_side() => (Pane::Editor, Some(open)),
            Self::Large => (Pane::Editor, Some(side)),
        }
    }

    /// The tabs to offer: every pane that does not already have a column.
    #[must_use]
    pub fn tabs(self) -> Vec<Pane> {
        Pane::ALL
            .into_iter()
            .filter(|pane| match self {
                Self::Compact => true,
                Self::Medium => *pane != Pane::Files,
                Self::Large => pane.is_side(),
            })
            .collect()
    }

    /// The width of the file list's column, if it has one.
    #[must_use]
    pub fn sidebar_width(self) -> Option<f32> {
        match self {
            Self::Compact => None,
            Self::Medium => Some(240.0),
            Self::Large => Some(280.0),
        }
    }

    /// The width of the third column, if there is one.
    #[must_use]
    pub fn side_width(self, width: f32) -> Option<f32> {
        (self == Self::Large).then(|| (width * 0.34).clamp(380.0, 520.0))
    }
}

/// Whether an on-screen keyboard is covering part of the page: the visible
/// part is much shorter than the page. Browser toolbars account for less than
/// this; keyboards for far more.
#[must_use]
pub fn keyboard_is_open(page_height: f32, visible_height: f32) -> bool {
    page_height - visible_height > 150.0
}

/// Which way a swipe went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swipe {
    /// The finger moved left: go to the next pane.
    Next,
    /// The finger moved right: go to the previous pane.
    Previous,
}

/// Whether a touch was a swipe between panes. `start_x` is where it began,
/// `dx` and `dy` how far it moved, in points, over `millis` milliseconds on a
/// page `width` points wide.
///
/// It has to be a clearly sideways flick made at some speed, so that
/// scrolling, selecting text and an unsteady tap are not mistaken for one:
/// at least 90 points, within 0.7 seconds, at 250 points a second or more.
/// It must also start away from the screen's edges, which Safari keeps for
/// going back and forward.
#[must_use]
pub fn swipe(start_x: f32, dx: f32, dy: f32, millis: f32, width: f32) -> Option<Swipe> {
    let sideways = dx.abs() >= 90.0 && dy.abs() <= dx.abs() * 0.35;
    let quick = millis <= 700.0 && dx.abs() >= millis * 0.25;
    let clear_of_edges = start_x >= 30.0 && start_x <= width - 30.0;
    (sideways && quick && clear_of_edges).then_some(if dx < 0.0 {
        Swipe::Next
    } else {
        Swipe::Previous
    })
}

impl Swipe {
    /// The tab to show after this swipe when `current` is showing, or `None`
    /// at either end: a swipe does not wrap round.
    #[must_use]
    pub fn from(self, current: Pane, tabs: &[Pane]) -> Option<Pane> {
        let at = tabs.iter().position(|tab| *tab == current)?;
        match self {
            Self::Next => tabs.get(at + 1).copied(),
            Self::Previous => at.checked_sub(1).map(|before| tabs[before]),
        }
    }
}

/// What an extra key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Sends a key, named as GPUI names keystrokes: `escape`, `left`.
    Key(&'static str),
    /// Types this text.
    Text(&'static str),
    /// Holds Control for the next key.
    Control,
    /// Shows the next group of keys.
    NextPage,
}

/// One extra key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    /// What is drawn on it.
    pub label: &'static str,
    /// What a screen reader or Voice Control calls it.
    pub name: &'static str,
    pub press: Press,
}

const fn key(label: &'static str, name: &'static str, keystroke: &'static str) -> Cap {
    Cap {
        label,
        name,
        press: Press::Key(keystroke),
    }
}

const fn text(label: &'static str, name: &'static str) -> Cap {
    Cap {
        label,
        name,
        press: Press::Text(label),
    }
}

const MORE: Cap = Cap {
    label: "⋯",
    name: "More keys",
    press: Press::NextPage,
};

/// The extra keys, a group at a time. The first group holds the keys an
/// on-screen keyboard does not have at all; the rest hold the punctuation
/// code needs that it buries two layers deep. Every group ends with the key
/// that shows the next one, so no group needs scrolling.
pub const KEY_PAGES: [&[Cap]; 4] = [
    &[
        key("Esc", "Escape", "escape"),
        key("Tab", "Tab", "tab"),
        key("←", "Left", "left"),
        key("↓", "Down", "down"),
        key("↑", "Up", "up"),
        key("→", "Right", "right"),
        Cap {
            label: "Ctrl",
            name: "Control",
            press: Press::Control,
        },
        MORE,
    ],
    &[
        text("{", "Left brace"),
        text("}", "Right brace"),
        text("[", "Left bracket"),
        text("]", "Right bracket"),
        text("(", "Left parenthesis"),
        text(")", "Right parenthesis"),
        text("=", "Equals"),
        MORE,
    ],
    &[
        text("<", "Less than"),
        text(">", "Greater than"),
        text("|", "Vertical bar"),
        text("\\", "Backslash"),
        text("_", "Underscore"),
        text("#", "Number sign"),
        text("`", "Backtick"),
        MORE,
    ],
    &[
        text("\"", "Double quote"),
        text("'", "Single quote"),
        text(":", "Colon"),
        text(";", "Semicolon"),
        text("/", "Slash"),
        text("-", "Hyphen"),
        text("+", "Plus"),
        MORE,
    ],
];

/// The width each key gets when a group is spread across `width` points with
/// `gap` points between keys and `edge` points at each end.
#[must_use]
pub fn key_width(keys: usize, width: f32, gap: f32, edge: f32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let keys = keys.max(1) as f32;
    (width - 2.0 * edge - gap * (keys - 1.0)) / keys
}

/// How many rows a group of `keys` keys needs so that each is at least
/// [`MIN_TAP`] wide: one, or two on a very narrow screen.
#[must_use]
pub fn key_rows(keys: usize, width: f32, gap: f32, edge: f32) -> usize {
    if key_width(keys, width, gap, edge) >= MIN_TAP {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_for_the_devices_this_is_built_for() {
        // Points are pixels divided by the display's scale. The iPhone Duo's
        // are worked out from Apple's pixel counts at 3x, not published.
        assert_eq!(Layout::for_width(390.0), Layout::Compact); // iPhone
        assert_eq!(Layout::for_width(466.0), Layout::Compact); // iPhone Duo, folded
        assert_eq!(Layout::for_width(626.0), Layout::Medium); // iPhone Duo, open
        assert_eq!(Layout::for_width(890.0), Layout::Medium); // iPhone Duo, open, on its side
        assert_eq!(Layout::for_width(834.0), Layout::Medium); // iPad Pro 11-inch, upright
        assert_eq!(Layout::for_width(1032.0), Layout::Medium); // iPad Pro 13-inch, upright
        assert_eq!(Layout::for_width(1210.0), Layout::Large); // iPad Pro 11-inch, on its side
        assert_eq!(Layout::for_width(1376.0), Layout::Large); // iPad Pro 13-inch, on its side
    }

    #[test]
    fn what_each_layout_shows() {
        use Pane::{Changes, Editor, Files, More, Terminal};
        assert_eq!(Layout::Compact.shown(Files, Terminal), (Files, None));
        assert_eq!(Layout::Compact.tabs(), Pane::ALL);
        // With the file list in its own column, choosing it shows the editor.
        assert_eq!(Layout::Medium.shown(Files, Terminal), (Editor, None));
        assert_eq!(Layout::Medium.shown(Changes, Terminal), (Changes, None));
        assert_eq!(Layout::Medium.tabs(), [Editor, Terminal, Changes, More]);
        // The widest layout always shows the editor and remembers the third column.
        assert_eq!(
            Layout::Large.shown(Editor, Changes),
            (Editor, Some(Changes))
        );
        assert_eq!(Layout::Large.shown(More, Changes), (Editor, Some(More)));
        assert_eq!(Layout::Large.tabs(), [Terminal, Changes, More]);
        // Every pane can be reached in every layout: by a tab or its own column.
        for layout in [Layout::Compact, Layout::Medium, Layout::Large] {
            for pane in Pane::ALL {
                let has_column = match pane {
                    Files => layout.sidebar_width().is_some(),
                    Editor => layout == Layout::Large,
                    _ => false,
                };
                assert!(
                    has_column || layout.tabs().contains(&pane),
                    "{layout:?} {pane:?}"
                );
            }
        }
    }

    #[test]
    fn every_column_leaves_the_editor_room() {
        for width in [600.0, 626.0, 834.0, 1032.0, 1100.0, 1210.0, 1376.0, 2000.0] {
            let layout = Layout::for_width(width);
            let used =
                layout.sidebar_width().unwrap_or(0.0) + layout.side_width(width).unwrap_or(0.0);
            assert!(
                width - used >= 360.0,
                "{width}: the editor gets {}",
                width - used
            );
        }
    }

    #[test]
    fn extra_keys_stay_big_enough_to_tap_on_the_narrowest_phone() {
        for page in KEY_PAGES {
            // One row from 375 points wide (iPhone mini and every current iPhone).
            assert_eq!(key_rows(page.len(), 375.0, 2.0, 4.0), 1);
            // At 320 points (Display Zoom on a small phone) the group takes
            // two rows instead of shrinking its keys.
            assert_eq!(key_rows(page.len(), 320.0, 2.0, 4.0), 2);
            assert!(key_width(page.len().div_ceil(2), 320.0, 2.0, 4.0) >= MIN_TAP);
            assert_eq!(page.last().map(|cap| cap.press), Some(Press::NextPage));
        }
        let names: Vec<&str> = KEY_PAGES
            .iter()
            .flat_map(|page| page.iter())
            .map(|cap| cap.name)
            .collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        // "More keys" is on every page; every other name is said once.
        assert_eq!(unique.len(), names.len() - (KEY_PAGES.len() - 1));
    }

    #[test]
    fn only_a_quick_sideways_flick_is_a_swipe() {
        assert_eq!(swipe(200.0, -140.0, 10.0, 200.0, 390.0), Some(Swipe::Next));
        assert_eq!(
            swipe(200.0, 140.0, -20.0, 300.0, 390.0),
            Some(Swipe::Previous)
        );
        // A slower hand still counts if it covers the ground.
        assert_eq!(swipe(300.0, -200.0, 0.0, 600.0, 390.0), Some(Swipe::Next));
        // Too short, too slow for its length, too long in time, too diagonal, or a scroll.
        assert_eq!(swipe(200.0, -60.0, 0.0, 150.0, 390.0), None);
        assert_eq!(swipe(200.0, -100.0, 0.0, 600.0, 390.0), None);
        assert_eq!(swipe(200.0, -300.0, 0.0, 900.0, 390.0), None);
        assert_eq!(swipe(200.0, -140.0, 80.0, 200.0, 390.0), None);
        assert_eq!(swipe(200.0, 5.0, -300.0, 200.0, 390.0), None);
        // From the screen's edge it is Safari's back or forward gesture.
        assert_eq!(swipe(8.0, 160.0, 0.0, 200.0, 390.0), None);
        assert_eq!(swipe(385.0, -160.0, 0.0, 200.0, 390.0), None);
    }

    #[test]
    fn a_swipe_moves_one_tab_and_stops_at_the_ends() {
        use Pane::{Changes, Editor, Files, More, Terminal};
        let tabs = Layout::Compact.tabs();
        assert_eq!(Swipe::Next.from(Editor, &tabs), Some(Terminal));
        assert_eq!(Swipe::Next.from(Terminal, &tabs), Some(Changes));
        assert_eq!(Swipe::Previous.from(Terminal, &tabs), Some(Editor));
        assert_eq!(Swipe::Previous.from(Files, &tabs), None);
        assert_eq!(Swipe::Next.from(More, &tabs), None);
        // Where the file list has its own column it is not a stop.
        assert_eq!(Swipe::Previous.from(Editor, &Layout::Medium.tabs()), None);
    }

    #[test]
    fn keyboard_detection() {
        assert!(keyboard_is_open(844.0, 508.0));
        assert!(!keyboard_is_open(844.0, 844.0));
        // Safari's toolbar hiding and showing is not a keyboard.
        assert!(!keyboard_is_open(844.0, 744.0));
    }
}
