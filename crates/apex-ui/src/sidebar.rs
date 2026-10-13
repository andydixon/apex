//! The file explorer sidebar.
//!
//! `Sidebar` owns the lazily loaded directory trees for every workspace root
//! (`DirEntryNode` from `apex-project`), the filter text, the selected row
//! and the inline rename state. It knows nothing about documents or git: the
//! application pushes `git_marks` in (`git_ui`) and keeps `roots` in step with
//! `ApexApp::roots` via `set_roots`.
//!
//! Data flow: `workspace_ui::show_sidebar` calls `ui` once per frame; the
//! widget never mutates the file system or opens files itself, it returns a
//! list of `SidebarAction`s that `workspace_ui` executes afterwards. Row
//! clicks that only expand or collapse a directory are handled internally.
//!
//! Gotcha: `draw_node` recurses while borrowing `self` mutably, so the tree
//! being drawn is `take`n out of `self.roots` (and each node's `children`)
//! for the duration and put back afterwards.

use std::path::{Path, PathBuf};

use apex_project::tree::{DirEntryNode, TreeOptions};
use egui::{Color32, Rect, Ui, pos2, vec2};

use crate::theme::Theme;

/// Git status shown next to a path (filled in by the application).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitMark {
    /// Tracked and changed in the working tree (also staged or renamed).
    Modified,
    /// Newly added to the index.
    Added,
    /// Not known to git.
    Untracked,
    /// Deleted in the working tree.
    Deleted,
    /// Matched by `.gitignore`; drawn muted with no letter.
    Ignored,
    /// Has unresolved merge conflict markers.
    Conflict,
}

/// Something the user asked the explorer to do, returned from `Sidebar::ui`
/// and carried out by `workspace_ui` (which has access to documents, roots
/// and dialogs).
#[derive(Debug, Clone, PartialEq)]
pub enum SidebarAction {
    /// Single click: open in a preview tab that the next preview replaces.
    OpenPreview(PathBuf),
    /// Double click: open as a permanent tab.
    Open(PathBuf),
    /// Create a file inside this directory (prompt for the name).
    NewFile(PathBuf),
    /// Create a folder inside this directory (prompt for the name).
    NewFolder(PathBuf),
    /// Inline rename was confirmed with Enter; `new_name` is trimmed and non-empty.
    Rename { from: PathBuf, new_name: String },
    /// Copy a file next to itself.
    Duplicate(PathBuf),
    /// Delete after confirmation.
    Delete(PathBuf),
    /// Show the path in the OS file manager.
    Reveal(PathBuf),
    /// Copy the path to the clipboard; `true` means relative to its root.
    CopyPath(PathBuf, bool),
    /// Open Find in Files scoped to this directory.
    FindInFolder(PathBuf),
    /// Remove a workspace root (context menu on a root row).
    RemoveRoot(PathBuf),
    /// The "+" button: choose a folder to add as a root.
    AddFolder,
    /// The refresh button: re-read every loaded directory and restart the file index.
    Refresh,
    /// "Open Folder…" shown when there are no roots.
    OpenFolder,
}

/// State of the file explorer. Owned by `ApexApp::sidebar`; the trees are
/// loaded on demand as directories are expanded and refreshed by the file
/// watcher through `refresh_path`.
#[derive(Default)]
pub struct Sidebar {
    /// One tree per workspace root, in the same order as `ApexApp::roots`.
    pub roots: Vec<DirEntryNode>,
    /// Text of the filter box; matched case-insensitively against file names.
    pub filter: String,
    /// Panel width in points, updated from the resizable panel each frame and saved in the session.
    pub width: f32,
    /// The highlighted row (last clicked or revealed); may be a directory.
    pub selected: Option<PathBuf>,
    /// When set, the active document's file is revealed and selected automatically.
    pub follow_active: bool,
    /// Listing options (hidden files, ignore rules, sort order) handed to every node load.
    pub opts: TreeOptions,
    /// Inline rename in progress: (path, new name).
    pub renaming: Option<(PathBuf, String)>,
    /// Set when a rename starts so the text field grabs focus on its first frame.
    pub focus_rename: bool,
    /// Per-path git status, replaced wholesale by `git_ui` whenever status is refreshed.
    pub git_marks: std::collections::HashMap<PathBuf, GitMark>,
}

impl Sidebar {
    /// A sidebar with no roots, the default width (240 pt) and follow-active on.
    pub fn new(opts: TreeOptions) -> Self {
        Self { width: 240.0, follow_active: true, opts, ..Default::default() }
    }

    /// Bring the trees into line with the workspace roots: drop trees for roots
    /// that were removed and load a fresh top level for roots that are new.
    /// Existing trees keep their expansion state.
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

    /// Re-read every loaded directory from disk, keeping expansion state.
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

    /// Expand the ancestors of `path` in whichever root contains it and select it.
    /// Does nothing if no root contains the path.
    pub fn reveal(&mut self, path: &Path) {
        for r in &mut self.roots {
            if r.reveal(path, &self.opts) {
                self.selected = Some(path.to_path_buf());
                return;
            }
        }
    }

    /// Draw the whole explorer (header, filter box, trees) and return the
    /// actions the user triggered this frame. `active_file` is highlighted with
    /// the hover colour so it stands out without stealing the selection.
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
        // Lower-case once here; names are lower-cased per row in `draw_node`.
        let filter = self.filter.trim().to_lowercase();
        // Row height tracks the UI font so rows do not clip at large sizes.
        let row_h = ui_font + 8.0;
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            // Take the trees out so `draw_node` can borrow `self` mutably for
            // selection and rename state while walking them.
            let mut roots = std::mem::take(&mut self.roots);
            for (i, root) in roots.iter_mut().enumerate() {
                let is_last_root = i + 1 == self.roots.len();
                // Computed but unused at present (kept for a future root separator).
                let _ = is_last_root;
                self.draw_node(ui, root, 0, theme, ui_font, row_h, &filter, active_file, &mut actions, true);
            }
            self.roots = roots;
        });
        actions
    }

    /// Draw one row and, if it is an expanded directory, its children.
    /// Handles click (select/expand/preview), double click (open), the inline
    /// rename field and the context menu. Children are `take`n from the node
    /// while recursing for the same borrow reason as in `ui`.
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
        // Files that do not match the filter vanish; directories are pruned by the caller.
        if !filter.is_empty() && !node.is_dir && !node.name.to_lowercase().contains(filter) {
            return;
        }
        // 14 pt per level matches the width reserved for the disclosure triangle.
        let indent = 8.0 + depth as f32 * 14.0;
        // Rows are at least 200 pt wide so the horizontal scroll area has
        // something to scroll when the panel is narrow.
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(200.0), row_h), egui::Sense::click());
        let selected = self.selected.as_deref() == Some(node.path.as_path());
        let is_active = active_file == Some(node.path.as_path());
        if selected || is_active {
            // Selection wins over the active-file highlight when both apply.
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
            // Roots are recoloured below (`label_colour`); this arm only keeps the match exhaustive.
            None if is_root => theme.ui.text,
            None => theme.ui.text,
        };
        // Roots are drawn as small muted capitals, like section headings.
        let name = if is_root { node.name.to_uppercase() } else { node.name.clone() };
        let font = egui::FontId::proportional(if is_root { ui_font - 1.0 } else { ui_font });
        let label_colour = if is_root { theme.ui.text_muted } else { colour };
        // Inline rename field.
        if self.renaming.as_ref().is_some_and(|(p, _)| p == &node.path) {
            // `is_some_and` above guarantees the `Some`.
            let (_, text) = self.renaming.as_mut().expect("checked");
            let edit = egui::TextEdit::singleline(text).font(font.clone()).desired_width(rect.width() - (x - rect.min.x) - 8.0);
            let r = ui.put(Rect::from_min_max(pos2(x, rect.min.y + 1.0), pos2(rect.max.x - 4.0, rect.max.y - 1.0)), edit);
            // Focus only on the first frame so the user can click elsewhere later.
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
            // Escape, or clicking away, abandons the rename. Enter also drops
            // focus, hence the explicit `!enter` guard.
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
            // Arrow glyph marks symlinked entries; sits left of the git letter.
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
        // Single click selects; on a directory it toggles expansion, on a file
        // it opens a preview tab. Double click (below) makes the tab permanent.
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
        // Context menu. Roots cannot be renamed, duplicated or deleted from
        // here but can be removed from the workspace.
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
        // Children: load lazily on first expansion, show a load error inline,
        // then recurse. With a filter active, directories auto-expand so the
        // matching files are visible.
        if node.is_dir && node.expanded {
            if node.children.is_none() {
                node.load(&self.opts);
            }
            if let Some(err) = &node.error {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(format!("  {err}")).color(theme.ui.error).size(ui_font - 2.0));
            }
            // Take the children out for the recursive calls (see module docs).
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
        // Keeps the `Color32` import used; it is referenced nowhere else here.
        let _ = Color32::WHITE;
    }
}

/// Does any file at most six levels below `node` match the filter? Loads
/// directories on the way, so filtering a huge tree does real I/O; the
/// depth cap bounds that and stops symlink cycles.
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
