//! Apex user interface built on egui.

pub mod app;
pub mod command;
pub mod editor;
pub mod find_in_files;
pub mod git_ui;
pub mod keymap;
pub mod lsp_ui;
pub mod palette;
pub mod settings;
pub mod sidebar;
pub mod snippets_ui;
pub mod spell_ui;
pub mod symbol_index;
pub mod tasks_ui;
pub mod theme;
pub mod view;
pub mod workspace;
pub mod workspace_ui;

pub use app::{ApexApp, StartupArgs};
