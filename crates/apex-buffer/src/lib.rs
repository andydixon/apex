//! Text storage foundations for Apex.
//!
//! This crate knows nothing about the GUI. It provides:
//!
//! * [`Rope`] re-exported from `ropey`, the canonical in-memory text store;
//! * Unicode-aware position mapping between the different offset spaces
//!   ([`position`]);
//! * grapheme- and word-aware caret motion ([`motion`]);
//! * line-ending detection and conversion ([`line_ending`]);
//! * encoding detection, decoding and encoding ([`encoding`]).
//!
//! # Offset spaces
//!
//! Apex distinguishes five different ways of addressing text; confusing them is
//! the root of most editor bugs, so every function in this crate names which one
//! it takes:
//!
//! | Space            | Unit                     | Used by                          |
//! |------------------|--------------------------|----------------------------------|
//! | `byte`           | UTF-8 byte               | tree-sitter, regex, file I/O     |
//! | `char`           | Unicode scalar value     | ropey, all cursor arithmetic     |
//! | `grapheme`       | extended grapheme cluster| caret movement, deletion         |
//! | `display column` | terminal cells (tabs expanded, wide chars = 2) | rulers, column selection |
//! | `utf16`          | UTF-16 code unit         | LSP                              |
//!
//! The canonical document offset throughout Apex is the **char** offset; it is
//! what [`ropey`] indexes by, and converting to the other spaces is `O(log n)`.
//!
//! # Dependencies and callers
//!
//! Depends only on `ropey`, `unicode-segmentation`, `unicode-width`,
//! `encoding_rs` and `memchr`. `apex-edit` builds documents and selections on
//! top of it; `apex-syntax` and the UI use the byte/char conversions when
//! talking to tree-sitter and when laying out lines. Every function clamps
//! out-of-range input rather than panicking, so stale positions are safe.

pub mod encoding;
pub mod line_ending;
pub mod motion;
pub mod position;

pub use encoding::{DecodeOutcome, Encoding, decode, detect_binary, encode};
pub use line_ending::LineEnding;
pub use position::{Position, display_col_to_char_col, display_width_of};
pub use ropey::{Rope, RopeSlice};
