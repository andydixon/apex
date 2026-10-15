//! Workspace-level services that do not depend on the GUI.
//!
//! This crate is the non-UI layer of Apex. `apex-app` and `apex-ui` call into
//! it; it never calls back into them. Everything that may block (file I/O,
//! process spawning, network through `git`/`gh`, LSP transport) either
//! returns quickly or runs on its own thread and reports through a channel
//! that the UI polls once per frame.
//!
//! Rough map:
//! - `fileio`, `paths`, `session`, `watch`, `tree`, `index`, `search`: files
//!   and workspaces.
//! - `git`, `git_repo`: read-only repository queries through libgit2.
//! - `git_cli`, `git_history`, `forge`: writes and network through the user's
//!   `git` and `gh` binaries, run as cancellable background jobs.
//! - `lsp`: language-server process management and JSON-RPC framing.
//! - `tasks`: build/test/run commands with output and problem parsing.
//! - `spell`: a word-list spell checker.
//! - `diff_align`: block and word alignment for the side-by-side diff view.

pub mod diff_align;
pub mod fileio;
pub mod forge;
pub mod git;
pub mod git_cli;
pub mod git_history;
pub mod git_repo;
pub mod index;
pub mod lsp;
pub mod paths;
pub mod remote;
pub mod search;
pub mod secret;
pub mod session;
pub mod spell;
pub mod tasks;
pub mod token;
pub mod tree;
pub mod watch;
