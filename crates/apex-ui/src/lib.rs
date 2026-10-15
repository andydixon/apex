//! Apex user interface built on egui.
//!
//! This crate owns everything the user sees: the application state and frame
//! loop (`app`), the editor widget (`editor`), the file explorer (`sidebar`),
//! the command palette, menus, dialogs, and the git, LSP, tasks and spelling
//! integrations that hang off `ApexApp`. The lower crates supply the pieces it
//! composes: `apex-buffer` (rope and positions), `apex-edit` (documents,
//! selections, operations), `apex-syntax` (tree-sitter highlighting) and
//! `apex-project` (file I/O, indexing, git, LSP, sessions).
//!
//! The binary crate (`apex-app`) constructs `ApexApp` with `StartupArgs` and
//! hands it to eframe; everything after that runs from `ApexApp::ui` once per
//! frame. `ApexApp` is a single struct split across many `impl` blocks, one per
//! module, so each feature file adds methods rather than owning state of its own.

/// `ApexApp` itself: state, the per-frame loop, command execution, menus and dialogs.
pub mod app;
/// The `Command` enum every key binding, menu item and palette entry resolves to.
pub mod command;
/// Side-by-side and unified diff tabs.
pub mod diff_ui;
/// The text editor widget: input handling and painting of one `View`.
pub mod editor;
/// Native open/save dialogs, run on a thread and polled each frame.
pub mod file_dialogs;
/// Project-wide search and replace.
pub mod find_in_files;
/// GitHub (`gh`) integration: pull requests and checks.
pub mod forge_ui;
/// Blame column and inline blame note.
pub mod git_blame;
/// Branch switch/create/rename/delete/merge commands.
pub mod git_branches;
/// The commit log window.
pub mod git_log_ui;
/// Long-running git operations (fetch, pull, push) with askpass support.
pub mod git_ops;
/// The source control panel in the bottom panel.
pub mod git_panel;
/// Repository status cache, gutter marks and branch label.
pub mod git_ui;
/// The hex editor binary files open in.
pub mod hex_ui;
/// Interactive history editing (rebase) window.
pub mod history_ui;
/// Key bindings: parsing, contexts and chord dispatch.
pub mod keymap;
/// Language server client glue: completion, hover, diagnostics, overlays.
pub mod lsp_ui;
/// macOS-only native dialog helpers.
#[cfg(target_os = "macos")]
pub mod macos_dialogs;
/// The command/goto palette widget.
pub mod palette;
/// Editing files on other machines over SSH.
pub mod remote_ui;
/// `Settings`: the user's `settings.toml`, with defaults and validation.
pub mod settings;
/// The settings editor panel.
pub mod settings_ui;
/// The file explorer sidebar.
pub mod sidebar;
/// Snippet expansion and tab-stop sessions.
pub mod snippets_ui;
/// Spell checking underlines and suggestions.
pub mod spell_ui;
/// Background project-wide symbol index for Goto Symbol in Project.
pub mod symbol_index;
/// Build tasks, the problems list and the bottom panel tabs.
pub mod tasks_ui;
/// Colour themes: built-ins, user TOML themes and egui style application.
pub mod theme;
/// `View`: one tab's selections, scroll, folds and layout cache over a document.
pub mod view;
/// Vim emulation state machine.
pub mod vim;
/// Vim key handling and status label.
pub mod vim_ui;
/// Panes and the split layout tree.
pub mod workspace;
/// Workspace roots, file index, sessions, recovery, macros and project commands.
pub mod workspace_ui;

// The only types the binary crate needs to start the editor.
pub use app::{ApexApp, OpenRequest, StartupArgs};
