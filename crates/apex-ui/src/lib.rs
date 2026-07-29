//! Apex user interface built on egui.

pub mod app;
pub mod command;
pub mod editor;
pub mod keymap;
pub mod settings;
pub mod theme;
pub mod view;
pub mod workspace;

pub use app::{ApexApp, StartupArgs};
