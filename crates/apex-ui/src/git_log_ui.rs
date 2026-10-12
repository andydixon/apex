//! Git > Show Log: every commit reachable from branches and tags with a lane
//! graph, a filter, and actions on a commit (show, branch here, check out,
//! cherry-pick, revert, open on the remote).

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use apex_project::git_repo::LogRow;
use egui::{Color32, Rect, Sense, Stroke, pos2, vec2};

use crate::app::ApexApp;
use crate::command::Command;
use crate::workspace_ui::PromptKind;

const LIMIT: usize = 5000;

#[derive(Default)]
pub struct LogWindow {
    pub open: bool,
    repo: PathBuf,
    rows: Vec<LogRow>,
    loading: Option<Receiver<Result<Vec<LogRow>, String>>>,
    error: Option<String>,
    filter: String,
    selected: Option<usize>,
    /// Reload once the action started from the log has run.
    pub(crate) reload_after_job: bool,
}

const LANE_COLOURS: [Color32; 6] = [
    Color32::from_rgb(0x4e, 0x9a, 0xf0),
    Color32::from_rgb(0x5c, 0xc0, 0x6c),
    Color32::from_rgb(0xe0, 0x9a, 0x3c),
    Color32::from_rgb(0xc0, 0x6c, 0xd8),
    Color32::from_rgb(0xe0, 0x5c, 0x5c),
    Color32::from_rgb(0x4c, 0xc0, 0xc0),
];

impl ApexApp {
    pub(crate) fn git_log_command(&mut self, cmd: Command) -> bool {
        if cmd != Command::GitLog {
            return false;
        }
        let Some(repo) = self.git_panel_repo().or_else(|| self.git_target(None).map(|t| t.0)) else { return true };
        let w = &mut self.git.log;
        if w.repo != repo {
            *w = Default::default();
            w.repo = repo;
        }
        w.open = true;
        self.git_log_reload();
        true
    }

    fn git_log_reload(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        let repo = self.git.log.repo.clone();
        let ctx = self.egui_ctx.clone();
        std::thread::Builder::new()
            .name("apex-git-log".into())
            .spawn(move || {
                let _ = tx.send(apex_project::git_repo::log(&repo, LIMIT).map_err(|e| e.message().to_string()));
                if let Some(c) = ctx {
                    c.request_repaint();
                }
            })
            .ok();
        self.git.log.loading = Some(rx);
    }

    pub(crate) fn show_git_log(&mut self, ctx: &egui::Context) {
        if !self.git.log.open {
            return;
        }
        if let Some(rx) = &self.git.log.loading
            && let Ok(r) = rx.try_recv()
        {
            let w = &mut self.git.log;
            w.loading = None;
            match r {
                Ok(rows) => {
                    w.rows = rows;
                    w.error = None;
                }
                Err(e) => w.error = Some(e),
            }
        }
        let theme = self.theme.clone();
        let ui_font = self.settings.ui_font_size;
        let mut open = true;
        let mut cmds: Vec<(Command, Option<String>)> = Vec::new();
        let mut reload = false;
        let mut prompt_branch: Option<String> = None;
        egui::Window::new("Git Log")
            .id(egui::Id::new("apex-git-log"))
            .open(&mut open)
            .default_size([1000.0, 620.0])
            .max_size(ctx.content_rect().size() - vec2(16.0, 16.0))
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                let w = &mut self.git.log;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(w.repo.display().to_string()).strong());
                    ui.add(
                        egui::TextEdit::singleline(&mut w.filter)
                            .id(egui::Id::new("git-log-filter"))
                            .hint_text("Filter by message, author or id")
                            .desired_width(260.0),
                    );
                    if ui.button("Reload").clicked() {
                        reload = true;
                    }
                    if w.loading.is_some() {
                        ui.spinner();
                    } else {
                        ui.label(
                            egui::RichText::new(format!("{} commits{}", w.rows.len(), if w.rows.len() >= LIMIT { " (most recent)" } else { "" }))
                                .color(theme.ui.text_muted),
                        );
                    }
                });
                if let Some(e) = &w.error {
                    ui.label(egui::RichText::new(e).color(theme.ui.error));
                }
                ui.separator();
                let q = w.filter.trim().to_lowercase();
                let visible: Vec<usize> = (0..w.rows.len())
                    .filter(|&i| {
                        q.is_empty() || {
                            let r = &w.rows[i];
                            r.summary.to_lowercase().contains(&q)
                                || r.author.to_lowercase().contains(&q)
                                || r.id.starts_with(&q)
                                || r.refs.iter().any(|x| x.to_lowercase().contains(&q))
                        }
                    })
                    .collect();
                // The graph only makes sense for the full, unfiltered list.
                let graph = q.is_empty();
                let lanes =
                    if graph { w.rows.iter().flat_map(|r| r.top.iter().chain(&r.bottom).map(|s| s.0.max(s.1))).max().map(|m| m + 1).unwrap_or(1) } else { 0 };
                let lane_w = 12.0;
                let graph_w = (lanes as f32 * lane_w + 6.0).min(220.0);
                let row_h = ui_font + 10.0;
                if !ui.ctx().memory(|m| m.has_focus(egui::Id::new("git-log-filter"))) && !visible.is_empty() && ui.ui_contains_pointer() {
                    let (up, down, enter) =
                        ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::Enter)));
                    let cur = w.selected.and_then(|s| visible.iter().position(|&v| v == s)).unwrap_or(0);
                    if up {
                        w.selected = Some(visible[cur.saturating_sub(1)]);
                    } else if down {
                        w.selected = Some(visible[(cur + 1).min(visible.len() - 1)]);
                    } else if enter && let Some(s) = w.selected {
                        cmds.push((Command::GitShowCommit, Some(w.rows[s].id.clone())));
                    }
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, visible.len(), |ui, range| {
                    for vi in range {
                        let i = visible[vi];
                        let r = &w.rows[i];
                        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                        let selected = w.selected == Some(i);
                        if selected || resp.hovered() {
                            ui.painter().rect_filled(rect, 2.0, if selected { theme.ui.selection } else { theme.ui.hover });
                        }
                        let p = ui.painter_at(rect);
                        if graph {
                            let gx = |lane: usize| rect.min.x + 6.0 + lane as f32 * lane_w + lane_w / 2.0;
                            let (top, mid, bot) = (rect.min.y, rect.center().y, rect.max.y);
                            for &(a, b) in &r.top {
                                p.line_segment([pos2(gx(a), top), pos2(gx(b), mid)], Stroke::new(1.6, LANE_COLOURS[a % 6]));
                            }
                            for &(a, b) in &r.bottom {
                                p.line_segment([pos2(gx(a), mid), pos2(gx(b), bot)], Stroke::new(1.6, LANE_COLOURS[b % 6]));
                            }
                            let c = LANE_COLOURS[r.lane % 6];
                            if r.parents > 1 {
                                p.circle(pos2(gx(r.lane), mid), 4.0, theme.editor.background, Stroke::new(2.0, c));
                            } else {
                                p.circle_filled(pos2(gx(r.lane), mid), 4.0, c);
                            }
                        }
                        let font = egui::FontId::proportional(ui_font);
                        let small = egui::FontId::proportional(ui_font - 1.0);
                        let mut x = rect.min.x + graph_w + 6.0;
                        p.text(
                            pos2(x, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            &r.id[..7.min(r.id.len())],
                            egui::FontId::monospace(ui_font - 1.0),
                            theme.ui.text_muted,
                        );
                        x += 70.0;
                        for name in &r.refs {
                            let g = p.layout_no_wrap(name.clone(), small.clone(), theme.ui.text);
                            let bw = g.rect.width() + 10.0;
                            let br = Rect::from_min_size(pos2(x, rect.min.y + 3.0), vec2(bw, row_h - 6.0));
                            let colour = if name.starts_with("tag:") {
                                theme.ui.warning
                            } else if name.starts_with("HEAD") {
                                theme.ui.accent
                            } else {
                                theme.editor.git_added
                            };
                            p.rect_filled(br, 3.0, Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), 50));
                            p.galley(pos2(x + 5.0, rect.center().y - g.rect.height() / 2.0), g, theme.ui.text);
                            x += bw + 4.0;
                        }
                        let right_w = 260.0;
                        let summary_clip = Rect::from_min_max(pos2(x, rect.min.y), pos2((rect.max.x - right_w).max(x), rect.max.y));
                        ui.painter_at(summary_clip).text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, &r.summary, font.clone(), theme.ui.text);
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                        p.text(pos2(rect.max.x - right_w + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, &r.author, small.clone(), theme.ui.text_muted);
                        p.text(
                            pos2(rect.max.x - 8.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            apex_project::git::relative_age(r.time, now, false),
                            small.clone(),
                            theme.ui.text_muted,
                        );
                        if resp.clicked() {
                            w.selected = Some(i);
                        }
                        if resp.double_clicked() {
                            cmds.push((Command::GitShowCommit, Some(r.id.clone())));
                        }
                        let id = r.id.clone();
                        resp.context_menu(|ui| {
                            let mut act = |ui: &mut egui::Ui, label: &str, c: Command| {
                                if ui.button(label).clicked() {
                                    cmds.push((c, Some(id.clone())));
                                    ui.close();
                                }
                            };
                            act(ui, "Show Commit", Command::GitShowCommit);
                            if ui.button("Copy Commit Id").clicked() {
                                ui.ctx().copy_text(id.clone());
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Create Branch Here…").clicked() {
                                prompt_branch = Some(id.clone());
                                ui.close();
                            }
                            act(ui, "Check Out (Detached)…", Command::GitCheckoutCommit);
                            act(ui, "Cherry-pick onto Current Branch", Command::GitCherryPick);
                            act(ui, "Revert Commit…", Command::GitRevertCommit);
                            ui.separator();
                            act(ui, "Open on Remote", Command::GitOpenCommitOnRemote);
                        });
                    }
                });
            });
        let filter_focused = ctx.memory(|m| m.has_focus(egui::Id::new("git-log-filter")));
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !filter_focused && self.dialog.is_none() && !self.palette.open {
            open = false;
        }
        if !open {
            self.git.log.open = false;
        }
        if reload {
            self.git_log_reload();
        }
        if let Some(id) = prompt_branch {
            self.prompt("New branch name", "", PromptKind::GitArg(Command::GitCreateBranch, Some(id)));
        }
        self.queue.extend(cmds);
        // Set by the Git job queue whenever a command finishes.
        if self.git.log.reload_after_job && self.git.ops.running.is_none() {
            self.git.log.reload_after_job = false;
            self.git_log_reload();
        }
    }
}
