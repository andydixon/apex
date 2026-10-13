//! Snippets with tab stops, placeholders, mirrored fields, choices and
//! variables, in the TextMate/LSP syntax (`$1`, `${1:name}`, `${1|a,b|}`,
//! `$0`, `${TM_FILENAME}`).
//!
//! Expansion is pure: it produces the text and the char ranges of every stop
//! relative to the start of the insertion. The editor turns stops into
//! multi-cursor selections, so mirrored fields edit together for free.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A declarative snippet as stored in TOML.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct SnippetDef {
    /// Word typed before expanding (also the completion label). Required.
    pub trigger: String,
    /// Free-text summary shown alongside the trigger.
    pub description: String,
    /// Language ids this snippet applies to (empty = all).
    pub scope: Vec<String>,
    /// Snippet template in the syntax described in the module docs. Required.
    pub body: String,
}

/// Top-level shape of a snippet TOML file: an array of `[[snippets]]` tables.
#[derive(Debug, Clone, Deserialize)]
pub struct SnippetFile {
    #[serde(default)]
    pub snippets: Vec<SnippetDef>,
}

/// Parse a TOML snippet file. Invalid entries are skipped with a reason.
///
/// Mechanically: a TOML syntax or type error fails the whole file with the
/// parser's message; entries with an empty `trigger` or `body` are dropped
/// silently.
pub fn parse_snippet_file(src: &str) -> Result<Vec<SnippetDef>, String> {
    let f: SnippetFile = toml::from_str(src).map_err(|e| e.to_string())?;
    Ok(f.snippets.into_iter().filter(|s| !s.trigger.is_empty() && !s.body.is_empty()).collect())
}

/// Values for `${VARIABLE}` references.
#[derive(Debug, Clone, Default)]
pub struct Variables {
    /// File name with extension (`TM_FILENAME`); empty for an unsaved buffer.
    pub filename: String,
    /// File name without its extension (`TM_FILENAME_BASE`).
    pub filename_base: String,
    /// Full path of the file (`TM_FILEPATH`).
    pub filepath: String,
    /// Directory containing the file (`TM_DIRECTORY`).
    pub directory: String,
    /// Text of the primary selection (`TM_SELECTED_TEXT`).
    pub selected_text: String,
    /// Most recent clipboard entry (`CLIPBOARD`).
    pub clipboard: String,
    /// Zero-based line index of the caret; `TM_LINE_NUMBER` reports it plus one.
    pub line_number: usize,
    /// Text of the caret's line (`TM_CURRENT_LINE`).
    pub current_line: String,
    /// Word under the caret (`TM_CURRENT_WORD`).
    pub current_word: String,
    /// Pre-formatted local date (`CURRENT_DATE`), `YYYY-MM-DD` from the UI caller.
    pub date: String,
    /// Pre-formatted local time (`CURRENT_TIME`), `HH:MM:SS` from the UI caller.
    pub time: String,
    /// Four-digit year (`CURRENT_YEAR`).
    pub year: String,
}

impl Variables {
    /// Value of variable `name`, accepting both TextMate (`TM_*`) and short
    /// aliases. Returns `None` for an unknown name.
    fn get(&self, name: &str) -> Option<String> {
        Some(match name {
            "TM_FILENAME" | "FILENAME" => self.filename.clone(),
            "TM_FILENAME_BASE" | "FILENAME_BASE" => self.filename_base.clone(),
            "TM_FILEPATH" | "FILEPATH" => self.filepath.clone(),
            "TM_DIRECTORY" | "DIRECTORY" => self.directory.clone(),
            "TM_SELECTED_TEXT" | "SELECTION" => self.selected_text.clone(),
            "CLIPBOARD" => self.clipboard.clone(),
            "TM_LINE_NUMBER" => (self.line_number + 1).to_string(),
            "TM_LINE_INDEX" => self.line_number.to_string(),
            "TM_CURRENT_LINE" => self.current_line.clone(),
            "TM_CURRENT_WORD" => self.current_word.clone(),
            "CURRENT_DATE" | "DATE" => self.date.clone(),
            "CURRENT_TIME" | "TIME" => self.time.clone(),
            "CURRENT_YEAR" | "YEAR" => self.year.clone(),
            _ => return None,
        })
    }
}

/// The result of expanding a snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// Expanded text, already indented and using the requested line ending.
    pub text: String,
    /// Stops in tab order: index 1.. then 0 last. Each has one or more
    /// (start, end) char ranges relative to the expansion start.
    pub stops: Vec<(u32, Vec<(usize, usize)>)>,
    /// Choices per stop index.
    pub choices: BTreeMap<u32, Vec<String>>,
}

impl Expansion {
    /// The caret position when the snippet has no stops at all.
    ///
    /// Returns the length of `text` in chars, i.e. a char offset relative to the
    /// insertion start.
    pub fn end(&self) -> usize {
        self.text.chars().count()
    }
}

/// Single-pass recursive-descent parser that writes expanded text and
/// records stop ranges as it goes.
struct Parser<'a> {
    /// Remaining template input.
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    /// Values for `$NAME` / `${NAME}` references.
    vars: &'a Variables,
    /// Expanded text so far (not yet indented).
    out: String,
    /// Length of `out` in chars, kept alongside so stop offsets are char offsets
    /// without recounting.
    len: usize,
    /// Ranges (char offsets into `out`) per stop index, in the order seen.
    stops: BTreeMap<u32, Vec<(usize, usize)>>,
    /// Options for `${n|a,b|}` stops.
    choices: BTreeMap<u32, Vec<String>>,
    /// Placeholder text recorded per stop so later bare `$n` mirrors reuse it.
    placeholders: BTreeMap<u32, String>,
    /// Current `${...}` nesting depth, capped to bound recursion.
    depth: u32,
}

impl<'a> Parser<'a> {
    /// Append `s` to the output, keeping `len` in step.
    fn push_str(&mut self, s: &str) {
        self.out.push_str(s);
        self.len += s.chars().count();
    }

    /// Append one char to the output, keeping `len` in step.
    fn push(&mut self, c: char) {
        self.out.push(c);
        self.len += 1;
    }

    /// Parse input until `terminator` (consumed) or end of input when `None`.
    /// A backslash makes the next char literal.
    fn parse_until(&mut self, terminator: Option<char>) {
        while let Some(c) = self.chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = self.chars.next() {
                        self.push(n);
                    }
                }
                '$' => self.parse_dollar(),
                c if Some(c) == terminator => return,
                c => self.push(c),
            }
        }
    }

    /// Handle the input after a `$`: `${...}`, a bare `$n` stop (filled with
    /// that stop's earlier placeholder, if any), or a bare `$NAME` variable.
    /// Unknown variables and a lone `$` are emitted literally.
    fn parse_dollar(&mut self) {
        match self.chars.peek().copied() {
            Some('{') => {
                self.chars.next();
                self.parse_braced();
            }
            Some(d) if d.is_ascii_digit() => {
                let idx = self.read_number();
                let start = self.len;
                if let Some(p) = self.placeholders.get(&idx).cloned() {
                    self.push_str(&p);
                }
                self.stops.entry(idx).or_default().push((start, self.len));
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let name = self.read_ident();
                match self.vars.get(&name) {
                    Some(v) => self.push_str(&v),
                    None => self.push_str(&format!("${name}")),
                }
            }
            _ => self.push('$'),
        }
    }

    /// Consume a run of ASCII digits as a stop index, saturating at `u32::MAX`.
    fn read_number(&mut self) -> u32 {
        let mut n = 0u32;
        while let Some(d) = self.chars.peek().copied().filter(|c| c.is_ascii_digit()) {
            n = n.saturating_mul(10).saturating_add(d as u32 - '0' as u32);
            self.chars.next();
        }
        n
    }

    /// Consume a run of ASCII alphanumerics and underscores as a variable name.
    fn read_ident(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.chars.peek().copied().filter(|c| c.is_ascii_alphanumeric() || *c == '_') {
            s.push(c);
            self.chars.next();
        }
        s
    }

    /// Parse the body of `${...}` after the opening brace: `${n}`,
    /// `${n:placeholder}` (which may nest), `${n|a,b|}`, `${NAME}`,
    /// `${NAME:default}` or `${NAME/...}`. Anything else emits `${` literally
    /// and leaves the rest for the caller.
    fn parse_braced(&mut self) {
        self.depth += 1;
        if self.depth > 16 {
            // Pathological nesting: emit literally.
            self.push_str("${");
            self.depth -= 1;
            return;
        }
        let start = self.len;
        match self.chars.peek().copied() {
            Some(d) if d.is_ascii_digit() => {
                let idx = self.read_number();
                match self.chars.next() {
                    Some('}') => {
                        if let Some(p) = self.placeholders.get(&idx).cloned() {
                            self.push_str(&p);
                        }
                        self.stops.entry(idx).or_default().push((start, self.len));
                    }
                    Some(':') => {
                        self.parse_until(Some('}'));
                        // Only the first placeholder for an index is kept, so a later
                        // `${1:other}` does not change what bare `$1` mirrors insert.
                        let text: String = self.out.chars().skip(start).collect();
                        self.placeholders.entry(idx).or_insert(text);
                        self.stops.entry(idx).or_default().push((start, self.len));
                    }
                    Some('|') => {
                        let mut choices = Vec::new();
                        let mut cur = String::new();
                        loop {
                            match self.chars.next() {
                                Some('\\') => {
                                    if let Some(n) = self.chars.next() {
                                        cur.push(n);
                                    }
                                }
                                Some(',') => choices.push(std::mem::take(&mut cur)),
                                Some('|') => {
                                    choices.push(std::mem::take(&mut cur));
                                    let _ = self.chars.next(); // closing }
                                    break;
                                }
                                Some(c) => cur.push(c),
                                None => break,
                            }
                        }
                        let first = choices.first().cloned().unwrap_or_default();
                        self.push_str(&first);
                        self.placeholders.entry(idx).or_insert(first);
                        self.choices.insert(idx, choices);
                        self.stops.entry(idx).or_default().push((start, self.len));
                    }
                    // Unexpected char (or end of input) after the index: record an
                    // empty stop; the char itself is dropped.
                    _ => {
                        self.stops.entry(idx).or_default().push((start, self.len));
                    }
                }
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let name = self.read_ident();
                // ${VAR:default} or ${VAR/regex/fmt/} (transforms are not supported: default used).
                let next = self.chars.next();
                let value = self.vars.get(&name);
                match next {
                    Some('}') => self.push_str(&value.unwrap_or_default()),
                    Some(':') => {
                        let before = self.len;
                        self.parse_until(Some('}'));
                        if let Some(v) = value.filter(|v| !v.is_empty()) {
                            // Replace the default with the variable's value.
                            // Any stops recorded inside the default keep their offsets.
                            let default_len = self.len - before;
                            let keep: String = self.out.chars().take(self.out.chars().count() - default_len).collect();
                            self.out = keep;
                            self.len = before;
                            self.push_str(&v);
                        }
                    }
                    _ => {
                        // Skip until the closing brace.
                        for c in self.chars.by_ref() {
                            if c == '}' {
                                break;
                            }
                        }
                        self.push_str(&value.unwrap_or_default());
                    }
                }
            }
            _ => self.push_str("${"),
        }
        self.depth -= 1;
    }
}

/// Expand `body` with `vars`, indenting continuation lines with `indent`
/// and using `line_ending` between lines.
///
/// `indent` is the leading whitespace of the insertion line; `line_ending`
/// must be non-empty. Unknown variables in `${NAME}` form expand to an
/// empty string. If the body has no `$0`, no index-0 stop is added and
/// callers place the caret at `Expansion::end`.
pub fn expand(body: &str, vars: &Variables, indent: &str, line_ending: &str) -> Expansion {
    // Normalise line endings in the template, then indent continuation lines.
    let normalised: String = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut p = Parser {
        chars: normalised.chars().peekable(),
        vars,
        out: String::with_capacity(normalised.len() + 16),
        len: 0,
        stops: BTreeMap::new(),
        choices: BTreeMap::new(),
        placeholders: BTreeMap::new(),
        depth: 0,
    };
    p.parse_until(None);
    // Apply indentation after parsing so stop offsets can be corrected exactly.
    let (text, stops) = indent_expansion(&p.out, &p.stops, indent, line_ending);
    let mut ordered: Vec<(u32, Vec<(usize, usize)>)> = stops.into_iter().filter(|(i, _)| *i != 0).collect();
    ordered.sort_by_key(|(i, _)| *i);
    if let Some(zero) = p.stops.get(&0) {
        // This block has no effect; `$0` is appended below.
        let _ = zero;
    }
    // Recompute $0 after indentation from the indented map.
    let zero = indent_expansion(&p.out, &p.stops, indent, line_ending).1.remove(&0);
    if let Some(z) = zero {
        ordered.push((0, z));
    }
    Expansion { text, stops: ordered, choices: p.choices }
}

/// Insert `indent` after every newline and shift stop offsets accordingly.
///
/// Each `\n` in `text` becomes `line_ending` followed by `indent`. Stop
/// offsets are char offsets in and out; out-of-range offsets are clamped
/// to the end. Panics if `line_ending` is empty.
fn indent_expansion(text: &str, stops: &BTreeMap<u32, Vec<(usize, usize)>>, indent: &str, line_ending: &str) -> (String, BTreeMap<u32, Vec<(usize, usize)>>) {
    let mut out = String::with_capacity(text.len() + 32);
    // shift[i] = number of chars inserted before original char index i
    let mut shifts: Vec<usize> = Vec::with_capacity(text.chars().count() + 1);
    let mut added = 0usize;
    let ending_extra = line_ending.chars().count() - 1;
    let indent_chars = indent.chars().count();
    for c in text.chars() {
        shifts.push(added);
        if c == '\n' {
            out.push_str(line_ending);
            out.push_str(indent);
            added += ending_extra + indent_chars;
        } else {
            out.push(c);
        }
    }
    shifts.push(added);
    let mapped = stops
        .iter()
        .map(|(i, ranges)| (*i, ranges.iter().map(|(a, b)| (a + shifts[(*a).min(shifts.len() - 1)], b + shifts[(*b).min(shifts.len() - 1)])).collect()))
        .collect();
    (out, mapped)
}

/// Built-in snippets shipped with the editor, as TOML sources.
///
/// Each entry is `(language id, TOML source)`; some languages share a file.
pub const BUILTIN_SNIPPET_SOURCES: &[(&str, &str)] = &[
    ("rust", include_str!("../../../assets/snippets/rust.toml")),
    ("python", include_str!("../../../assets/snippets/python.toml")),
    ("javascript", include_str!("../../../assets/snippets/javascript.toml")),
    ("typescript", include_str!("../../../assets/snippets/javascript.toml")),
    ("go", include_str!("../../../assets/snippets/go.toml")),
    ("c", include_str!("../../../assets/snippets/c.toml")),
    ("cpp", include_str!("../../../assets/snippets/c.toml")),
    ("html", include_str!("../../../assets/snippets/html.toml")),
    ("markdown", include_str!("../../../assets/snippets/markdown.toml")),
    ("bash", include_str!("../../../assets/snippets/bash.toml")),
];

/// All built-in snippets, tagged with their language scope.
///
/// Snippets without an explicit `scope` get the language of the file they
/// came from. A malformed built-in file is logged and skipped.
pub fn builtin_snippets() -> Vec<SnippetDef> {
    let mut out = Vec::new();
    for (lang, src) in BUILTIN_SNIPPET_SOURCES {
        match parse_snippet_file(src) {
            Ok(list) => out.extend(list.into_iter().map(|mut s| {
                if s.scope.is_empty() {
                    s.scope = vec![lang.to_string()];
                }
                s
            })),
            Err(e) => tracing::error!("built-in snippets for {lang}: {e}"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins down stop ordering (`$0` last), mirror text, choice defaults and
    /// offsets after continuation-line indentation.
    #[test]
    fn stops_placeholders_mirrors_and_choices() {
        let e = expand("fn ${1:name}($2) -> ${3|(),bool|} {\n    $1$0\n}", &Variables::default(), "  ", "\n");
        assert_eq!(e.text, "fn name() -> () {\n      name\n  }");
        assert_eq!(e.stops[0], (1, vec![(3, 7), (24, 28)]));
        assert_eq!(e.stops[1], (2, vec![(8, 8)]));
        assert_eq!(e.stops[2], (3, vec![(13, 15)]));
        assert_eq!(e.stops[3], (0, vec![(28, 28)]));
        assert_eq!(e.choices[&3], vec!["()", "bool"]);
    }

    /// Pins down variable lookup, defaults for unknown variables, `\$` escapes
    /// and CRLF line endings.
    #[test]
    fn variables_escapes_and_crlf() {
        let vars = Variables { filename: "main.rs".into(), year: "2026".into(), ..Default::default() };
        let e = expand("// $TM_FILENAME ${CURRENT_YEAR} ${UNKNOWN:dflt} \\$x\nnext", &vars, "", "\r\n");
        assert_eq!(e.text, "// main.rs 2026 dflt $x\r\nnext");
        assert!(e.stops.is_empty());
        assert_eq!(e.end(), e.text.chars().count());
    }

    /// Pins down that every bundled snippet file parses and expands to
    /// non-empty text.
    #[test]
    fn builtin_snippets_parse() {
        let all = builtin_snippets();
        assert!(all.len() > 30);
        assert!(all.iter().any(|s| s.trigger == "fn" && s.scope == vec!["rust".to_string()]));
        for s in &all {
            let e = expand(&s.body, &Variables::default(), "", "\n");
            assert!(!e.text.is_empty(), "{}", s.trigger);
        }
    }

    /// Pins down that truncated or deeply nested templates expand without
    /// panicking.
    #[test]
    fn malformed_input_never_panics() {
        for src in ["${", "$", "${1", "${1:", "${1|a,b", "${|}", "$999999999999", "${VAR", "\\", "${1:${2:${3:${4:deep}}}}"] {
            let _ = expand(src, &Variables::default(), "    ", "\n");
        }
    }
}
