//! User settings with layered defaults. Stored as TOML in the config directory.

use std::path::Path;

use apex_edit::{IndentSettings, IndentStyle};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WhitespaceMode {
    None,
    #[default]
    Selection,
    All,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub theme: String,
    /// `"auto"` switches between the dark and light variant of `theme` with the
    /// system preference; `"dark"` or `"light"` keeps `theme` as chosen.
    pub theme_mode: String,
    pub font_family: String,
    pub font_size: f32,
    pub ui_font_size: f32,
    pub line_height: f32,
    pub ligatures: bool,
    pub ui_scale: f32,

    pub tab_width: usize,
    pub indent_width: usize,
    pub use_tabs: bool,
    pub detect_indentation: bool,
    pub auto_indent: bool,
    pub auto_pair: bool,
    pub smart_backspace: bool,
    pub word_wrap: bool,
    pub wrap_column: usize,
    pub line_numbers: bool,
    pub relative_line_numbers: bool,
    pub highlight_current_line: bool,
    pub highlight_matching_brackets: bool,
    pub highlight_word_occurrences: bool,
    pub show_whitespace: WhitespaceMode,
    pub indent_guides: bool,
    pub rulers: Vec<usize>,
    pub minimap: bool,
    pub minimap_width: f32,
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
    pub format_on_save: bool,
    pub auto_save_delay_ms: u64,
    pub recovery: bool,
    pub recovery_interval_ms: u64,
    pub restore_session: bool,
    pub default_encoding: String,
    pub fallback_encoding: String,
    /// `"auto"`, `"lf"`, `"crlf"` or `"cr"` for new files.
    pub default_line_ending: String,
    pub confirm_delete: bool,

    pub find_wrap: bool,
    pub find_case_sensitive: bool,
    pub find_whole_word: bool,
    pub find_regex: bool,

    pub exclude_patterns: Vec<String>,
    pub binary_patterns: Vec<String>,
    pub follow_symlinks: bool,
    pub show_hidden_files: bool,

    pub reduced_motion: bool,
    pub low_distraction: bool,
    pub muted_syntax: bool,
    pub distraction_free_width: f32,
    pub typewriter_scrolling: bool,
    pub vim_mode: bool,
    pub spell_check: bool,
    pub spell_language: String,
    pub spell_in_comments: bool,
    pub spell_in_strings: bool,
    pub lsp_enabled: bool,
    /// Language id → server command (argv). Known servers found on PATH are used automatically.
    pub lsp_servers: std::collections::BTreeMap<String, Vec<String>>,
    pub large_file_mb: usize,
    pub git_gutter: bool,
    pub completion: bool,
    pub completion_delay_ms: u64,
}

impl Default for Settings {
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
            completion: true,
            completion_delay_ms: 150,
        }
    }
}

/// A non-fatal problem found while loading settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsIssue {
    pub source: String,
    pub message: String,
}

impl Settings {
    pub fn indent(&self) -> IndentSettings {
        IndentSettings {
            style: if self.use_tabs { IndentStyle::Tabs } else { IndentStyle::Spaces },
            indent_width: self.indent_width.clamp(1, 16),
            tab_width: self.tab_width.clamp(1, 16),
        }
    }

    /// Parse TOML on top of `base`. Unknown keys are reported as issues; the
    /// whole document is rejected only if it is not valid TOML.
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
                issues.push(SettingsIssue { source: source.into(), message: format!("could not apply settings: {e}") });
                (base.clone(), issues)
            }
        }
    }

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

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn round_trips_through_toml() {
        let s = Settings::default();
        let txt = s.to_toml_string();
        let (back, issues) = Settings::parse_layer(&Settings { font_size: 1.0, ..Default::default() }, &txt, "rt");
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(back, s);
    }
}
