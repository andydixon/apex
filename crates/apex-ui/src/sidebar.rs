//! The file explorer sidebar.

use std::path::{Path, PathBuf};

use apex_project::tree::{DirEntryNode, TreeOptions};
use egui::{Color32, Rect, Ui, pos2, vec2};

use crate::theme::Theme;

/// Git status shown next to a path (filled in by the application).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitMark {
    Modified,
    Added,
    Untracked,
    Deleted,
    Ignored,
    Conflict,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SidebarAction {
    OpenPreview(PathBuf),
    Open(PathBuf),
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename { from: PathBuf, new_name: String },
    Duplicate(PathBuf),
    Delete(PathBuf),
    Reveal(PathBuf),
    CopyPath(PathBuf, bool),
    FindInFolder(PathBuf),
    RemoveRoot(PathBuf),
    AddFolder,
    Refresh,
    OpenFolder,
}

#[derive(Default)]
pub struct Sidebar {
    pub roots: Vec<DirEntryNode>,
    pub filter: String,
    pub width: f32,
    pub selected: Option<PathBuf>,
    pub follow_active: bool,
    pub opts: TreeOptions,
    /// Inline rename in progress: (path, new name).
    pub renaming: Option<(PathBuf, String)>,
    pub focus_rename: bool,
    pub git_marks: std::collections::HashMap<PathBuf, GitMark>,
}

impl Sidebar {
    pub fn new(opts: TreeOptions) -> Self {
        Self { width: 240.0, follow_active: true, opts, ..Default::default() }
    }

    pub fn set_roots(&mut self, roots: &[PathBuf]) {
        self.roots.retain(|r| roots.contains(&r.path));
        for r in roots {
            if !self.roots.iter().any(|n| &n.path == r) {
                let mut node = DirEntryNode::root(r.clone());
                node.load(&self.opts);
                self.roots.push(node);
            }
        }
    }

    pub fn refresh_all(&mut self) {
        for r in &mut self.roots {
            let mut visited = std::collections::HashSet::new();
            r.refresh(&self.opts, &mut visited);
        }
    }

    /// Refresh the directory containing `path` (or `path` itself if a dir).
    pub fn refresh_path(&mut self, path: &Path) {
        let dir = if path.is_dir() { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
        for r in &mut self.roots {
            if let Some(n) = r.find_mut(&dir)
                && n.children.is_some()
            {
                let mut visited = std::collections::HashSet::new();
                n.refresh(&self.opts, &mut visited);
            }
        }
    }

    pub fn reveal(&mut self, path: &Path) {
        for r in &mut self.roots {
            if r.reveal(path, &self.opts) {
                self.selected = Some(path.to_path_buf());
                return;
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, theme: &Theme, ui_font: f32, active_file: Option<&Path>) -> Vec<SidebarAction> {
        let mut actions = Vec::new();
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 0.0, theme.ui.sidebar);
        ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("FOLDERS").size(ui_font - 2.0).color(theme.ui.text_muted).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                    actions.push(SidebarAction::Refresh);
                }
                if ui.small_button("+").on_hover_text("Add folder to workspace").clicked() {
                    actions.push(SidebarAction::AddFolder);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter files")
                    .desired_width(ui.available_width() - 8.0)
                    .font(egui::FontId::proportional(ui_font - 1.0)),
            );
        });
        if self.roots.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("No folder open").color(theme.ui.text_muted));
                if ui.button("Open Folder…").clicked() {
                    actions.push(SidebarAction::OpenFolder);
                }
            });
            return actions;
        }
        let filter = self.filter.trim().to_lowercase();
        let row_h = ui_font + 8.0;
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            let mut roots = std::mem::take(&mut self.roots);
            for (i, root) in roots.iter_mut().enumerate() {
                let is_last_root = i + 1 == self.roots.len();
                let _ = is_last_root;
                self.draw_node(ui, root, 0, theme, ui_font, row_h, &filter, active_file, &mut actions, true);
            }
            self.roots = roots;
        });
        actions
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_node(
        &mut self,
        ui: &mut Ui,
        node: &mut DirEntryNode,
        depth: usize,
        theme: &Theme,
        ui_font: f32,
        row_h: f32,
        filter: &str,
        active_file: Option<&Path>,
        actions: &mut Vec<SidebarAction>,
        is_root: bool,
    ) {
        if !filter.is_empty() && !node.is_dir && !node.name.to_lowercase().contains(filter) {
            return;
        }
        let indent = 8.0 + depth as f32 * 14.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(200.0), row_h), egui::Sense::click());
        let selected = self.selected.as_deref() == Some(node.path.as_path());
        let is_active = active_file == Some(node.path.as_path());
        if selected || is_active {
            ui.painter().rect_filled(rect, 0.0, if selected { theme.ui.selection } else { theme.ui.hover });
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 0.0, theme.ui.hover);
        }
        let mut x = rect.min.x + indent;
        if node.is_dir {
            // Disclosure triangle.
            let c = pos2(x + 4.0, rect.center().y);
            let pts = if node.expanded {
                vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
            } else {
                vec![c + vec2(-2.0, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 4.0)]
            };
            ui.painter().add(egui::Shape::convex_polygon(pts, theme.ui.text_muted, egui::Stroke::NONE));
            x += 14.0;
        } else {
            x += 14.0;
        }
        let mark = self.git_marks.get(&node.path).copied();
        let colour = match mark {
            Some(GitMark::Modified) => theme.editor.git_modified,
            Some(GitMark::Added) | Some(GitMark::Untracked) => theme.editor.git_added,
            Some(GitMark::Deleted) | Some(GitMark::Conflict) => theme.editor.git_deleted,
            Some(GitMark::Ignored) => theme.ui.text_muted,
            None if is_root => theme.ui.text,
            None => theme.ui.text,
        };
        let name = if is_root { node.name.to_uppercase() } else { node.name.clone() };
        let font = egui::FontId::proportional(if is_root { ui_font - 1.0 } else { ui_font });
        let label_colour = if is_root { theme.ui.text_muted } else { colour };
        // Inline rename field.
        if self.renaming.as_ref().is_some_and(|(p, _)| p == &node.path) {
            let (_, text) = self.renaming.as_mut().expect("checked");
            let edit = egui::TextEdit::singleline(text).font(font.clone()).desired_width(rect.width() - (x - rect.min.x) - 8.0);
            let r = ui.put(Rect::from_min_max(pos2(x, rect.min.y + 1.0), pos2(rect.max.x - 4.0, rect.max.y - 1.0)), edit);
            if self.focus_rename {
                r.request_focus();
                self.focus_rename = false;
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if enter {
                let (p, new_name) = self.renaming.take().expect("checked");
                if !new_name.trim().is_empty() && new_name != node.name {
                    actions.push(SidebarAction::Rename { from: p, new_name: new_name.trim().to_string() });
                }
            } else if escape || (r.lost_focus() && !enter) {
                self.renaming = None;
            }
        } else {
            ui.painter().text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, &name, font, label_colour);
            if let Some(m) = mark {
                let s = match m {
                    GitMark::Modified => "M",
                    GitMark::Added => "A",
                    GitMark::Untracked => "U",
                    GitMark::Deleted => "D",
                    GitMark::Conflict => "!",
                    GitMark::Ignored => "",
                };
                if !s.is_empty() {
                    ui.painter().text(
                        pos2(rect.max.x - 8.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        s,
                        egui::FontId::proportional(ui_font - 2.0),
                        colour,
                    );
                }
            }
            if node.is_symlink {
                ui.painter().text(
                    pos2(rect.max.x - 24.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    "↗",
                    egui::FontId::proportional(ui_font - 2.0),
                    theme.ui.text_muted,
                );
            }
        }
        if resp.clicked() {
            self.selected = Some(node.path.clone());
            if node.is_dir {
                node.expanded = !node.expanded;
                if node.expanded && node.children.is_none() {
                    node.load(&self.opts);
                }
            } else {
                actions.push(SidebarAction::OpenPreview(node.path.clone()));
            }
        }
        if resp.double_clicked() && !node.is_dir {
            actions.push(SidebarAction::Open(node.path.clone()));
        }
        let path = node.path.clone();
        let is_dir = node.is_dir;
        resp.context_menu(|ui| {
            self.selected = Some(path.clone());
            let dir = if is_dir { path.clone() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
            if ui.button("New File…").clicked() {
                actions.push(SidebarAction::NewFile(dir.clone()));
                ui.close();
            }
            if ui.button("New Folder…").clicked() {
                actions.push(SidebarAction::NewFolder(dir.clone()));
                ui.close();
            }
            ui.separator();
            if !is_root {
                if ui.button("Rename").clicked() {
                    self.renaming = Some((path.clone(), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    self.focus_rename = true;
                    ui.close();
                }
                if !is_dir && ui.button("Duplicate").clicked() {
                    actions.push(SidebarAction::Duplicate(path.clone()));
                    ui.close();
                }
                if ui.button("Delete…").clicked() {
                    actions.push(SidebarAction::Delete(path.clone()));
                    ui.close();
                }
                ui.separator();
            }
            if ui.button("Find in Folder…").clicked() {
                actions.push(SidebarAction::FindInFolder(dir.clone()));
                ui.close();
            }
            if ui.button("Reveal in File Manager").clicked() {
                actions.push(SidebarAction::Reveal(path.clone()));
                ui.close();
            }
            if ui.button("Copy Path").clicked() {
                actions.push(SidebarAction::CopyPath(path.clone(), false));
                ui.close();
            }
            if ui.button("Copy Relative Path").clicked() {
                actions.push(SidebarAction::CopyPath(path.clone(), true));
                ui.close();
            }
            if is_root {
                ui.separator();
                if ui.button("Remove Folder from Workspace").clicked() {
                    actions.push(SidebarAction::RemoveRoot(path.clone()));
                    ui.close();
                }
            }
        });
        if node.is_dir && node.expanded {
            if node.children.is_none() {
                node.load(&self.opts);
            }
            if let Some(err) = &node.error {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(format!("  {err}")).color(theme.ui.error).size(ui_font - 2.0));
            }
            let mut children = node.children.take().unwrap_or_default();
            for child in children.iter_mut() {
                // With a filter, show directories only if something inside matches.
                if !filter.is_empty() && child.is_dir && !dir_matches(child, filter, &self.opts, 0) {
                    continue;
                }
                if !filter.is_empty() && child.is_dir {
                    child.expanded = true;
                }
                self.draw_node(ui, child, depth + 1, theme, ui_font, row_h, filter, active_file, actions, false);
            }
            node.children = Some(children);
        }
        let _ = Color32::WHITE;
    }
}

fn dir_matches(node: &mut DirEntryNode, filter: &str, opts: &TreeOptions, depth: usize) -> bool {
    if depth > 6 {
        return false;
    }
    if node.children.is_none() {
        node.load(opts);
    }
    let Some(children) = node.children.as_mut() else { return false };
    children.iter_mut().any(|c| if c.is_dir { dir_matches(c, filter, opts, depth + 1) } else { c.name.to_lowercase().contains(filter) })
}
