//! Indentation settings and detection.

use ropey::Rope;
use serde::{Deserialize, Serialize};

/// Character used for indentation. Serialised as `tabs` / `spaces`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IndentStyle {
    /// Indent with tab characters (padded with spaces for partial levels).
    Tabs,
    /// Indent with spaces only.
    #[default]
    Spaces,
}

/// Effective indentation configuration for a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndentSettings {
    /// Whether indentation is written as tabs or spaces.
    pub style: IndentStyle,
    /// Width of one indent level in columns (spaces mode) or the visual width of a tab.
    pub indent_width: usize,
    /// Number of columns a tab character advances to (the next multiple of this).
    pub tab_width: usize,
}

impl Default for IndentSettings {
    fn default() -> Self {
        Self { style: IndentStyle::Spaces, indent_width: 4, tab_width: 4 }
    }
}

impl IndentSettings {
    /// The string inserted for one level of indentation.
    ///
    /// One tab in tabs mode, or `indent_width` spaces (at least one) in spaces
    /// mode.
    pub fn unit(&self) -> String {
        match self.style {
            IndentStyle::Tabs => "\t".into(),
            IndentStyle::Spaces => " ".repeat(self.indent_width.max(1)),
        }
    }

    /// Build leading whitespace for `columns` of indentation.
    ///
    /// `columns` is a display width. In tabs mode as many whole tabs as fit are
    /// used and any remainder is padded with spaces.
    pub fn whitespace_for(&self, columns: usize) -> String {
        match self.style {
            IndentStyle::Tabs => {
                let tw = self.tab_width.max(1);
                let mut s = "\t".repeat(columns / tw);
                s.push_str(&" ".repeat(columns % tw));
                s
            }
            IndentStyle::Spaces => " ".repeat(columns),
        }
    }

    /// Display width of a leading-whitespace string.
    ///
    /// Returns display columns, expanding tabs to the next tab stop. Scanning
    /// stops at the first character that is neither a space nor a tab, so `ws`
    /// may be a whole line.
    pub fn width_of(&self, ws: &str) -> usize {
        let tw = self.tab_width.max(1);
        let mut col = 0;
        for c in ws.chars() {
            match c {
                '\t' => col += tw - col % tw,
                ' ' => col += 1,
                _ => break,
            }
        }
        col
    }

    /// Short human-readable summary such as `Tabs: 4` or `Spaces: 2`.
    pub fn label(&self) -> String {
        match self.style {
            IndentStyle::Tabs => format!("Tabs: {}", self.tab_width),
            IndentStyle::Spaces => format!("Spaces: {}", self.indent_width),
        }
    }
}

/// Guess the indentation style used by `rope` from its first `max_lines` lines.
///
/// Returns `None` when there is not enough evidence; callers keep their
/// configured default in that case.
///
/// A line counts as tab-indented only when tabs come before any space. The
/// detected tab-mode result always uses a width of 4; the spaces-mode
/// width is the most frequent indentation change between consecutive
/// space-indented lines (1 to 8 columns).
pub fn detect_indent(rope: &Rope, max_lines: usize) -> Option<IndentSettings> {
    let mut tab_lines = 0usize;
    let mut space_lines = 0usize;
    // Histogram of indentation deltas between consecutive space-indented lines.
    let mut deltas = [0usize; 9];
    let mut prev_spaces: Option<usize> = None;
    for line in rope.lines().take(max_lines) {
        let mut spaces = 0usize;
        let mut tabs = 0usize;
        let mut saw_content = false;
        for c in line.chars() {
            match c {
                ' ' => spaces += 1,
                // Tabs after spaces are treated as content (mixed indentation is not
                // counted as tab-indented).
                '\t' if spaces == 0 => tabs += 1,
                '\n' | '\r' => break,
                _ => {
                    saw_content = true;
                    break;
                }
            }
        }
        if !saw_content {
            continue; // blank line: ignore
        }
        if tabs > 0 {
            tab_lines += 1;
            // Do not measure a delta across a tab-indented line.
            prev_spaces = None;
            continue;
        }
        if spaces > 0 {
            space_lines += 1;
        }
        if let Some(p) = prev_spaces {
            let d = spaces.abs_diff(p);
            if (1..=8).contains(&d) {
                deltas[d] += 1;
            }
        }
        prev_spaces = Some(spaces);
    }
    if tab_lines == 0 && space_lines == 0 {
        return None;
    }
    if tab_lines > space_lines {
        return Some(IndentSettings { style: IndentStyle::Tabs, indent_width: 4, tab_width: 4 });
    }
    // Prefer the most common delta; ties resolve towards the smaller width
    // except that 2 and 4 are preferred over odd widths.
    let mut best = 0;
    let mut best_count = 0;
    // The strict `>` below means the earlier width in this list wins a tie.
    for w in [2usize, 4, 8, 3, 1, 5, 6, 7] {
        if deltas[w] > best_count {
            best_count = deltas[w];
            best = w;
        }
    }
    if best == 0 {
        return None;
    }
    Some(IndentSettings { style: IndentStyle::Spaces, indent_width: best, tab_width: 4 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins down detection of two-space and tab indentation, and `None` for
    /// unindented text.
    #[test]
    fn detects_spaces_and_tabs() {
        let r = Rope::from_str("fn a() {\n  if x {\n    y();\n  }\n}\n");
        let s = detect_indent(&r, 100).unwrap();
        assert_eq!(s.style, IndentStyle::Spaces);
        assert_eq!(s.indent_width, 2);
        let r = Rope::from_str("fn a() {\n\tif x {\n\t\ty();\n\t}\n}\n");
        assert_eq!(detect_indent(&r, 100).unwrap().style, IndentStyle::Tabs);
        assert!(detect_indent(&Rope::from_str("a\nb\n"), 100).is_none());
    }

    /// Pins down tab-plus-space padding, tab-stop width measurement and the
    /// default indent unit.
    #[test]
    fn whitespace_helpers() {
        let s = IndentSettings { style: IndentStyle::Tabs, indent_width: 4, tab_width: 4 };
        assert_eq!(s.whitespace_for(10), "\t\t  ");
        assert_eq!(s.width_of("\t\t  x"), 10);
        let s = IndentSettings::default();
        assert_eq!(s.unit(), "    ");
    }
}
