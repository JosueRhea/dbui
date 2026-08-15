//! Colours and metrics.
//!
//! Themes are named palettes (Wave, Gruvbox, Nord, …) adapted from edui's
//! catalog, plus a clean Light. Everything that draws takes `&Theme` — nothing
//! reads a colour from a global.

use dbui_app::domain::{Driver, ValueKind};
use gpui::{px, rgb, rgba, Pixels, Rgba};

/// A full chrome + value-colour palette.
#[derive(Clone)]
pub struct Theme {
    pub id: &'static str,
    pub label: &'static str,
    pub is_light: bool,

    /// The window behind everything.
    pub background: Rgba,
    /// Sidebar and toolbars -- one step back from the content.
    pub panel: Rgba,
    /// Menus, modals, and the grid header: one step forward.
    pub elevated: Rgba,
    pub border: Rgba,
    /// A divider inside a panel, weaker than a border between panels.
    pub divider: Rgba,

    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_faint: Rgba,
    /// Text on an accent-filled surface.
    pub text_on_accent: Rgba,

    pub accent: Rgba,
    pub accent_hover: Rgba,
    /// A selected row: tinted, not filled, so the text stays readable.
    pub selection: Rgba,
    pub hover: Rgba,
    pub stripe: Rgba,

    pub success: Rgba,
    pub danger: Rgba,
    pub warning: Rgba,

    // Value colours in the grid. Typed data is easier to scan when its type is
    // visible without reading the header.
    pub value_null: Rgba,
    pub value_number: Rgba,
    pub value_text: Rgba,
    pub value_bool: Rgba,
    pub value_temporal: Rgba,
    pub value_structured: Rgba,
    pub value_binary: Rgba,
}

/// Built-in themes in picker order. Wave stays first — it is the default.
pub fn all_themes() -> &'static [Theme] {
    static THEMES: std::sync::OnceLock<Vec<Theme>> = std::sync::OnceLock::new();
    THEMES.get_or_init(|| {
        vec![
            wave(),
            gruvbox_dark(),
            gruvbox_light(),
            nord(),
            solarized_dark(),
            solarized_light(),
            tokyo_night(),
            light(),
        ]
    })
}

pub fn theme_by_id(id: &str) -> Option<&'static Theme> {
    all_themes().iter().find(|theme| theme.id == id)
}

pub fn default_theme() -> Theme {
    wave()
}

impl Theme {
    /// Clone the named theme, or Wave if unknown.
    pub fn named(id: &str) -> Self {
        theme_by_id(id)
            .cloned()
            .unwrap_or_else(default_theme)
    }

    /// The colour for a decoded cell.
    pub fn value_color(&self, kind: ValueKind) -> Rgba {
        match kind {
            ValueKind::Null => self.value_null,
            ValueKind::Bool => self.value_bool,
            ValueKind::Number => self.value_number,
            ValueKind::Text => self.value_text,
            ValueKind::Binary => self.value_binary,
            ValueKind::Uuid => self.value_temporal,
            ValueKind::Temporal => self.value_temporal,
            ValueKind::Structured => self.value_structured,
            ValueKind::Unsupported => self.danger,
        }
    }

    /// The engine's own brand colour, for the dot beside a connection.
    pub fn driver_color(&self, driver: Driver) -> Rgba {
        match driver {
            Driver::Postgres => rgb(0x4a90d9),
            Driver::MySql => rgb(0xe48e00),
            Driver::Sqlite => rgb(0x6bbf59),
        }
    }

    /// Alternating row tint. Returns `None` for the plain rows so the caller
    /// can skip painting them at all.
    pub fn stripe(&self, index: usize) -> Option<Rgba> {
        (index % 2 == 1).then_some(self.stripe)
    }
}

impl Default for Theme {
    fn default() -> Self {
        default_theme()
    }
}

fn dark_hover() -> Rgba {
    rgba(0xffffff0d)
}
fn light_hover() -> Rgba {
    rgba(0x0000000a)
}
fn dark_stripe() -> Rgba {
    rgba(0xffffff05)
}
fn light_stripe() -> Rgba {
    rgba(0x00000006)
}

/// dbui's original dark palette (close to edui's Wave chrome).
fn wave() -> Theme {
    Theme {
        id: "wave",
        label: "Wave",
        is_light: false,
        background: rgb(0x1b1e24),
        panel: rgb(0x16191e),
        elevated: rgb(0x22262e),
        border: rgb(0x2c313b),
        divider: rgb(0x252932),
        text: rgb(0xd7dbe2),
        text_muted: rgb(0x8b93a1),
        text_faint: rgb(0x5c6472),
        text_on_accent: rgb(0xffffff),
        accent: rgb(0x4d8ef7),
        accent_hover: rgb(0x6ba1f8),
        selection: rgba(0x4d8ef733),
        hover: dark_hover(),
        stripe: dark_stripe(),
        success: rgb(0x4cc38a),
        danger: rgb(0xf16d70),
        warning: rgb(0xdcae4a),
        value_null: rgb(0x646c7a),
        value_number: rgb(0x7fd0a8),
        value_text: rgb(0xd7dbe2),
        value_bool: rgb(0xd9a2f0),
        value_temporal: rgb(0x7fb8e8),
        value_structured: rgb(0xdcae4a),
        value_binary: rgb(0x9aa3b2),
    }
}

fn gruvbox_dark() -> Theme {
    Theme {
        id: "gruvbox-dark",
        label: "Gruvbox Dark",
        is_light: false,
        background: rgb(0x282828),
        panel: rgb(0x1d2021),
        elevated: rgb(0x32302f),
        border: rgb(0x504945),
        divider: rgb(0x3c3836),
        text: rgb(0xebdbb2),
        text_muted: rgb(0xd5c4a1),
        text_faint: rgb(0x928374),
        text_on_accent: rgb(0x1d2021),
        accent: rgb(0xfabd2f),
        accent_hover: rgb(0xfe8019),
        selection: rgba(0x50494588),
        hover: dark_hover(),
        stripe: dark_stripe(),
        success: rgb(0xb8bb26),
        danger: rgb(0xfb4934),
        warning: rgb(0xfabd2f),
        value_null: rgb(0x928374),
        value_number: rgb(0xd3869b),
        value_text: rgb(0xebdbb2),
        value_bool: rgb(0xfb4934),
        value_temporal: rgb(0x83a598),
        value_structured: rgb(0x8ec07c),
        value_binary: rgb(0xa89984),
    }
}

fn gruvbox_light() -> Theme {
    Theme {
        id: "gruvbox-light",
        label: "Gruvbox Light",
        is_light: true,
        background: rgb(0xfbf1c7),
        panel: rgb(0xf2e5bc),
        elevated: rgb(0xebdbb2),
        border: rgb(0xd5c4a1),
        divider: rgb(0xebdbb2),
        text: rgb(0x3c3836),
        text_muted: rgb(0x504945),
        text_faint: rgb(0x7c6f64),
        text_on_accent: rgb(0xfbf1c7),
        accent: rgb(0xb57614),
        accent_hover: rgb(0xaf3a03),
        selection: rgba(0xb5761433),
        hover: light_hover(),
        stripe: light_stripe(),
        success: rgb(0x79740e),
        danger: rgb(0x9d0006),
        warning: rgb(0xb57614),
        value_null: rgb(0x928374),
        value_number: rgb(0x8f3f71),
        value_text: rgb(0x3c3836),
        value_bool: rgb(0x9d0006),
        value_temporal: rgb(0x076678),
        value_structured: rgb(0x427b58),
        value_binary: rgb(0x7c6f64),
    }
}

fn nord() -> Theme {
    Theme {
        id: "nord",
        label: "Nord",
        is_light: false,
        background: rgb(0x2e3440),
        panel: rgb(0x292e39),
        elevated: rgb(0x3b4252),
        border: rgb(0x434c5e),
        divider: rgb(0x3b4252),
        text: rgb(0xd8dee9),
        text_muted: rgb(0xb8c2d0),
        text_faint: rgb(0x7b88a1),
        text_on_accent: rgb(0x2e3440),
        accent: rgb(0x88c0d0),
        accent_hover: rgb(0x81a1c1),
        selection: rgba(0x88c0d033),
        hover: dark_hover(),
        stripe: dark_stripe(),
        success: rgb(0xa3be8c),
        danger: rgb(0xbf616a),
        warning: rgb(0xebcb8b),
        value_null: rgb(0x616e88),
        value_number: rgb(0xb48ead),
        value_text: rgb(0xd8dee9),
        value_bool: rgb(0x81a1c1),
        value_temporal: rgb(0x8fbcbb),
        value_structured: rgb(0xa3be8c),
        value_binary: rgb(0x7b88a1),
    }
}

fn solarized_dark() -> Theme {
    Theme {
        id: "solarized-dark",
        label: "Solarized Dark",
        is_light: false,
        background: rgb(0x002b36),
        panel: rgb(0x002128),
        elevated: rgb(0x073642),
        border: rgb(0x0d4a55),
        divider: rgb(0x073642),
        text: rgb(0x93a1a1),
        text_muted: rgb(0x839496),
        text_faint: rgb(0x586e75),
        text_on_accent: rgb(0x002b36),
        accent: rgb(0xb58900),
        accent_hover: rgb(0xcb4b16),
        selection: rgba(0x268bd233),
        hover: dark_hover(),
        stripe: dark_stripe(),
        success: rgb(0x859900),
        danger: rgb(0xdc322f),
        warning: rgb(0xb58900),
        value_null: rgb(0x586e75),
        value_number: rgb(0xd33682),
        value_text: rgb(0x93a1a1),
        value_bool: rgb(0x859900),
        value_temporal: rgb(0x268bd2),
        value_structured: rgb(0x2aa198),
        value_binary: rgb(0x657b83),
    }
}

fn solarized_light() -> Theme {
    Theme {
        id: "solarized-light",
        label: "Solarized Light",
        is_light: true,
        background: rgb(0xfdf6e3),
        panel: rgb(0xeee8d5),
        elevated: rgb(0xeee8d5),
        border: rgb(0x93a1a1),
        divider: rgb(0xd3cbb7),
        text: rgb(0x657b83),
        text_muted: rgb(0x586e75),
        text_faint: rgb(0x93a1a1),
        text_on_accent: rgb(0xfdf6e3),
        accent: rgb(0xb58900),
        accent_hover: rgb(0xcb4b16),
        selection: rgba(0x268bd233),
        hover: light_hover(),
        stripe: light_stripe(),
        success: rgb(0x859900),
        danger: rgb(0xdc322f),
        warning: rgb(0xb58900),
        value_null: rgb(0x93a1a1),
        value_number: rgb(0xd33682),
        value_text: rgb(0x657b83),
        value_bool: rgb(0x859900),
        value_temporal: rgb(0x268bd2),
        value_structured: rgb(0x2aa198),
        value_binary: rgb(0x839496),
    }
}

fn tokyo_night() -> Theme {
    Theme {
        id: "tokyo-night",
        label: "Tokyo Night",
        is_light: false,
        background: rgb(0x1a1b26),
        panel: rgb(0x16161e),
        elevated: rgb(0x1f2335),
        border: rgb(0x2f334d),
        divider: rgb(0x24283b),
        text: rgb(0xc0caf5),
        text_muted: rgb(0xa9b1d6),
        text_faint: rgb(0x565f89),
        text_on_accent: rgb(0x1a1b26),
        accent: rgb(0x7aa2f7),
        accent_hover: rgb(0xbb9af7),
        selection: rgba(0x7aa2f733),
        hover: dark_hover(),
        stripe: dark_stripe(),
        success: rgb(0x9ece6a),
        danger: rgb(0xf7768e),
        warning: rgb(0xe0af68),
        value_null: rgb(0x565f89),
        value_number: rgb(0xff9e64),
        value_text: rgb(0xc0caf5),
        value_bool: rgb(0xbb9af7),
        value_temporal: rgb(0x2ac3de),
        value_structured: rgb(0x9ece6a),
        value_binary: rgb(0x7dcfff),
    }
}

/// Clean light chrome — easy on the eyes for daytime use.
fn light() -> Theme {
    Theme {
        id: "light",
        label: "Light",
        is_light: true,
        background: rgb(0xf6f7f9),
        panel: rgb(0xffffff),
        elevated: rgb(0xffffff),
        border: rgb(0xd8dde6),
        divider: rgb(0xe6e9ef),
        text: rgb(0x1c2333),
        text_muted: rgb(0x5c6578),
        text_faint: rgb(0x8b93a3),
        text_on_accent: rgb(0xffffff),
        accent: rgb(0x3b6ff5),
        accent_hover: rgb(0x2f5fd9),
        selection: rgba(0x3b6ff528),
        hover: light_hover(),
        stripe: light_stripe(),
        success: rgb(0x1f9d63),
        danger: rgb(0xd64545),
        warning: rgb(0xc48a1a),
        value_null: rgb(0x8b93a3),
        value_number: rgb(0x1f9d63),
        value_text: rgb(0x1c2333),
        value_bool: rgb(0x8b5cf6),
        value_temporal: rgb(0x2563eb),
        value_structured: rgb(0xc48a1a),
        value_binary: rgb(0x5c6578),
    }
}

/// Fixed metrics. Sizes that several components have to agree on -- a row
/// height the grid and its header both use, a sidebar width the layout depends
/// on -- rather than every dimension in the app.
///
/// Values scale with [`zoom_pct`] so ⌘+/⌘- zooms the whole chrome.
pub mod metrics {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const DEFAULT_ZOOM_PCT: u32 = 100;
    const MIN_ZOOM_PCT: u32 = 50;
    const MAX_ZOOM_PCT: u32 = 200;
    const ZOOM_STEP_PCT: u32 = 10;

    static ZOOM_PCT: AtomicU32 = AtomicU32::new(DEFAULT_ZOOM_PCT);

    /// Current UI zoom as a percentage (100 = default).
    pub fn zoom_pct() -> u32 {
        ZOOM_PCT.load(Ordering::Relaxed)
    }

    pub fn zoom() -> f32 {
        zoom_pct() as f32 / 100.0
    }

    /// Rem size matching the current zoom (1rem = 16px at 100%).
    pub fn rem_size() -> Pixels {
        px(16.0 * zoom())
    }

    pub fn set_zoom_pct(pct: u32) {
        let pct = pct.clamp(MIN_ZOOM_PCT, MAX_ZOOM_PCT);
        // Snap to step.
        let pct = (pct / ZOOM_STEP_PCT) * ZOOM_STEP_PCT;
        ZOOM_PCT.store(pct.max(MIN_ZOOM_PCT), Ordering::Relaxed);
    }

    pub fn zoom_in() -> u32 {
        set_zoom_pct(zoom_pct().saturating_add(ZOOM_STEP_PCT));
        zoom_pct()
    }

    pub fn zoom_out() -> u32 {
        set_zoom_pct(zoom_pct().saturating_sub(ZOOM_STEP_PCT));
        zoom_pct()
    }

    pub fn zoom_reset() -> u32 {
        set_zoom_pct(DEFAULT_ZOOM_PCT);
        zoom_pct()
    }

    fn z(base: f32) -> Pixels {
        px(base * zoom())
    }

    pub fn titlebar_height() -> Pixels {
        z(38.)
    }
    /// macOS traffic lights need this much clear space before the first
    /// control in the titlebar.
    pub fn traffic_light_inset() -> Pixels {
        z(78.)
    }
    pub fn sidebar_width() -> Pixels {
        z(258.)
    }
    pub fn status_height() -> Pixels {
        z(26.)
    }
    pub fn toolbar_height() -> Pixels {
        z(34.)
    }

    pub fn row_height() -> Pixels {
        z(26.)
    }
    pub fn header_height() -> Pixels {
        z(28.)
    }
    /// The gutter holding row numbers.
    pub fn row_number_width() -> Pixels {
        z(52.)
    }

    pub fn column_min_width() -> f32 {
        72. * zoom()
    }
    pub fn column_max_width() -> f32 {
        380. * zoom()
    }
    /// Roughly one monospace character at the grid's text size. Column widths
    /// are estimated from character counts rather than measured, which is
    /// close enough for a starting width the user can drag.
    ///
    /// Geist Mono advances at ~0.6em — keep this in sync so caret panning
    /// tracks the real glyphs.
    pub fn char_width() -> f32 {
        f32::from(text_size()) * 0.6
    }

    /// Monospace advance for detail fields (`text_size_small`).
    pub fn field_char_width() -> f32 {
        f32::from(text_size_small()) * 0.6
    }

    pub fn cell_padding() -> f32 {
        20. * zoom()
    }

    pub fn text_size() -> Pixels {
        z(13.)
    }
    pub fn text_size_small() -> Pixels {
        z(11.)
    }
    pub fn editor_text_size() -> Pixels {
        z(13.)
    }
    pub fn editor_line_height() -> Pixels {
        z(20.)
    }

    /// The monospace face for SQL and for cell values.
    ///
    /// Bundled Geist Mono (same as edui) so columns line up without depending
    /// on a system install.
    pub const MONO_FONT: &str = "Geist Mono";
    /// UI chrome uses the same family so the app reads as one type system.
    pub const UI_FONT: &str = "Geist Mono";
}
