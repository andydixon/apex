//! Declarative colour themes.
//!
//! Themes are TOML documents (see `assets/themes/*.toml`). Built-in themes are
//! embedded at compile time; user themes are loaded from the config directory
//! and may override any built-in by name.
//!
//! A theme has three colour groups: `ui` (chrome: panels, tabs, menus),
//! `editor` (the text area and its decorations) and `syntax` (a map from
//! dotted scope names to [`SyntaxStyle`]). Parsing layers a file over a base:
//! the built-in dark or light theme, which itself sits over [`Theme::minimal`],
//! so every colour is always defined and a user theme may list only the keys
//! it changes. `ApexApp` owns the loaded `Vec<Theme>` and the active one;
//! [`Theme::apply_to_egui`] pushes the active theme into egui's `Visuals` so
//! stock widgets match.
//!
//! Gotcha: the base themes are re-parsed from their TOML on every
//! [`Theme::parse`] call; cheap enough, but not something to do per frame.

use std::collections::HashMap;

use egui::Color32;
use serde::Deserialize;

/// A fully resolved colour scheme. Cloned freely (a few hundred bytes plus
/// the syntax map).
#[derive(Debug, Clone)]
pub struct Theme {
    /// Display name, also the key the `theme` setting refers to.
    pub name: String,
    /// Whether this is a dark scheme; picks the egui base visuals and the
    /// fallback base when parsing.
    pub dark: bool,
    pub ui: UiColors,
    pub editor: EditorColors,
    /// Scope name (e.g. `keyword.control`) → style; queried by prefix in `syntax_style`.
    syntax: HashMap<String, SyntaxStyle>,
}

/// How one syntax scope is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxStyle {
    pub fg: Color32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// Declares a `Copy` struct of `Color32` fields plus a `from_raw` constructor
/// that reads each field by name from a string map, parsing `#rrggbb` /
/// `#rrggbbaa` and falling back to the same field of `fallback` when absent.
macro_rules! colour_struct {
    ($name:ident { $($field:ident),* $(,)? }) => {
        #[derive(Debug, Clone, Copy)]
        pub struct $name { $(pub $field: Color32,)* }

        impl $name {
            fn from_raw(raw: &HashMap<String, String>, fallback: &$name) -> Result<Self, ThemeError> {
                Ok(Self { $($field: match raw.get(stringify!($field)) { Some(s) => parse_colour(s)?, None => fallback.$field },)* })
            }
        }
    };
}

// Colours of the application chrome (everything outside the text area).
// Field names are the keys of the `[ui]` table in a theme file.
colour_struct!(UiColors {
    background,
    surface,
    surface_raised,
    border,
    text,
    text_muted,
    accent,
    accent_text,
    tab_active,
    tab_inactive,
    tab_text_active,
    tab_text_inactive,
    status_bar,
    sidebar,
    panel,
    menu,
    hover,
    selection,
    warning,
    error,
    success,
    scrollbar,
    scrollbar_hover,
});

// Colours of the editor widget. Field names are the keys of the `[editor]`
// table in a theme file.
colour_struct!(EditorColors {
    background,
    foreground,
    gutter_background,
    line_number,
    line_number_active,
    current_line,
    selection,
    selection_inactive,
    caret,
    caret_secondary,
    whitespace,
    indent_guide,
    indent_guide_active,
    ruler,
    bracket_match,
    search_match,
    search_match_current,
    word_highlight,
    fold_marker,
    git_added,
    git_modified,
    git_deleted,
    diagnostic_error,
    diagnostic_warning,
    diagnostic_info,
    minimap_viewport,
});

/// Why a theme file was rejected.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme file is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    /// A colour value that is not `#rrggbb` or `#rrggbbaa`.
    #[error("invalid colour {0:?}: expected #rrggbb or #rrggbbaa")]
    Colour(String),
}

/// A `[syntax]` entry as written: either a bare colour string or a table
/// with `fg` and optional style flags.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawSyntax {
    Plain(String),
    Styled {
        fg: String,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        italic: bool,
        #[serde(default)]
        underline: bool,
    },
}

/// The on-disk shape of a theme file. Colour tables are kept as strings so
/// missing keys can fall back to the base theme in `from_raw`.
#[derive(Deserialize)]
struct RawTheme {
    name: String,
    #[serde(default)]
    dark: bool,
    #[serde(default)]
    ui: HashMap<String, String>,
    #[serde(default)]
    editor: HashMap<String, String>,
    #[serde(default)]
    syntax: HashMap<String, RawSyntax>,
}

/// Parse `#rrggbb` or `#rrggbbaa` (alpha unmultiplied). Surrounding
/// whitespace is ignored; anything else is a [`ThemeError::Colour`].
pub fn parse_colour(s: &str) -> Result<Color32, ThemeError> {
    let hex = s.trim().strip_prefix('#').ok_or_else(|| ThemeError::Colour(s.into()))?;
    let v = u32::from_str_radix(hex, 16).map_err(|_| ThemeError::Colour(s.into()))?;
    match hex.len() {
        6 => Ok(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
        8 => Ok(Color32::from_rgba_unmultiplied((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)),
        _ => Err(ThemeError::Colour(s.into())),
    }
}

/// Embedded theme sources in presentation order. Index 0 must be the dark
/// base and index 1 the light base: `base_dark`/`base_light` rely on it.
pub const BUILTIN_THEME_SOURCES: &[&str] = &[
    include_str!("../../../assets/themes/apex-dark.toml"),
    include_str!("../../../assets/themes/apex-light.toml"),
    include_str!("../../../assets/themes/graphite.toml"),
    include_str!("../../../assets/themes/midnight.toml"),
    include_str!("../../../assets/themes/high-contrast-dark.toml"),
    include_str!("../../../assets/themes/high-contrast-light.toml"),
    include_str!("../../../assets/themes/solar-dark.toml"),
    include_str!("../../../assets/themes/solar-light.toml"),
];

impl Theme {
    /// Parse a theme. Missing colours fall back to the built-in dark or light
    /// base so partial user themes still work.
    pub fn parse(src: &str) -> Result<Theme, ThemeError> {
        let raw: RawTheme = toml::from_str(src)?;
        let base = if raw.dark { Theme::base_dark() } else { Theme::base_light() };
        Self::parse_with_base(raw, &base)
    }

    /// Resolve `raw` over `base`: syntax entries are added to a copy of the
    /// base's map (so a user theme may restyle one scope), colour tables
    /// fall back field by field.
    fn parse_with_base(raw: RawTheme, base: &Theme) -> Result<Theme, ThemeError> {
        let mut syntax = base.syntax.clone();
        for (k, v) in raw.syntax {
            let style = match v {
                RawSyntax::Plain(c) => SyntaxStyle { fg: parse_colour(&c)?, bold: false, italic: false, underline: false },
                RawSyntax::Styled { fg, bold, italic, underline } => SyntaxStyle { fg: parse_colour(&fg)?, bold, italic, underline },
            };
            syntax.insert(k, style);
        }
        Ok(Theme {
            name: raw.name,
            dark: raw.dark,
            ui: UiColors::from_raw(&raw.ui, &base.ui)?,
            editor: EditorColors::from_raw(&raw.editor, &base.editor)?,
            syntax,
        })
    }

    /// The built-in dark theme resolved over `minimal`. Panics only if the
    /// embedded file is broken, which the tests catch.
    fn base_dark() -> Theme {
        let raw: RawTheme = toml::from_str(BUILTIN_THEME_SOURCES[0]).expect("built-in dark theme is valid");
        let mut t = Theme::minimal(true);
        t = Self::parse_with_base(raw, &t).expect("built-in dark theme is valid");
        t
    }

    /// The built-in light theme resolved over `minimal`.
    fn base_light() -> Theme {
        let raw: RawTheme = toml::from_str(BUILTIN_THEME_SOURCES[1]).expect("built-in light theme is valid");
        let t = Theme::minimal(false);
        Self::parse_with_base(raw, &t).expect("built-in light theme is valid")
    }

    /// A monochrome fallback with every colour defined, used as the ultimate base.
    fn minimal(dark: bool) -> Theme {
        let (bg, fg) = if dark { (Color32::from_rgb(30, 30, 30), Color32::from_rgb(220, 220, 220)) } else { (Color32::WHITE, Color32::BLACK) };
        let mid = Color32::from_rgb(128, 128, 128);
        Theme {
            name: "Minimal".into(),
            dark,
            ui: UiColors {
                background: bg,
                surface: bg,
                surface_raised: bg,
                border: mid,
                text: fg,
                text_muted: mid,
                accent: Color32::from_rgb(70, 130, 220),
                accent_text: Color32::WHITE,
                tab_active: bg,
                tab_inactive: bg,
                tab_text_active: fg,
                tab_text_inactive: mid,
                status_bar: bg,
                sidebar: bg,
                panel: bg,
                menu: bg,
                hover: mid,
                selection: Color32::from_rgb(60, 90, 140),
                warning: Color32::from_rgb(220, 180, 60),
                error: Color32::from_rgb(220, 80, 80),
                success: Color32::from_rgb(90, 190, 90),
                scrollbar: mid,
                scrollbar_hover: mid,
            },
            editor: EditorColors {
                background: bg,
                foreground: fg,
                gutter_background: bg,
                line_number: mid,
                line_number_active: fg,
                current_line: bg,
                selection: Color32::from_rgb(60, 90, 140),
                selection_inactive: mid,
                caret: fg,
                caret_secondary: fg,
                whitespace: mid,
                indent_guide: mid,
                indent_guide_active: mid,
                ruler: mid,
                bracket_match: mid,
                search_match: mid,
                search_match_current: mid,
                word_highlight: mid,
                fold_marker: mid,
                git_added: mid,
                git_modified: mid,
                git_deleted: mid,
                diagnostic_error: mid,
                diagnostic_warning: mid,
                diagnostic_info: mid,
                minimap_viewport: Color32::from_rgba_unmultiplied(128, 128, 128, 40),
            },
            syntax: HashMap::new(),
        }
    }

    /// All compiled-in themes, in presentation order.
    pub fn builtins() -> Vec<Theme> {
        // A broken built-in is logged and skipped rather than taking the app down.
        BUILTIN_THEME_SOURCES.iter().filter_map(|s| Theme::parse(s).map_err(|e| tracing::error!("built-in theme failed to parse: {e}")).ok()).collect()
    }

    /// The theme used when the configured one cannot be found.
    pub fn default_dark() -> Theme {
        Theme::base_dark()
    }

    /// Resolve a syntax scope such as `keyword.control.import` to a style by
    /// trying progressively shorter dotted prefixes.
    pub fn syntax_style(&self, scope: &str) -> Option<SyntaxStyle> {
        let mut s = scope;
        loop {
            if let Some(st) = self.syntax.get(s) {
                return Some(*st);
            }
            {
                // No more dots to strip: the scope is unknown.
                let i = s.rfind('.')?;
                s = &s[..i]
            }
        }
    }

    /// Foreground for a scope, or the plain editor foreground when unstyled.
    pub fn syntax_colour(&self, scope: &str) -> Color32 {
        self.syntax_style(scope).map(|s| s.fg).unwrap_or(self.editor.foreground)
    }

    /// Push this theme into egui's style so built-in widgets match.
    ///
    /// Starts from egui's own dark/light visuals so anything not listed here
    /// still has a sensible value, then overrides fills, strokes, rounding
    /// and spacing for every style (`all_styles_mut`) so dialogs and menus
    /// agree with the custom-painted editor.
    pub fn apply_to_egui(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        let ui = &self.ui;
        visuals.override_text_color = Some(ui.text);
        visuals.panel_fill = ui.surface;
        visuals.window_fill = ui.surface_raised;
        visuals.window_stroke = egui::Stroke::new(1.0, ui.border);
        visuals.extreme_bg_color = ui.background;
        visuals.faint_bg_color = ui.surface_raised;
        visuals.code_bg_color = ui.background;
        visuals.text_edit_bg_color = Some(ui.background);
        visuals.hyperlink_color = ui.accent;
        visuals.warn_fg_color = ui.warning;
        visuals.error_fg_color = ui.error;
        visuals.selection.bg_fill = ui.selection;
        visuals.selection.stroke = egui::Stroke::new(1.0, ui.text);
        // Widget states: noninteractive (labels, separators), inactive (idle
        // buttons), hovered, active (pressed) and open (menus/combos).
        visuals.widgets.noninteractive.bg_fill = ui.surface;
        visuals.widgets.noninteractive.weak_bg_fill = ui.surface;
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, ui.border);
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, ui.text_muted);
        visuals.widgets.inactive.bg_fill = ui.surface_raised;
        visuals.widgets.inactive.weak_bg_fill = ui.surface;
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, ui.text);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        visuals.widgets.hovered.bg_fill = ui.hover;
        visuals.widgets.hovered.weak_bg_fill = ui.hover;
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, ui.text);
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ui.border);
        visuals.widgets.active.bg_fill = ui.accent;
        visuals.widgets.active.weak_bg_fill = ui.hover;
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, ui.accent_text);
        visuals.widgets.open.bg_fill = ui.hover;
        visuals.widgets.open.weak_bg_fill = ui.hover;
        visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, ui.text);
        visuals.window_corner_radius = egui::CornerRadius::same(6);
        visuals.menu_corner_radius = egui::CornerRadius::same(6);
        // Lighter shadow on light themes, where a heavy one looks dirty.
        visuals.window_shadow =
            egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if self.dark { 120 } else { 50 }) };
        visuals.popup_shadow = visuals.window_shadow;
        visuals.text_cursor.stroke = egui::Stroke::new(1.5, ui.accent);
        for w in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            w.corner_radius = egui::CornerRadius::same(4);
        }
        ctx.set_visuals(visuals);
        ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(6.0, 4.0);
            style.spacing.button_padding = egui::vec2(8.0, 3.0);
            style.spacing.menu_margin = egui::Margin::same(6);
            style.spacing.window_margin = egui::Margin::same(10);
            style.visuals.striped = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every embedded theme parses and scope prefix lookup works.
    #[test]
    fn builtin_themes_parse() {
        let themes = Theme::builtins();
        assert_eq!(themes.len(), BUILTIN_THEME_SOURCES.len());
        let dark = &themes[0];
        assert_eq!(dark.name, "Apex Dark");
        assert!(dark.syntax_style("keyword.control.import").is_some());
        assert!(dark.syntax_style("nonexistent").is_none());
        assert!(dark.syntax_style("comment").unwrap().italic);
    }

    /// A theme listing one colour inherits the rest; bad colours and bad TOML are errors.
    #[test]
    fn partial_theme_inherits_base() {
        let t = Theme::parse("name = 'X'\ndark = true\n[editor]\nbackground = '#000000'\n").unwrap();
        assert_eq!(t.editor.background, Color32::BLACK);
        assert_eq!(t.editor.caret, Theme::default_dark().editor.caret);
        assert!(Theme::parse("name='bad'\n[ui]\ntext='red'").is_err());
        assert!(Theme::parse("not toml at all [[").is_err());
    }
}
