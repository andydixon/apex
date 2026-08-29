//! The Apex document model.
//!
//! * [`Selection`] / [`SelectionSet`] — multi-cursor state (char offsets).
//! * [`Transaction`] — an ordered, non-overlapping set of replacements that is
//!   applied atomically and yields its own inverse.
//! * [`History`] — undo/redo of transactions with typing coalescing.
//! * [`Document`] — rope + revision + history + file metadata.
//! * [`ops`] — pure editing commands producing transactions from selections.
//!
//! # Invariants
//!
//! * Every selection endpoint is a valid char offset `0..=len_chars` and lies
//!   on a grapheme boundary once an operation has normalised it.
//! * Selections in a [`SelectionSet`] are sorted by start offset and do not
//!   overlap. Two empty selections never share an offset. The set is never empty.
//! * Changes in a [`Transaction`] are sorted and pairwise disjoint; they are
//!   expressed in *pre-edit* coordinates and applied last-to-first so that
//!   earlier offsets stay valid.
//! * [`Document::revision`] strictly increases with every applied transaction.

pub mod brackets;
pub mod document;
pub mod history;
pub mod indent;
pub mod ops;
pub mod search;
pub mod selection;
pub mod snippet;
pub mod transaction;

pub use document::{ByteEdit, DocId, Document, EditKind, FileIdentity};
pub use history::History;
pub use indent::{IndentSettings, IndentStyle, detect_indent};
pub use selection::{Selection, SelectionSet};
pub use transaction::{Assoc, Change, Transaction};
