//! User settings with layered defaults. Stored as TOML in the config directory.
//!
//! [`Settings`] is a plain struct with a `Default` that is the documented
//! default for every key. Loading is layered: `ApexApp` starts from
//! `Settings::default()`, applies the user's `settings.toml` with
//! [`Settings::load_file`], then the `[settings]` table of the project file
//! (see `settings_ui.rs`, which also strips keys a project may not set).
//! Each layer is parsed by [`Settings::parse_layer`], which never fails as a
//! whole: unknown keys, wrong types and out-of-range values are reported as
//! [`SettingsIssue`]s and the offending key keeps its previous value.
//!
//! Gotcha: `deny_unknown_fields` on the struct is what makes the round-trip
//! test meaningful, but `parse_layer` filters unknown keys itself before
//! deserialising so that one typo does not reject the whole file.

use std::path::Path;

use apex_edit::{IndentSettings, IndentStyle};
use serde::{Deserialize, Serialize};

/// When the editor draws spaces and tabs as visible glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WhitespaceMode {
    None,
    /// Only inside the selection.
    #[default]
    Selection,
    All,
}

/// Every user-tunable value. Field names are the TOML keys; see
/// `settings_ui.rs` for the labels and help text shown in the panel.
///
/// Numeric fields have ranges enforced by `sanitise` (and mirrored by the
/// panel's widgets). Mutated at runtime by the settings panel, by toggle
/// commands (word wrap, Vim mode, ...) and by `reload_settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Name of the colour theme (matches `Theme::name`).
    pub theme: String,
    /// `"auto"` switches between the dark and light variant of `theme` with the
    /// system preference; `"dark"` or `"light"` keeps `theme` as chosen.
    pub theme_mode: String,
    /// Editor font family; `"monospace"` means the bundled font.
    pub font_family: String,
    /// Editor font size in points (6–72).
    pub font_size: f32,
    /// Font size for menus, sidebar and dialogs in points (8–40).
    pub ui_font_size: f32,
    /// Line height as a multiple of the font size (0.8–3.0).
    pub line_height: f32,
    pub ligatures: bool,
    /// egui zoom factor for the whole interface (0.5–3.0).
    pub ui_scale: f32,

    /// Columns a tab character occupies (1–16).
    pub tab_width: usize,
    /// Columns per indent level when indenting with spaces (1–16).
    pub indent_width: usize,
    pub use_tabs: bool,
    /// Guess tabs/spaces and width from each file on open.
    pub detect_indentation: bool,
    pub auto_indent: bool,
    pub auto_pair: bool,
    /// Backspace in leading whitespace removes one indent level.
    pub smart_backspace: bool,
    /// Default for new views; `ToggleWordWrap` overrides per view.
    pub word_wrap: bool,
    /// Wrap at this column; 0 wraps at the window edge.
    pub wrap_column: usize,
    pub line_numbers: bool,
    pub relative_line_numbers: bool,
    pub highlight_current_line: bool,
    pub highlight_matching_brackets: bool,
    pub highlight_word_occurrences: bool,
    pub show_whitespace: WhitespaceMode,
    pub indent_guides: bool,
    /// Columns at which vertical ruler lines are drawn.
    pub rulers: Vec<usize>,
    pub minimap: bool,
    /// Minimap width in pixels (20–400).
    pub minimap_width: f32,
    /// Caret width in pixels (1–8).
    pub caret_width: f32,
    pub caret_blink: bool,
    pub scroll_past_end: bool,
    pub smooth_scrolling: bool,
    pub scrollbars: bool,
    pub sidebar: bool,
    pub status_bar: bool,
    pub menu_bar: bool,
    pub tabs: bool,
    pub breadcrumbs: bool,

    pub trim_trailing_whitespace_on_save: bool,
    pub ensure_final_newline_on_save: bool,
    /// Run the language server's formatter before saving.
    pub format_on_save: bool,
    /// Idle time before an automatic save; 0 disables auto-save.
    pub auto_save_delay_ms: u64,
    /// Snapshot unsaved changes for crash recovery.
    pub recovery: bool,
    /// Interval between recovery snapshots.
    pub recovery_interval_ms: u64,
    /// Reopen the previous session's files on start.
    pub restore_session: bool,
    /// Encoding for new files (`"utf-8"`, `"utf-16le"`, ...).
    pub default_encoding: String,
    /// Encoding tried when a file is not valid UTF-8.
    pub fallback_encoding: String,
    /// `"auto"`, `"lf"`, `"crlf"` or `"cr"` for new files.
    pub default_line_ending: String,
    pub confirm_delete: bool,

    pub find_wrap: bool,
    pub find_case_sensitive: bool,
    pub find_whole_word: bool,
    pub find_regex: bool,

    /// Globs hidden from the explorer, Goto Anything and Find in Files.
    pub exclude_patterns: Vec<String>,
    /// Globs opened in the hex preview rather than as text.
    pub binary_patterns: Vec<String>,
    pub follow_symlinks: bool,
    pub show_hidden_files: bool,

    /// No animations or smooth scrolling.
    pub reduced_motion: bool,
    /// Muted highlighting and quieter notifications.
    pub low_distraction: bool,
    pub muted_syntax: bool,
    /// Text column width, in characters, in distraction-free mode (20–300).
    pub distraction_free_width: f32,
    /// Keep the caret line vertically centred.
    pub typewriter_scrolling: bool,
    pub vim_mode: bool,
    pub spell_check: bool,
    /// Hunspell dictionary code such as `en_GB`.
    pub spell_language: String,
    pub spell_in_comments: bool,
    pub spell_in_strings: bool,
    pub lsp_enabled: bool,
    /// Language id → server command (argv). Known servers found on PATH are used automatically.
    pub lsp_servers: std::collections::BTreeMap<String, Vec<String>>,
    /// Files larger than this (MB) open in reduced-feature mode.
    pub large_file_mb: usize,
    pub git_gutter: bool,
    /// Use the system's file dialogs by default; false selects an in-app path prompt.
    pub native_file_dialogs: bool,
    /// Source Control in the lower part of the sidebar.
    pub git_sidebar: bool,
    /// Dim author/date/summary after the caret line.
    pub git_blame_inline: bool,
    /// `ff-only`, `rebase` or `merge`.
    pub git_pull_mode: String,
    /// Fetch in the background every N minutes (0 = never).
    pub git_auto_fetch_minutes: u64,
    /// Show the completion popup while typing.
    pub completion: bool,
    /// Delay after typing before the completion popup opens.
    pub completion_delay_ms: u64,
}

impl Default for Settings {
    /// The documented defaults; also what `Reset all to defaults` restores
    /// and what `write_setting` compares against to decide what to persist.
    fn default() -> Self {
        Self {
            theme: "Apex Dark".into(),
            theme_mode: "dark".into(),
            font_family: "monospace".into(),
            font_size: 14.0,
            ui_font_size: 13.0,
            line_height: 1.35,
            ligatures: true,
            ui_scale: 1.0,
            tab_width: 4,
            indent_width: 4,
            use_tabs: false,
            detect_indentation: true,
            auto_indent: true,
            auto_pair: true,
            smart_backspace: true,
            word_wrap: false,
            wrap_column: 0,
            line_numbers: true,
            relative_line_numbers: false,
            highlight_current_line: true,
            highlight_matching_brackets: true,
            highlight_word_occurrences: true,
            show_whitespace: WhitespaceMode::Selection,
            indent_guides: true,
            rulers: Vec::new(),
            minimap: true,
            minimap_width: 90.0,
            caret_width: 2.0,
            caret_blink: true,
            scroll_past_end: true,
            smooth_scrolling: true,
            scrollbars: true,
            sidebar: true,
            status_bar: true,
            menu_bar: true,
            tabs: true,
            breadcrumbs: false,
            trim_trailing_whitespace_on_save: false,
            ensure_final_newline_on_save: false,
            format_on_save: false,
            auto_save_delay_ms: 0,
            recovery: true,
            recovery_interval_ms: 5000,
            restore_session: true,
            default_encoding: "utf-8".into(),
            fallback_encoding: "windows-1252".into(),
            default_line_ending: "auto".into(),
            confirm_delete: true,
            find_wrap: true,
            find_case_sensitive: false,
            find_whole_word: false,
            find_regex: false,
            exclude_patterns: vec!["**/.git".into(), "**/node_modules".into(), "**/target".into(), "**/*.o".into(), "**/*.pyc".into()],
            binary_patterns: vec![
                "*.png".into(),
                "*.jpg".into(),
                "*.gif".into(),
                "*.zip".into(),
                "*.pdf".into(),
                "*.exe".into(),
                "*.dll".into(),
                "*.so".into(),
            ],
            follow_symlinks: false,
            show_hidden_files: false,
            reduced_motion: false,
            low_distraction: false,
            muted_syntax: false,
            distraction_free_width: 90.0,
            typewriter_scrolling: false,
            vim_mode: false,
            spell_check: false,
            spell_language: "en_GB".into(),
            spell_in_comments: true,
            spell_in_strings: false,
            lsp_enabled: true,
            lsp_servers: std::collections::BTreeMap::new(),
            large_file_mb: 64,
            git_gutter: true,
            native_file_dialogs: true,
            git_sidebar: true,
            git_blame_inline: false,
            git_pull_mode: "ff-only".into(),
            git_auto_fetch_minutes: 0,
            completion: true,
            completion_delay_ms: 150,
        }
    }
}

/// A non-fatal problem found while loading settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsIssue {
    /// Where it came from: a file path or a caller-supplied label.
    pub source: String,
    pub message: String,
}

impl Settings {
    /// The indentation configuration documents start with, clamped to the
    /// ranges the editor supports.
    pub fn indent(&self) -> IndentSettings {
        IndentSettings {
            style: if self.use_tabs { IndentStyle::Tabs } else { IndentStyle::Spaces },
            indent_width: self.indent_width.clamp(1, 16),
            tab_width: self.tab_width.clamp(1, 16),
        }
    }

    /// Parse TOML on top of `base`. Unknown keys are reported as issues; the
    /// whole document is rejected only if it is not valid TOML.
    ///
    /// `source` labels the issues (normally the file path). Returns the
    /// merged settings and every issue found, in file order.
    pub fn parse_layer(base: &Settings, src: &str, source: &str) -> (Settings, Vec<SettingsIssue>) {
        let mut issues = Vec::new();
        let table: toml::Table = match toml::from_str(src) {
            Ok(t) => t,
            Err(e) => {
                issues.push(SettingsIssue { source: source.into(), message: format!("not valid TOML: {}", first_line(&e.to_string())) });
                return (base.clone(), issues);
            }
        };
        // Serialise base, overlay keys, deserialise. Keeps defaults for missing keys.
        let mut merged = toml::Value::try_from(base).expect("settings serialise");
        let known: toml::Table = match &merged {
            toml::Value::Table(t) => t.clone(),
            _ => toml::Table::new(),
        };
        if let toml::Value::Table(m) = &mut merged {
            for (k, v) in table {
                if !known.contains_key(&k) {
                    issues.push(SettingsIssue { source: source.into(), message: format!("unknown setting `{k}` ignored") });
                    continue;
                }
                // Type check against the base value's TOML type. An integer
                // where a float is expected is accepted (`font_size = 14`),
                // since TOML users rarely write `14.0`.
                let expected = &known[&k];
                if std::mem::discriminant(expected) != std::mem::discriminant(&v) && !(expected.is_float() && v.is_integer()) {
                    issues.push(SettingsIssue {
                        source: source.into(),
                        message: format!("setting `{k}` has the wrong type (expected {}), ignored", expected.type_str()),
                    });
                    continue;
                }
                let v = if expected.is_float() && v.is_integer() { toml::Value::Float(v.as_integer().unwrap_or(0) as f64) } else { v };
                m.insert(k, v);
            }
        }
        match merged.try_into::<Settings>() {
            Ok(mut s) => {
                s.sanitise(&mut issues, source);
                (s, issues)
            }
            Err(e) => {
                // Only reachable for a nested value of the right outer type
                // but wrong shape (e.g. a non-string in `rulers`).
                issues.push(SettingsIssue { source: source.into(), message: format!("could not apply settings: {e}") });
                (base.clone(), issues)
            }
        }
    }

    /// Clamp numeric fields into their supported ranges, recording an issue
    /// for each one that had to change. Non-finite floats become the minimum.
    fn sanitise(&mut self, issues: &mut Vec<SettingsIssue>, source: &str) {
        let mut clamp = |name: &str, v: &mut f32, lo: f32, hi: f32| {
            if !v.is_finite() || *v < lo || *v > hi {
                issues.push(SettingsIssue { source: source.into(), message: format!("`{name}` must be between {lo} and {hi}; using {}", v.clamp(lo, hi)) });
                *v = if v.is_finite() { v.clamp(lo, hi) } else { lo };
            }
        };
        clamp("font_size", &mut self.font_size, 6.0, 72.0);
        clamp("ui_font_size", &mut self.ui_font_size, 8.0, 40.0);
        clamp("line_height", &mut self.line_height, 0.8, 3.0);
        clamp("ui_scale", &mut self.ui_scale, 0.5, 3.0);
        clamp("caret_width", &mut self.caret_width, 1.0, 8.0);
        clamp("minimap_width", &mut self.minimap_width, 20.0, 400.0);
        clamp("distraction_free_width", &mut self.distraction_free_width, 20.0, 300.0);
        if self.tab_width == 0 || self.tab_width > 16 {
            issues.push(SettingsIssue { source: source.into(), message: "`tab_width` must be 1–16".into() });
            self.tab_width = self.tab_width.clamp(1, 16);
        }
        if self.indent_width == 0 || self.indent_width > 16 {
            issues.push(SettingsIssue { source: source.into(), message: "`indent_width` must be 1–16".into() });
            self.indent_width = self.indent_width.clamp(1, 16);
        }
    }

    /// Read `path` and layer it over `base`. A missing file is not an issue
    /// (first run); any other read error is reported and `base` returned.
    pub fn load_file(base: &Settings, path: &Path) -> (Settings, Vec<SettingsIssue>) {
        match std::fs::read_to_string(path) {
            Ok(src) => Self::parse_layer(base, &src, &path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (base.clone(), Vec::new()),
            Err(e) => (base.clone(), vec![SettingsIssue { source: path.display().to_string(), message: e.to_string() }]),
        }
    }

    /// Serialise with every key present, as a starting point for user editing.
    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }
}

/// First line of a multi-line error message, for the status bar.
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_dialogs_default_on_and_respect_config_override() {
        let defaults = Settings::default();
        assert!(defaults.native_file_dialogs);
        let (missing, issues) = Settings::parse_layer(&defaults, "font_size = 16", "test");
        assert!(issues.is_empty());
        assert!(missing.native_file_dialogs);
        let (override_settings, issues) = Settings::parse_layer(&defaults, "native_file_dialogs = false", "test");
        assert!(issues.is_empty());
        assert!(!override_settings.native_file_dialogs);
        let (layered, issues) = Settings::parse_layer(&override_settings, "font_size = 18", "test");
        assert!(issues.is_empty());
        assert!(!layered.native_file_dialogs);
    }

    /// Unknown keys, wrong types, out-of-range values and invalid TOML each
    /// produce an issue without losing the other settings.
    #[test]
    fn layering_and_diagnostics() {
        let base = Settings::default();
        let (s, issues) = Settings::parse_layer(&base, "font_size = 18\nnope = 1\ntab_width = 'x'\n", "test");
        assert_eq!(s.font_size, 18.0);
        assert_eq!(s.tab_width, 4);
        assert_eq!(issues.len(), 2);
        assert!(issues[0].message.contains("unknown setting"));
        assert!(issues[1].message.contains("wrong type"));
        let (s, issues) = Settings::parse_layer(&base, "font_size = 1000", "test");
        assert_eq!(s.font_size, 72.0);
        assert_eq!(issues.len(), 1);
        let (s, issues) = Settings::parse_layer(&base, "= = =", "test");
        assert_eq!(s, base);
        assert_eq!(issues.len(), 1);
    }

    /// `to_toml_string` output parses back to an identical struct.
    #[test]
    fn round_trips_through_toml() {
        let s = Settings::default();
        let txt = s.to_toml_string();
        let (back, issues) = Settings::parse_layer(&Settings { font_size: 1.0, ..Default::default() }, &txt, "rt");
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(back, s);
    }
}
