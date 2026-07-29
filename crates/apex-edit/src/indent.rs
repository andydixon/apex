//! Indentation settings and detection.

use ropey::Rope;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IndentStyle {
    Tabs,
    #[default]
    Spaces,
}

/// Effective indentation configuration for a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndentSettings {
    pub style: IndentStyle,
    /// Width of one indent level in columns (spaces mode) or the visual width of a tab.
    pub indent_width: usize,
    pub tab_width: usize,
}

impl Default for IndentSettings {
    fn default() -> Self {
        Self { style: IndentStyle::Spaces, indent_width: 4, tab_width: 4 }
    }
}

impl IndentSettings {
    /// The string inserted for one level of indentation.
    pub fn unit(&self) -> String {
        match self.style {
            IndentStyle::Tabs => "\t".into(),
            IndentStyle::Spaces => " ".repeat(self.indent_width.max(1)),
        }
    }

    /// Build leading whitespace for `columns` of indentation.
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

    #[test]
    fn whitespace_helpers() {
        let s = IndentSettings { style: IndentStyle::Tabs, indent_width: 4, tab_width: 4 };
        assert_eq!(s.whitespace_for(10), "\t\t  ");
        assert_eq!(s.width_of("\t\t  x"), 10);
        let s = IndentSettings::default();
        assert_eq!(s.unit(), "    ");
    }
}
