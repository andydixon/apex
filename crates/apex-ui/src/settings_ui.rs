//! The searchable settings editor. Every entry maps to one key of
//! `Settings`; changes apply immediately and are written to the user
//! `settings.toml` (only keys that differ from the defaults are stored, and
//! unrelated keys in the file are kept).

use std::path::PathBuf;

use apex_project::fileio;

use crate::app::ApexApp;
use crate::command::Command;
use crate::settings::{Settings, WhitespaceMode};

#[derive(Default)]
pub struct SettingsPanel {
    pub open: bool,
    pub query: String,
    pub focus_query: bool,
    /// Set when settings.toml was saved from the editor; reloaded on the next frame.
    pub reload_pending: bool,
}

enum Kind {
    Bool,
    Int(usize, usize),
    U64(u64, u64),
    Float(f32, f32),
    Text,
    Choice(&'static [&'static str]),
    Theme,
    List,
    IntList,
    Whitespace,
}

struct Entry {
    key: &'static str,
    category: &'static str,
    label: &'static str,
    help: &'static str,
    kind: Kind,
}

const ENTRIES: &[Entry] = &[
    Entry { key: "theme", category: "Appearance", label: "Theme", help: "Colour theme; user themes live in <config>/themes.", kind: Kind::Theme },
    Entry {
        key: "theme_mode",
        category: "Appearance",
        label: "Theme mode",
        help: "auto follows the system; dark or light picks the fallback.",
        kind: Kind::Choice(&["auto", "dark", "light"]),
    },
    Entry { key: "font_family", category: "Appearance", label: "Editor font", help: "Family name, or monospace for the bundled font.", kind: Kind::Text },
    Entry { key: "font_size", category: "Appearance", label: "Editor font size", help: "Points.", kind: Kind::Float(6.0, 72.0) },
    Entry { key: "ui_font_size", category: "Appearance", label: "Interface font size", help: "Menus, sidebar and dialogs.", kind: Kind::Float(8.0, 40.0) },
    Entry { key: "line_height", category: "Appearance", label: "Line height", help: "Multiple of the font size.", kind: Kind::Float(0.8, 3.0) },
    Entry { key: "ligatures", category: "Appearance", label: "Ligatures", help: "Use the font's ligatures when it has them.", kind: Kind::Bool },
    Entry {
        key: "ui_scale",
        category: "Appearance",
        label: "Interface scale",
        help: "Scales everything; 1.0 is the system size.",
        kind: Kind::Float(0.5, 3.0),
    },
    Entry { key: "caret_width", category: "Appearance", label: "Caret width", help: "Pixels.", kind: Kind::Float(1.0, 8.0) },
    Entry { key: "caret_blink", category: "Appearance", label: "Caret blink", help: "", kind: Kind::Bool },
    Entry { key: "tab_width", category: "Editing", label: "Tab width", help: "Columns a tab character occupies.", kind: Kind::Int(1, 16) },
    Entry {
        key: "indent_width",
        category: "Editing",
        label: "Indent width",
        help: "Columns per indent level when indenting with spaces.",
        kind: Kind::Int(1, 16),
    },
    Entry { key: "use_tabs", category: "Editing", label: "Indent with tabs", help: "", kind: Kind::Bool },
    Entry {
        key: "detect_indentation",
        category: "Editing",
        label: "Detect indentation",
        help: "Guess tabs/spaces and width from each file.",
        kind: Kind::Bool,
    },
    Entry { key: "auto_indent", category: "Editing", label: "Auto indent", help: "Keep the indentation of the previous line on Enter.", kind: Kind::Bool },
    Entry { key: "auto_pair", category: "Editing", label: "Auto-pair brackets and quotes", help: "", kind: Kind::Bool },
    Entry {
        key: "smart_backspace",
        category: "Editing",
        label: "Smart backspace",
        help: "Backspace removes one indent level in leading whitespace.",
        kind: Kind::Bool,
    },
    Entry {
        key: "word_wrap",
        category: "Editing",
        label: "Word wrap",
        help: "Default for new views; View > Toggle Word Wrap overrides per view.",
        kind: Kind::Bool,
    },
    Entry { key: "wrap_column", category: "Editing", label: "Wrap column", help: "0 wraps at the window edge.", kind: Kind::Int(0, 500) },
    Entry {
        key: "completion",
        category: "Editing",
        label: "Completion popup",
        help: "Word, snippet and language-server completions while typing.",
        kind: Kind::Bool,
    },
    Entry {
        key: "completion_delay_ms",
        category: "Editing",
        label: "Completion delay",
        help: "Milliseconds after typing before the popup opens.",
        kind: Kind::U64(0, 2000),
    },
    Entry { key: "vim_mode", category: "Editing", label: "Vim mode", help: "Modal editing; see the user guide for the supported subset.", kind: Kind::Bool },
    Entry { key: "line_numbers", category: "Display", label: "Line numbers", help: "", kind: Kind::Bool },
    Entry { key: "relative_line_numbers", category: "Display", label: "Relative line numbers", help: "", kind: Kind::Bool },
    Entry { key: "highlight_current_line", category: "Display", label: "Highlight current line", help: "", kind: Kind::Bool },
    Entry { key: "highlight_matching_brackets", category: "Display", label: "Highlight matching brackets", help: "", kind: Kind::Bool },
    Entry { key: "highlight_word_occurrences", category: "Display", label: "Highlight other occurrences of the selected word", help: "", kind: Kind::Bool },
    Entry { key: "show_whitespace", category: "Display", label: "Show whitespace", help: "", kind: Kind::Whitespace },
    Entry { key: "indent_guides", category: "Display", label: "Indent guides", help: "", kind: Kind::Bool },
    Entry { key: "rulers", category: "Display", label: "Rulers", help: "Columns, comma separated (e.g. 80, 120).", kind: Kind::IntList },
    Entry { key: "minimap", category: "Display", label: "Minimap", help: "", kind: Kind::Bool },
    Entry { key: "minimap_width", category: "Display", label: "Minimap width", help: "Pixels.", kind: Kind::Float(20.0, 400.0) },
    Entry { key: "scroll_past_end", category: "Display", label: "Scroll past end", help: "", kind: Kind::Bool },
    Entry { key: "smooth_scrolling", category: "Display", label: "Smooth scrolling", help: "", kind: Kind::Bool },
    Entry { key: "scrollbars", category: "Display", label: "Scrollbars", help: "", kind: Kind::Bool },
    Entry { key: "sidebar", category: "Display", label: "Sidebar", help: "", kind: Kind::Bool },
    Entry { key: "status_bar", category: "Display", label: "Status bar", help: "", kind: Kind::Bool },
    Entry { key: "menu_bar", category: "Display", label: "Menu bar", help: "Alt shows it temporarily when hidden.", kind: Kind::Bool },
    Entry { key: "tabs", category: "Display", label: "Tab bar", help: "", kind: Kind::Bool },
    Entry { key: "breadcrumbs", category: "Display", label: "Breadcrumbs", help: "", kind: Kind::Bool },
    Entry { key: "git_gutter", category: "Display", label: "Git gutter marks", help: "", kind: Kind::Bool },
    Entry { key: "trim_trailing_whitespace_on_save", category: "Files", label: "Trim trailing whitespace on save", help: "", kind: Kind::Bool },
    Entry { key: "ensure_final_newline_on_save", category: "Files", label: "Ensure final newline on save", help: "", kind: Kind::Bool },
    Entry {
        key: "format_on_save",
        category: "Files",
        label: "Format on save",
        help: "Uses the language server's formatter when one is running.",
        kind: Kind::Bool,
    },
    Entry {
        key: "auto_save_delay_ms",
        category: "Files",
        label: "Auto-save delay",
        help: "Milliseconds of inactivity before saving; 0 disables auto-save.",
        kind: Kind::U64(0, 600_000),
    },
    Entry {
        key: "recovery",
        category: "Files",
        label: "Crash recovery snapshots",
        help: "Unsaved changes are snapshotted in the data directory.",
        kind: Kind::Bool,
    },
    Entry {
        key: "recovery_interval_ms",
        category: "Files",
        label: "Recovery interval",
        help: "Milliseconds between snapshots.",
        kind: Kind::U64(1000, 600_000),
    },
    Entry { key: "restore_session", category: "Files", label: "Restore session on start", help: "", kind: Kind::Bool },
    Entry {
        key: "default_encoding",
        category: "Files",
        label: "Default encoding",
        help: "For new files.",
        kind: Kind::Choice(&["utf-8", "utf-8-bom", "utf-16le", "utf-16be", "windows-1252", "iso-8859-1"]),
    },
    Entry {
        key: "fallback_encoding",
        category: "Files",
        label: "Fallback encoding",
        help: "Used when a file is not valid UTF-8.",
        kind: Kind::Choice(&["windows-1252", "iso-8859-1", "iso-8859-15", "windows-1251", "shift_jis", "gbk", "euc-kr"]),
    },
    Entry {
        key: "default_line_ending",
        category: "Files",
        label: "Default line ending",
        help: "auto uses the platform convention.",
        kind: Kind::Choice(&["auto", "lf", "crlf", "cr"]),
    },
    Entry { key: "confirm_delete", category: "Files", label: "Confirm before deleting files", help: "", kind: Kind::Bool },
    Entry {
        key: "large_file_mb",
        category: "Files",
        label: "Large-file threshold",
        help: "Megabytes; larger files open in reduced-feature mode.",
        kind: Kind::Int(1, 4096),
    },
    Entry { key: "find_wrap", category: "Search", label: "Wrap around", help: "", kind: Kind::Bool },
    Entry { key: "find_case_sensitive", category: "Search", label: "Case sensitive by default", help: "", kind: Kind::Bool },
    Entry { key: "find_whole_word", category: "Search", label: "Whole word by default", help: "", kind: Kind::Bool },
    Entry { key: "find_regex", category: "Search", label: "Regular expressions by default", help: "", kind: Kind::Bool },
    Entry {
        key: "exclude_patterns",
        category: "Projects",
        label: "Exclude patterns",
        help: "Glob patterns hidden from the explorer, Goto Anything and Find in Files; comma separated.",
        kind: Kind::List,
    },
    Entry {
        key: "binary_patterns",
        category: "Projects",
        label: "Binary patterns",
        help: "Globs opened in the hex preview; comma separated.",
        kind: Kind::List,
    },
    Entry { key: "follow_symlinks", category: "Projects", label: "Follow symbolic links", help: "", kind: Kind::Bool },
    Entry { key: "show_hidden_files", category: "Projects", label: "Show hidden files", help: "", kind: Kind::Bool },
    Entry { key: "reduced_motion", category: "Focus and accessibility", label: "Reduced motion", help: "No animations or smooth scrolling.", kind: Kind::Bool },
    Entry {
        key: "low_distraction",
        category: "Focus and accessibility",
        label: "Low-distraction mode",
        help: "Muted highlighting and quieter notifications.",
        kind: Kind::Bool,
    },
    Entry { key: "muted_syntax", category: "Focus and accessibility", label: "Muted syntax colours", help: "", kind: Kind::Bool },
    Entry {
        key: "distraction_free_width",
        category: "Focus and accessibility",
        label: "Distraction-free width",
        help: "Columns of text in distraction-free mode.",
        kind: Kind::Float(20.0, 300.0),
    },
    Entry {
        key: "typewriter_scrolling",
        category: "Focus and accessibility",
        label: "Typewriter scrolling",
        help: "Keep the caret line vertically centred.",
        kind: Kind::Bool,
    },
    Entry {
        key: "spell_check",
        category: "Spelling",
        label: "Spell check",
        help: "Needs a dictionary; see Tools > Toggle Spell Check for the search path.",
        kind: Kind::Bool,
    },
    Entry { key: "spell_language", category: "Spelling", label: "Dictionary language", help: "Hunspell code such as en_GB.", kind: Kind::Text },
    Entry { key: "spell_in_comments", category: "Spelling", label: "Check comments in code", help: "", kind: Kind::Bool },
    Entry { key: "spell_in_strings", category: "Spelling", label: "Check strings in code", help: "", kind: Kind::Bool },
    Entry {
        key: "lsp_enabled",
        category: "Language servers",
        label: "Language servers",
        help: "Start known servers found on PATH; configure others under lsp_servers in settings.toml.",
        kind: Kind::Bool,
    },
];

impl ApexApp {
    pub(crate) fn settings_path(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    pub(crate) fn settings_command(&mut self, ctx: &egui::Context, cmd: Command) -> bool {
        match cmd {
            Command::Settings => {
                self.settings_panel.open = !self.settings_panel.open;
                self.settings_panel.focus_query = self.settings_panel.open;
            }
            Command::OpenSettingsFile => {
                let p = self.settings_path();
                if !p.exists() {
                    let _ = apex_project::paths::ensure_private_dir(&self.config_dir);
                    let body = "# Apex settings. Keys not listed here keep their defaults;\n# Tools > Settings shows every key with its description.\n\n";
                    if let Err(e) = fileio::save_atomic(&p, body.as_bytes()) {
                        self.set_status(format!("Could not create settings file: {e}"), true);
                        return true;
                    }
                }
                self.open_path(&p);
            }
            _ => return false,
        }
        let _ = ctx;
        true
    }

    /// Re-read settings.toml (and the project layer) and apply the result.
    pub(crate) fn reload_settings(&mut self, ctx: &egui::Context) {
        let (mut s, mut issues) = Settings::load_file(&Settings::default(), &self.settings_path());
        if let Some(p) = self.project_file.clone()
            && let Ok(src) = std::fs::read_to_string(&p)
            && let Ok(table) = toml::from_str::<toml::Table>(&src)
            && let Some(ps) = table.get("settings").and_then(|v| v.as_table())
        {
            let (merged, more) = Settings::parse_layer(&s, &toml::to_string(ps).unwrap_or_default(), &p.display().to_string());
            s = merged;
            issues.extend(more);
        }
        let old = std::mem::replace(&mut self.settings, s);
        self.settings_issues = issues.clone();
        self.apply_settings_delta(ctx, &old);
        if issues.is_empty() {
            self.set_status("Settings reloaded".into(), false);
        } else {
            self.set_status(format!("Settings reloaded with {} issue(s): {}", issues.len(), issues[0].message), true);
        }
    }

    /// Apply whatever changed between `old` and the current settings.
    fn apply_settings_delta(&mut self, ctx: &egui::Context, old: &Settings) {
        let s = &self.settings;
        if s.theme != old.theme {
            self.theme = Self::pick_theme(&self.themes, &s.theme, &self.config_dir);
            self.theme.apply_to_egui(ctx);
        }
        if s.ui_scale != old.ui_scale {
            ctx.set_zoom_factor(s.ui_scale);
        }
        if s.font_family != old.font_family || s.ui_font_size != old.ui_font_size || s.ligatures != old.ligatures {
            self.fonts_applied = false;
        }
        if s.vim_mode != old.vim_mode {
            self.vim = crate::vim::Vim::new();
        }
        self.invalidate_all_layouts();
    }

    /// Persist one key of the current settings to the user file.
    fn write_setting(&mut self, key: &str) {
        let path = self.settings_path();
        let mut table: toml::Table = match std::fs::read_to_string(&path) {
            Ok(src) => match toml::from_str(&src) {
                Ok(t) => t,
                Err(e) => {
                    let first = e.to_string().lines().next().unwrap_or("").to_string();
                    self.set_status(format!("settings.toml is not valid TOML ({first}); fix it before changing settings here"), true);
                    return;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => {
                self.set_status(format!("Could not read settings.toml: {e}"), true);
                return;
            }
        };
        let current = toml::Value::try_from(&self.settings).ok().and_then(|v| v.get(key).cloned());
        let default = toml::Value::try_from(Settings::default()).ok().and_then(|v| v.get(key).cloned());
        match current {
            Some(v) if Some(&v) != default.as_ref() => {
                table.insert(key.to_string(), v);
            }
            _ => {
                table.remove(key);
            }
        }
        let body = format!(
            "# Apex settings. Keys not listed here keep their defaults;\n# Tools > Settings shows every key with its description.\n\n{}",
            toml::to_string_pretty(&table).unwrap_or_default()
        );
        let _ = apex_project::paths::ensure_private_dir(&self.config_dir);
        if let Err(e) = fileio::save_atomic(&path, body.as_bytes()) {
            self.set_status(format!("Could not save settings: {e}"), true);
        }
    }

    pub(crate) fn show_settings_panel(&mut self, ctx: &egui::Context) {
        if self.settings_panel.reload_pending {
            self.settings_panel.reload_pending = false;
            self.reload_settings(ctx);
        }
        if !self.settings_panel.open {
            return;
        }
        let mut open = true;
        let mut changed_key: Option<&'static str> = None;
        let old = self.settings.clone();
        let theme_names: Vec<String> = self.themes.iter().map(|t| t.name.clone()).collect();
        let mut open_file = false;
        let mut reset_all = false;
        let query = self.settings_panel.query.to_lowercase();
        let focus_query = std::mem::take(&mut self.settings_panel.focus_query);
        egui::Window::new("Settings").id(egui::Id::new("apex-settings")).open(&mut open).default_size([640.0, 520.0]).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.settings_panel.query).hint_text("Search settings").desired_width(260.0));
                if focus_query {
                    r.request_focus();
                }
                if ui.button("Open settings.toml").on_hover_text(self.settings_path().display().to_string()).clicked() {
                    open_file = true;
                }
                if ui.button("Reset all to defaults").clicked() {
                    reset_all = true;
                }
            });
            ui.small("Changes apply immediately and are saved to settings.toml. Project files can override them.");
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let mut category = "";
                let mut shown = 0;
                for e in ENTRIES {
                    let hay = format!("{} {} {} {}", e.label, e.key, e.help, e.category).to_lowercase();
                    if !query.is_empty() && !query.split_whitespace().all(|w| hay.contains(w)) {
                        continue;
                    }
                    shown += 1;
                    if e.category != category {
                        category = e.category;
                        ui.add_space(8.0);
                        ui.heading(category);
                    }
                    let changed = setting_row(ui, &mut self.settings, e, &theme_names);
                    if changed {
                        changed_key = Some(e.key);
                    }
                }
                if shown == 0 {
                    ui.label("No settings match.");
                }
            });
        });
        self.settings_panel.open = open;
        if open_file {
            self.execute(ctx, Command::OpenSettingsFile, None);
        }
        if reset_all {
            let defaults = Settings::default();
            let old = std::mem::replace(&mut self.settings, defaults);
            // Write every key (removes them all from the file), then apply.
            for e in ENTRIES {
                self.write_setting(e.key);
            }
            self.apply_settings_delta(ctx, &old);
            self.set_status("Settings reset to defaults".into(), false);
            return;
        }
        if let Some(key) = changed_key {
            // Keep the file and runtime in step; numeric fields are clamped by the widgets.
            if key == "ui_scale" {
                let v = self.settings.ui_scale;
                self.set_ui_scale(ctx, v);
            }
            self.write_setting(key);
            self.apply_settings_delta(ctx, &old);
        }
    }
}

/// One settings row. Returns true when the value changed this frame.
fn setting_row(ui: &mut egui::Ui, s: &mut Settings, e: &Entry, themes: &[String]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.set_min_height(22.0);
        let label = ui.label(e.label);
        if !e.help.is_empty() {
            label.on_hover_text(e.help);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match &e.kind {
                Kind::Bool => {
                    if let Some(v) = bool_field(s, e.key) {
                        changed |= ui.checkbox(v, "").changed();
                    }
                }
                Kind::Int(lo, hi) => {
                    if let Some(v) = usize_field(s, e.key) {
                        changed |= ui.add(egui::DragValue::new(v).range(*lo..=*hi)).changed();
                    }
                }
                Kind::U64(lo, hi) => {
                    if let Some(v) = u64_field(s, e.key) {
                        changed |= ui.add(egui::DragValue::new(v).range(*lo..=*hi).speed(10.0)).changed();
                    }
                }
                Kind::Float(lo, hi) => {
                    if let Some(v) = f32_field(s, e.key) {
                        // Drag values commit continuously; `lost_focus` would miss drags, so accept each change.
                        changed |= ui.add(egui::DragValue::new(v).range(*lo..=*hi).speed(0.05).max_decimals(2)).changed();
                    }
                }
                Kind::Text => {
                    if let Some(v) = string_field(s, e.key) {
                        let r = ui.add(egui::TextEdit::singleline(v).desired_width(200.0));
                        changed |= r.lost_focus() && r.changed() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    }
                }
                Kind::Choice(options) => {
                    if let Some(v) = string_field(s, e.key) {
                        egui::ComboBox::from_id_salt(e.key).selected_text(v.clone()).show_ui(ui, |ui| {
                            for o in options.iter() {
                                if ui.selectable_label(v == o, *o).clicked() && v != o {
                                    *v = o.to_string();
                                    changed = true;
                                }
                            }
                        });
                    }
                }
                Kind::Theme => {
                    egui::ComboBox::from_id_salt("theme").selected_text(s.theme.clone()).show_ui(ui, |ui| {
                        for name in themes {
                            if ui.selectable_label(&s.theme == name, name).clicked() && &s.theme != name {
                                s.theme = name.clone();
                                changed = true;
                            }
                        }
                    });
                }
                Kind::Whitespace => {
                    let label = |m: WhitespaceMode| match m {
                        WhitespaceMode::None => "never",
                        WhitespaceMode::Selection => "in selection",
                        WhitespaceMode::All => "always",
                    };
                    egui::ComboBox::from_id_salt("show_whitespace").selected_text(label(s.show_whitespace)).show_ui(ui, |ui| {
                        for m in [WhitespaceMode::None, WhitespaceMode::Selection, WhitespaceMode::All] {
                            if ui.selectable_label(s.show_whitespace == m, label(m)).clicked() && s.show_whitespace != m {
                                s.show_whitespace = m;
                                changed = true;
                            }
                        }
                    });
                }
                Kind::List => {
                    if let Some(v) = string_list_field(s, e.key) {
                        let id = egui::Id::new(("settings-list", e.key));
                        let mut text: String = ui.memory(|m| m.data.get_temp::<String>(id)).unwrap_or_else(|| v.join(", "));
                        let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(320.0));
                        if r.lost_focus() {
                            let parsed: Vec<String> = text.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
                            if parsed != *v {
                                *v = parsed;
                                changed = true;
                            }
                            ui.memory_mut(|m| m.data.remove::<String>(id));
                        } else if r.has_focus() {
                            ui.memory_mut(|m| m.data.insert_temp(id, text));
                        }
                    }
                }
                Kind::IntList => {
                    let id = egui::Id::new(("settings-intlist", e.key));
                    let mut text: String =
                        ui.memory(|m| m.data.get_temp::<String>(id)).unwrap_or_else(|| s.rulers.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", "));
                    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(200.0));
                    if r.lost_focus() {
                        let parsed: Vec<usize> = text.split(',').filter_map(|p| p.trim().parse::<usize>().ok()).filter(|n| *n > 0 && *n < 1000).collect();
                        if parsed != s.rulers {
                            s.rulers = parsed;
                            changed = true;
                        }
                        ui.memory_mut(|m| m.data.remove::<String>(id));
                    } else if r.has_focus() {
                        ui.memory_mut(|m| m.data.insert_temp(id, text));
                    }
                }
            }
            if !e.help.is_empty() {
                ui.add_space(8.0);
                ui.weak(e.help);
            }
        });
    });
    changed
}

fn bool_field<'a>(s: &'a mut Settings, key: &str) -> Option<&'a mut bool> {
    Some(match key {
        "ligatures" => &mut s.ligatures,
        "caret_blink" => &mut s.caret_blink,
        "use_tabs" => &mut s.use_tabs,
        "detect_indentation" => &mut s.detect_indentation,
        "auto_indent" => &mut s.auto_indent,
        "auto_pair" => &mut s.auto_pair,
        "smart_backspace" => &mut s.smart_backspace,
        "word_wrap" => &mut s.word_wrap,
        "completion" => &mut s.completion,
        "vim_mode" => &mut s.vim_mode,
        "line_numbers" => &mut s.line_numbers,
        "relative_line_numbers" => &mut s.relative_line_numbers,
        "highlight_current_line" => &mut s.highlight_current_line,
        "highlight_matching_brackets" => &mut s.highlight_matching_brackets,
        "highlight_word_occurrences" => &mut s.highlight_word_occurrences,
        "indent_guides" => &mut s.indent_guides,
        "minimap" => &mut s.minimap,
        "scroll_past_end" => &mut s.scroll_past_end,
        "smooth_scrolling" => &mut s.smooth_scrolling,
        "scrollbars" => &mut s.scrollbars,
        "sidebar" => &mut s.sidebar,
        "status_bar" => &mut s.status_bar,
        "menu_bar" => &mut s.menu_bar,
        "tabs" => &mut s.tabs,
        "breadcrumbs" => &mut s.breadcrumbs,
        "git_gutter" => &mut s.git_gutter,
        "trim_trailing_whitespace_on_save" => &mut s.trim_trailing_whitespace_on_save,
        "ensure_final_newline_on_save" => &mut s.ensure_final_newline_on_save,
        "format_on_save" => &mut s.format_on_save,
        "recovery" => &mut s.recovery,
        "restore_session" => &mut s.restore_session,
        "confirm_delete" => &mut s.confirm_delete,
        "find_wrap" => &mut s.find_wrap,
        "find_case_sensitive" => &mut s.find_case_sensitive,
        "find_whole_word" => &mut s.find_whole_word,
        "find_regex" => &mut s.find_regex,
        "follow_symlinks" => &mut s.follow_symlinks,
        "show_hidden_files" => &mut s.show_hidden_files,
        "reduced_motion" => &mut s.reduced_motion,
        "low_distraction" => &mut s.low_distraction,
        "muted_syntax" => &mut s.muted_syntax,
        "typewriter_scrolling" => &mut s.typewriter_scrolling,
        "spell_check" => &mut s.spell_check,
        "spell_in_comments" => &mut s.spell_in_comments,
        "spell_in_strings" => &mut s.spell_in_strings,
        "lsp_enabled" => &mut s.lsp_enabled,
        _ => return None,
    })
}

fn usize_field<'a>(s: &'a mut Settings, key: &str) -> Option<&'a mut usize> {
    Some(match key {
        "tab_width" => &mut s.tab_width,
        "indent_width" => &mut s.indent_width,
        "wrap_column" => &mut s.wrap_column,
        "large_file_mb" => &mut s.large_file_mb,
        _ => return None,
    })
}

fn u64_field<'a>(s: &'a mut Settings, key: &str) -> Option<&'a mut u64> {
    Some(match key {
        "completion_delay_ms" => &mut s.completion_delay_ms,
        "auto_save_delay_ms" => &mut s.auto_save_delay_ms,
        "recovery_interval_ms" => &mut s.recovery_interval_ms,
        _ => return None,
    })
}

fn f32_field<'a>(s: &'a mut Settings, key: &str) -> Option<&'a mut f32> {
    Some(match key {
        "font_size" => &mut s.font_size,
        "ui_font_size" => &mut s.ui_font_size,
        "line_height" => &mut s.line_height,
        "ui_scale" => &mut s.ui_scale,
        "caret_width" => &mut s.caret_width,
        "minimap_width" => &mut s.minimap_width,
        "distraction_free_width" => &mut s.distraction_free_width,
        _ => return None,
    })
}

fn string_field<'a>(s: &'a mut Settings, key: &str) -> Option<&'a mut String> {
    Some(match key {
        "theme_mode" => &mut s.theme_mode,
        "font_family" => &mut s.font_family,
        "default_encoding" => &mut s.default_encoding,
        "fallback_encoding" => &mut s.fallback_encoding,
        "default_line_ending" => &mut s.default_line_ending,
        "spell_language" => &mut s.spell_language,
        _ => return None,
    })
}

fn string_list_field<'a>(s: &'a mut Settings, key: &str) -> Option<&'a mut Vec<String>> {
    Some(match key {
        "exclude_patterns" => &mut s.exclude_patterns,
        "binary_patterns" => &mut s.binary_patterns,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every entry must resolve to a field of its declared kind, and every
    /// settings key (except the structured lsp_servers map) must have an entry.
    #[test]
    fn entries_cover_settings() {
        let mut s = Settings::default();
        for e in ENTRIES {
            let ok = match e.kind {
                Kind::Bool => bool_field(&mut s, e.key).is_some(),
                Kind::Int(..) => usize_field(&mut s, e.key).is_some(),
                Kind::U64(..) => u64_field(&mut s, e.key).is_some(),
                Kind::Float(..) => f32_field(&mut s, e.key).is_some(),
                Kind::Text | Kind::Choice(_) => string_field(&mut s, e.key).is_some(),
                Kind::List => string_list_field(&mut s, e.key).is_some(),
                Kind::Theme => e.key == "theme",
                Kind::IntList => e.key == "rulers",
                Kind::Whitespace => e.key == "show_whitespace",
            };
            assert!(ok, "entry {} does not resolve", e.key);
        }
        let table = toml::Value::try_from(Settings::default()).unwrap();
        for key in table.as_table().unwrap().keys() {
            if key == "lsp_servers" {
                continue;
            }
            assert!(ENTRIES.iter().any(|e| e.key == key), "settings key {key} has no settings panel entry");
        }
    }
}
