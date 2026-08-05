//! Syntax support: language detection, incremental tree-sitter parsing,
//! highlighting, folding and symbol extraction.
//!
//! Nothing here touches the GUI. A [`SyntaxState`] is owned per document by
//! the application and fed byte edits from [`apex_edit::Document`].
//!
//! # Threading
//!
//! Parsing normally happens on the caller's thread within a time budget. If a
//! parse exceeds the budget it is abandoned and restarted on a worker thread
//! with a snapshot of the rope; the result carries the document revision it
//! was parsed at and is discarded if the document has moved on. The stale tree
//! (already told about the edits) keeps providing highlights meanwhile.

pub mod fold;
pub mod highlight;
pub mod language;
pub mod simple;
pub mod symbols;

pub use highlight::{Span, SyntaxState};
pub use language::{LanguageDef, detect, languages, lookup};
pub use symbols::{Symbol, SymbolKind};
