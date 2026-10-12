//! Build tasks in the application: task discovery, running, the output and
//! problems panels, and workspace trust.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use apex_project::tasks::{self, MAX_OUTPUT_LINES, Problem, RunningTask, TaskDef, TaskEvent, TaskSource};
use egui::Ui;

use crate::app::{ApexApp, Dialog};
use crate::command::Command;
use crate::palette::{PaletteItem, PaletteMode, Payload};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelTab {
    #[default]
    Find,
    Output,
    Problems,
    Git,
}

#[derive(Default)]
pub struct TasksUi {
    pub defs: Vec<TaskDef>,
    pub selected: Option<usize>,
    pub running: Option<RunningTask>,
    pub output: VecDeque<String>,
    pub problems: Vec<Problem>,
    pub last_exit: Option<(String, Option<i32>, Duration, bool, bool)>,
    pub problem_index: Option<usize>,
    pub scroll_to_bottom: bool,
    pub project_tasks: Vec<TaskDef>,
    pub last_run: Option<usize>,
    detected_for: Vec<PathBuf>,
}

/// Workspace roots the user has trusted to run project-defined programs.
#[derive(Default)]
pub struct TrustStore {
    trusted: HashSet<PathBuf>,
    loaded: bool,
}

impl TrustStore {
    fn path() -> PathBuf {
        apex_project::paths::data_dir().join("trusted.json")
    }

    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(bytes) = std::fs::read(Self::path())
            && let Ok(v) = serde_json::from_slice::<Vec<PathBuf>>(&bytes)
        {
            self.trusted = v.into_iter().collect();
        }
    }

    fn save(&self) {
        let p = Self::path();
        let _ = p.parent().map(apex_project::paths::ensure_private_dir);
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.trusted.iter().collect::<Vec<_>>()) {
            let _ = apex_project::fileio::save_atomic(&p, &bytes);
        }
    }

    pub fn is_trusted(&mut self, root: &Path) -> bool {
        self.load();
        self.trusted.iter().any(|t| root.starts_with(t))
    }

    pub fn trust(&mut self, root: &Path) {
        self.load();
        self.trusted.insert(root.to_path_buf());
        self.save();
    }
}

impl ApexApp {
    /// Rebuild the task list from detection, user tasks and (trusted) project tasks.
    pub(crate) fn tasks_refresh(&mut self) {
        if self.tasks.detected_for == self.roots && !self.tasks.defs.is_empty() {
            return;
        }
        self.tasks.detected_for = self.roots.clone();
        let mut defs = Vec::new();
        for r in &self.roots {
            defs.extend(tasks::detect_tasks(r));
        }
        // User tasks from <config>/tasks.toml.
        let user_file = self.config_dir.join("tasks.toml");
        if let Ok(src) = std::fs::read_to_string(&user_file) {
            match toml::from_str::<TaskFile>(&src) {
                Ok(f) => defs.extend(f.tasks.into_iter().map(|mut t| {
                    t.source = TaskSource::User;
                    t
                })),
                Err(e) => tracing::warn!("tasks.toml: {e}"),
            }
        }
        defs.extend(self.tasks.project_tasks.iter().cloned());
        self.tasks.defs = defs;
        if self.tasks.selected.is_some_and(|i| i >= self.tasks.defs.len()) {
            self.tasks.selected = None;
        }
    }

    /// Tasks declared in a project file (`[[tasks]]`), kept until trusted.
    pub(crate) fn set_project_tasks(&mut self, tasks: Vec<TaskDef>) {
        self.tasks.project_tasks = tasks
            .into_iter()
            .map(|mut t| {
                t.source = TaskSource::Project;
                t
            })
            .collect();
        self.tasks.detected_for.clear();
    }

    pub(crate) fn tasks_poll(&mut self) -> bool {
        let Some(t) = &self.tasks.running else { return false };
        let mut got = false;
        for _ in 0..512 {
            match t.rx.try_recv() {
                Ok(TaskEvent::Line(l)) => {
                    if self.tasks.output.len() >= MAX_OUTPUT_LINES {
                        self.tasks.output.pop_front();
                    }
                    self.tasks.output.push_back(l);
                    got = true;
                }
                Ok(TaskEvent::Problem(p)) => {
                    self.tasks.problems.push(p);
                    got = true;
                }
                Ok(TaskEvent::Error(e)) => {
                    self.tasks.output.push_back(format!("[error] {e}"));
                    got = true;
                }
                Ok(TaskEvent::Exited { code, elapsed, timed_out, killed }) => {
                    let name = t.name.clone();
                    let status = if timed_out {
                        format!("[timed out after {:.1}s]", elapsed.as_secs_f32())
                    } else if killed {
                        "[cancelled]".to_string()
                    } else {
                        format!("[finished in {:.2}s with exit code {}]", elapsed.as_secs_f32(), code.map(|c| c.to_string()).unwrap_or_else(|| "none".into()))
                    };
                    self.tasks.output.push_back(status.clone());
                    self.tasks.last_exit = Some((name.clone(), code, elapsed, timed_out, killed));
                    self.tasks.running = None;
                    let errors = self.tasks.problems.iter().filter(|p| p.is_error).count();
                    let summary = if errors > 0 { format!("{name}: {errors} error{}", if errors == 1 { "" } else { "s" }) } else { format!("{name} {status}") };
                    self.set_status(summary, code.is_some_and(|c| c != 0) || timed_out);
                    return true;
                }
                Err(_) => break,
            }
        }
        if got {
            self.tasks.scroll_to_bottom = true;
        }
        got || self.tasks.running.is_some()
    }

    pub(crate) fn run_task(&mut self, index: usize, ctx: &egui::Context) {
        let Some(def) = self.tasks.defs.get(index).cloned() else { return };
        let root = self.roots.first().cloned().or_else(|| self.current_dir()).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        if def.source == TaskSource::Project && !self.trust.is_trusted(&root) {
            self.dialog = Some(Dialog::Trust { root, then: Some((Command::Build, Some(index.to_string()))) });
            return;
        }
        if let Some(r) = self.tasks.running.take() {
            r.cancel();
        }
        // Save modified files first, as build tools read from disk.
        self.execute(ctx, Command::SaveAll, None);
        self.tasks.output.clear();
        self.tasks.problems.clear();
        self.tasks.problem_index = None;
        self.tasks.last_exit = None;
        self.tasks.output.push_back(format!("$ {}  (in {})", def.command.join(" "), root.display()));
        match tasks::run(&def, &root) {
            Ok(r) => {
                self.tasks.running = Some(r);
                self.tasks.last_run = Some(index);
                self.tasks.selected = Some(index);
                self.panel_open = true;
                self.panel_tab = PanelTab::Output;
                self.tasks.scroll_to_bottom = true;
            }
            Err(e) => {
                self.tasks.output.push_back(format!("[error] {e}"));
                self.panel_open = true;
                self.panel_tab = PanelTab::Output;
                self.set_status(format!("Could not start task: {e}"), true);
            }
        }
    }

    pub(crate) fn tasks_command(&mut self, ctx: &egui::Context, cmd: Command, arg: Option<String>) -> bool {
        match cmd {
            Command::BuildSelect => {
                self.tasks_refresh();
                let mut items: Vec<PaletteItem> = self
                    .tasks
                    .defs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| PaletteItem {
                        label: t.name.clone(),
                        detail: format!(
                            "{}{}",
                            t.command.join(" "),
                            match t.source {
                                TaskSource::Project => " (project)",
                                TaskSource::User => " (user)",
                                TaskSource::BuiltIn => "",
                            }
                        ),
                        shortcut: String::new(),
                        payload: Payload::Index(i),
                        boost: (self.tasks.selected == Some(i)) as u32,
                    })
                    .collect();
                if let Some(p) = self.active_ids().and_then(|(_, d)| self.docs.get(&d)).and_then(|d| d.path.clone())
                    && let Some(t) = tasks::task_for_file(&p)
                {
                    self.tasks.defs.push(t.clone());
                    items.push(PaletteItem {
                        label: t.name.clone(),
                        detail: t.command.join(" "),
                        shortcut: String::new(),
                        payload: Payload::Index(self.tasks.defs.len() - 1),
                        boost: 0,
                    });
                }
                if items.is_empty() {
                    self.set_status("No build tasks detected; open a folder with Cargo.toml, go.mod, package.json, Makefile or add tasks.toml".into(), false);
                    return true;
                }
                self.palette.show_with(PaletteMode::BuildTasks, items, "Select build task", "");
            }
            Command::Build => {
                self.tasks_refresh();
                let index = arg.and_then(|a| a.parse::<usize>().ok()).or(self.tasks.selected).or(if self.tasks.defs.is_empty() { None } else { Some(0) });
                match index {
                    Some(i) => self.run_task(i, ctx),
                    None => self.execute(ctx, Command::BuildSelect, None),
                }
            }
            Command::BuildRerun => match self.tasks.last_run {
                Some(i) => self.run_task(i, ctx),
                None => self.execute(ctx, Command::BuildSelect, None),
            },
            Command::BuildCancel => {
                if let Some(r) = self.tasks.running.take() {
                    r.cancel();
                    self.tasks.output.push_back("[cancelling…]".into());
                } else {
                    self.set_status("No task is running".into(), false);
                }
            }
            Command::ShowOutput => {
                self.panel_open = !(self.panel_open && self.panel_tab == PanelTab::Output);
                self.panel_tab = PanelTab::Output;
            }
            Command::ShowProblems => {
                self.panel_open = !(self.panel_open && self.panel_tab == PanelTab::Problems);
                self.panel_tab = PanelTab::Problems;
            }
            Command::NextDiagnostic | Command::PrevDiagnostic => {
                let all = self.all_problems();
                if all.is_empty() {
                    self.set_status("No problems".into(), false);
                    return true;
                }
                let n = all.len();
                let i = match (self.tasks.problem_index, cmd) {
                    (None, Command::NextDiagnostic) => 0,
                    (None, _) => n - 1,
                    (Some(i), Command::NextDiagnostic) => (i + 1) % n,
                    (Some(i), _) => (i + n - 1) % n,
                };
                self.tasks.problem_index = Some(i);
                let p = all[i].clone();
                self.goto_problem(&p, ctx);
            }
            Command::TrustWorkspace => {
                let root = self.roots.first().cloned().or_else(|| self.current_dir());
                match root {
                    Some(r) => self.dialog = Some(Dialog::Trust { root: r, then: None }),
                    None => self.set_status("Open a folder first".into(), false),
                }
            }
            _ => return false,
        }
        true
    }

    /// Task problems plus language-server diagnostics, in file order.
    pub(crate) fn all_problems(&self) -> Vec<Problem> {
        let mut v: Vec<Problem> = self.tasks.problems.clone();
        v.extend(self.lsp_problems());
        v.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
        v.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.message == b.message);
        v
    }

    pub(crate) fn goto_problem(&mut self, p: &Problem, ctx: &egui::Context) {
        self.note_jump();
        if let Some(v) = self.open_path(&p.file) {
            self.make_permanent(v);
            self.goto_line_col(v, p.line, p.col.unwrap_or(1));
            self.focus_active_editor(ctx);
            self.set_status(p.message.clone(), p.is_error);
        }
    }

    pub(crate) fn show_output_panel(&mut self, ui: &mut Ui) {
        let theme = self.theme.clone();
        let ui_font = self.settings.ui_font_size;
        let mono = egui::FontId::monospace(ui_font);
        ui.horizontal(|ui| {
            let name = self.tasks.running.as_ref().map(|r| r.name.clone()).or_else(|| self.tasks.last_exit.as_ref().map(|e| e.0.clone()));
            if let Some(n) = name {
                ui.label(egui::RichText::new(n).strong());
            }
            if let Some(r) = &self.tasks.running {
                ui.spinner();
                ui.label(egui::RichText::new(format!("{:.1}s", r.started.elapsed().as_secs_f32())).color(theme.ui.text_muted));
                if ui.button("Cancel").clicked() {
                    self.queue.push_back((Command::BuildCancel, None));
                }
            } else if ui.button("Run").clicked() {
                self.queue.push_back((Command::BuildRerun, None));
            }
            if ui.button("Select task…").clicked() {
                self.queue.push_back((Command::BuildSelect, None));
            }
            if ui.button("Clear").clicked() {
                self.tasks.output.clear();
            }
            let errors = self.tasks.problems.iter().filter(|p| p.is_error).count();
            let warnings = self.tasks.problems.len() - errors;
            if errors + warnings > 0 {
                ui.label(egui::RichText::new(format!("{errors} errors, {warnings} warnings")).color(if errors > 0 {
                    theme.ui.error
                } else {
                    theme.ui.warning
                }));
            }
        });
        ui.separator();
        let row_h = ui_font + 5.0;
        let n = self.tasks.output.len();
        let re = location_regex();
        let mut open: Option<(PathBuf, usize, usize)> = None;
        let root = self.roots.first().cloned().unwrap_or_default();
        let area = egui::ScrollArea::both().auto_shrink([false, false]).id_salt("apex-output").stick_to_bottom(self.tasks.scroll_to_bottom);
        self.tasks.scroll_to_bottom = self.tasks.running.is_some();
        area.show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let line = &self.tasks.output[i];
                let is_err = line.contains("error") && !line.contains("0 errors");
                let colour = if line.starts_with('[') {
                    theme.ui.text_muted
                } else if is_err {
                    theme.ui.error
                } else if line.contains("warning") {
                    theme.ui.warning
                } else {
                    theme.ui.text
                };
                // Clickable file:line references.
                if let Some(m) = re.captures(line) {
                    let (file, l, c) = (
                        m.get(1).map(|x| x.as_str()).unwrap_or(""),
                        m.get(2).and_then(|x| x.as_str().parse::<usize>().ok()).unwrap_or(1),
                        m.get(3).and_then(|x| x.as_str().parse::<usize>().ok()).unwrap_or(1),
                    );
                    let resp = ui.add(egui::Label::new(egui::RichText::new(line).font(mono.clone()).color(colour).underline()).sense(egui::Sense::click()));
                    if resp.clicked() {
                        let p = PathBuf::from(file);
                        let p = if p.is_absolute() { p } else { root.join(p) };
                        open = Some((p, l, c));
                    }
                    resp.on_hover_text("Open location");
                } else {
                    ui.label(egui::RichText::new(line).font(mono.clone()).color(colour));
                }
            }
        });
        if let Some((p, l, c)) = open {
            self.note_jump();
            if let Some(v) = self.open_path(&p) {
                self.make_permanent(v);
                self.goto_line_col(v, l, c);
                self.focus_active_editor(ui.ctx());
            }
        }
    }

    pub(crate) fn show_problems_panel(&mut self, ui: &mut Ui) {
        let theme = self.theme.clone();
        let ui_font = self.settings.ui_font_size;
        let problems = self.all_problems();
        if problems.is_empty() {
            ui.label(egui::RichText::new("No problems reported by build tasks or language servers.").color(theme.ui.text_muted));
            return;
        }
        let row_h = ui_font + 6.0;
        let mut go: Option<usize> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("apex-problems").show_rows(ui, row_h, problems.len(), |ui, range| {
            for i in range {
                let p = &problems[i];
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                if resp.hovered() || self.tasks.problem_index == Some(i) {
                    ui.painter().rect_filled(rect, 2.0, if self.tasks.problem_index == Some(i) { theme.ui.selection } else { theme.ui.hover });
                }
                let icon = if p.is_error { "E" } else { "W" };
                let colour = if p.is_error { theme.ui.error } else { theme.ui.warning };
                ui.painter().text(rect.min + egui::vec2(6.0, row_h / 2.0), egui::Align2::LEFT_CENTER, icon, egui::FontId::proportional(ui_font), colour);
                let loc = format!("{}:{}{}", self.display_path(&p.file), p.line, p.col.map(|c| format!(":{c}")).unwrap_or_default());
                ui.painter().text(
                    rect.min + egui::vec2(24.0, row_h / 2.0),
                    egui::Align2::LEFT_CENTER,
                    &loc,
                    egui::FontId::proportional(ui_font),
                    theme.ui.accent,
                );
                let w = loc.len() as f32 * ui_font * 0.55 + 36.0;
                ui.painter().text(
                    rect.min + egui::vec2(w, row_h / 2.0),
                    egui::Align2::LEFT_CENTER,
                    p.message.chars().take(200).collect::<String>(),
                    egui::FontId::proportional(ui_font),
                    theme.ui.text,
                );
                if resp.clicked() {
                    go = Some(i);
                }
            }
        });
        if let Some(i) = go {
            self.tasks.problem_index = Some(i);
            let p = problems[i].clone();
            self.goto_problem(&p, ui.ctx());
        }
    }
}

#[derive(serde::Deserialize)]
struct TaskFile {
    #[serde(default)]
    tasks: Vec<TaskDef>,
}

fn location_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"([A-Za-z]:[\\/][^:\s]+|[^\s:]+\.[A-Za-z0-9]+):(\d+)(?::(\d+))?").expect("valid regex"))
}
