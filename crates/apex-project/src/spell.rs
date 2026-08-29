//! Spell checking against word-list dictionaries (Hunspell `.dic` word lists
//! or plain one-word-per-line files) with a user dictionary and suggestions.
//!
//! No dictionary is bundled; the checker looks in the usual system locations
//! and the configuration directory, and reports clearly when none is found.
//! Affix rules in `.aff` files are not interpreted (documented limitation), so
//! inflected forms absent from the `.dic` list are flagged.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Dictionary {
    words: HashSet<String>,
    /// Words whose capitalisation matters (proper nouns) kept as written.
    exact: HashSet<String>,
    user: HashSet<String>,
    ignored: HashSet<String>,
    pub language: String,
    pub source: Option<PathBuf>,
    pub user_path: Option<PathBuf>,
}

/// Candidate dictionary files for a language code such as `en_GB`.
pub fn candidate_paths(language: &str, config_dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![config_dir.join("dictionaries").join(format!("{language}.dic")), config_dir.join("dictionaries").join(format!("{language}.txt"))];
    for base in
        ["/usr/share/hunspell", "/usr/share/myspell", "/usr/share/myspell/dicts", "/usr/local/share/hunspell", "/Library/Spelling", "/System/Library/Spelling"]
    {
        v.push(PathBuf::from(base).join(format!("{language}.dic")));
        if let Some(short) = language.split(['_', '-']).next() {
            v.push(PathBuf::from(base).join(format!("{short}.dic")));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(&home).join("Library/Spelling").join(format!("{language}.dic")));
        v.push(PathBuf::from(&home).join(".local/share/hunspell").join(format!("{language}.dic")));
    }
    v.push(PathBuf::from("/usr/share/dict/words"));
    v
}

impl Dictionary {
    /// Load the first readable candidate for `language`.
    pub fn load(language: &str, config_dir: &Path) -> Option<Dictionary> {
        for p in candidate_paths(language, config_dir) {
            if let Ok(d) = Dictionary::from_file(&p, language) {
                let mut d = d;
                d.user_path = Some(config_dir.join("dictionaries").join("user.txt"));
                d.load_user();
                return Some(d);
            }
        }
        None
    }

    pub fn from_file(path: &Path, language: &str) -> std::io::Result<Dictionary> {
        let text = std::fs::read_to_string(path)?;
        let mut d = Dictionary { language: language.to_string(), source: Some(path.to_path_buf()), ..Default::default() };
        d.add_wordlist(&text);
        if d.words.is_empty() {
            return Err(std::io::Error::other("empty dictionary"));
        }
        Ok(d)
    }

    /// Parse a Hunspell `.dic` (count line, `word/FLAGS`) or a plain list.
    pub fn add_wordlist(&mut self, text: &str) {
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || (i == 0 && line.chars().all(|c| c.is_ascii_digit())) {
                continue;
            }
            let word = line.split(['/', '\t']).next().unwrap_or(line).trim();
            if word.is_empty() || word.chars().any(|c| c.is_ascii_digit()) {
                continue;
            }
            self.insert(word);
        }
    }

    fn insert(&mut self, word: &str) {
        let lower = word.to_lowercase();
        if word.chars().next().is_some_and(|c| c.is_uppercase()) && word != lower {
            self.exact.insert(word.to_string());
        }
        self.words.insert(lower);
    }

    fn load_user(&mut self) {
        if let Some(p) = &self.user_path
            && let Ok(t) = std::fs::read_to_string(p)
        {
            for w in t.lines().map(str::trim).filter(|w| !w.is_empty()) {
                self.user.insert(w.to_lowercase());
            }
        }
    }

    /// Add to the user dictionary (persisted).
    pub fn add_user_word(&mut self, word: &str) -> std::io::Result<()> {
        let w = word.trim().to_lowercase();
        if w.is_empty() {
            return Ok(());
        }
        self.user.insert(w.clone());
        if let Some(p) = &self.user_path {
            if let Some(parent) = p.parent() {
                crate::paths::ensure_private_dir(parent)?;
            }
            let mut all: Vec<&String> = self.user.iter().collect();
            all.sort();
            let body: String = all.iter().map(|w| format!("{w}\n")).collect();
            crate::fileio::save_atomic(p, body.as_bytes())?;
        }
        Ok(())
    }

    /// Ignore for this session only.
    pub fn ignore(&mut self, word: &str) {
        self.ignored.insert(word.to_lowercase());
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Is `word` acceptable? Case rules: all-caps and capitalised forms of a
    /// known word are fine; a known proper noun must keep its capitalisation.
    pub fn check(&self, word: &str) -> bool {
        if word.chars().count() < 2 || word.chars().any(|c| c.is_ascii_digit()) {
            return true;
        }
        let lower = word.to_lowercase();
        if self.user.contains(&lower) || self.ignored.contains(&lower) {
            return true;
        }
        if self.exact.contains(word) {
            return true;
        }
        if !self.words.contains(&lower) {
            // Possessive and plural possessive forms.
            if let Some(stem) = lower.strip_suffix("'s").or_else(|| lower.strip_suffix("s'")) {
                return self.words.contains(stem);
            }
            return false;
        }
        // Known as a proper noun only? Then lowercase/other casing is wrong.
        let has_exact_variant = self.exact.iter().any(|e| e.to_lowercase() == lower);
        if has_exact_variant && !self.exact.contains(word) && word != word.to_uppercase() {
            return false;
        }
        true
    }

    /// Suggestions by edit distance (1, then 2), preserving initial capitalisation.
    pub fn suggest(&self, word: &str, limit: usize) -> Vec<String> {
        let lower = word.to_lowercase();
        let capitalised = word.chars().next().is_some_and(|c| c.is_uppercase());
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let e1 = edits1(&lower);
        for c in &e1 {
            if self.words.contains(c) && seen.insert(c.clone()) {
                out.push(c.clone());
            }
        }
        if out.len() < limit && lower.chars().count() <= 12 {
            for c in &e1 {
                if out.len() >= limit * 4 {
                    break;
                }
                for c2 in edits1(c) {
                    if self.words.contains(&c2) && seen.insert(c2.clone()) {
                        out.push(c2);
                    }
                }
            }
        }
        // Prefer candidates with the same length and first letter.
        out.sort_by_key(|c| (c.chars().next() != lower.chars().next(), (c.chars().count() as i64 - lower.chars().count() as i64).abs(), c.clone()));
        out.truncate(limit);
        if capitalised {
            out = out
                .into_iter()
                .map(|c| {
                    let mut cs = c.chars();
                    match cs.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                        None => c,
                    }
                })
                .collect();
        }
        out
    }
}

fn edits1(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let n = chars.len();
    let alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyz'".chars().collect();
    let mut out = Vec::with_capacity(n * 54);
    for i in 0..=n {
        if i < n {
            // deletion
            let mut s: String = chars[..i].iter().collect();
            s.extend(chars[i + 1..].iter());
            out.push(s);
            // transposition
            if i + 1 < n {
                let mut t = chars.clone();
                t.swap(i, i + 1);
                out.push(t.iter().collect());
            }
            // replacement
            for &a in &alphabet {
                if a != chars[i] {
                    let mut r = chars.clone();
                    r[i] = a;
                    out.push(r.iter().collect());
                }
            }
        }
        // insertion
        for &a in &alphabet {
            let mut s: String = chars[..i].iter().collect();
            s.push(a);
            s.extend(chars[i..].iter());
            out.push(s);
        }
    }
    out
}

/// Split text into checkable words with their char offsets. Tokens that look
/// like code (digits, underscores, paths, URLs, camelCase) are skipped whole.
pub fn words(text: &str) -> Vec<(usize, usize, &str)> {
    const BREAKS: &str = ",;!?()\"“”‘’[]{}*#<>=+&|`-\\";
    let mut out = Vec::new();
    let mut tokens: Vec<(usize, usize)> = Vec::new(); // byte ranges
    let mut start: Option<usize> = None;
    for (bi, c) in text.char_indices() {
        let is_break = c.is_whitespace() || BREAKS.contains(c);
        match (is_break, start) {
            (true, Some(s)) => {
                tokens.push((s, bi));
                start = None;
            }
            (false, None) => start = Some(bi),
            _ => {}
        }
    }
    if let Some(s) = start {
        tokens.push((s, text.len()));
    }
    // Char offset of every byte start, computed lazily via a running count.
    let mut char_at_byte = 0usize;
    let mut last_byte = 0usize;
    for (bs, be) in tokens {
        char_at_byte += text[last_byte..bs].chars().count();
        last_byte = bs;
        let raw = &text[bs..be];
        let trimmed = raw.trim_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"'));
        if trimmed.is_empty() {
            continue;
        }
        let lead = raw.find(trimmed).unwrap_or(0);
        let ok_chars = trimmed.chars().all(|c| c.is_alphabetic() || c == '\'');
        let mut prev_lower = false;
        let mut camel = false;
        for c in trimmed.chars() {
            if c.is_uppercase() && prev_lower {
                camel = true;
            }
            prev_lower = c.is_lowercase();
        }
        if ok_chars && !camel {
            let cs = char_at_byte + raw[..lead].chars().count();
            out.push((cs, cs + trimmed.chars().count(), trimmed));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> Dictionary {
        let mut d = Dictionary::default();
        d.add_wordlist("5\nhello/S\nworld\ncolour\nLondon\nthe\ncat's\nreceive\n");
        d
    }

    #[test]
    fn checks_case_and_possessives() {
        let d = dict();
        assert!(d.check("hello"));
        assert!(d.check("Hello"));
        assert!(d.check("HELLO"));
        assert!(!d.check("helo"));
        assert!(d.check("London"));
        assert!(!d.check("london"));
        assert!(d.check("world's"));
        assert!(d.check("x1"));
        assert!(d.check("a"));
    }

    #[test]
    fn suggests_close_words() {
        let d = dict();
        assert_eq!(d.suggest("helo", 3)[0], "hello");
        assert_eq!(d.suggest("Wrold", 3)[0], "World");
        assert_eq!(d.suggest("recieve", 3)[0], "receive");
        assert!(d.suggest("zzzzzzzz", 3).is_empty());
    }

    #[test]
    fn user_and_ignored_words() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = dict();
        d.user_path = Some(dir.path().join("user.txt"));
        assert!(!d.check("apex"));
        d.add_user_word("Apex").unwrap();
        assert!(d.check("apex"));
        assert_eq!(std::fs::read_to_string(dir.path().join("user.txt")).unwrap(), "apex\n");
        d.ignore("Zorg");
        assert!(d.check("zorg"));
    }

    #[test]
    fn tokeniser_skips_code_like_tokens() {
        let w: Vec<&str> = words("Hello world, it's foo_bar camelCase https://x.y/z v2 'quoted' end.").into_iter().map(|(_, _, w)| w).collect();
        assert_eq!(w, vec!["Hello", "world", "it's", "quoted", "end"]);
        let w: Vec<(usize, usize, &str)> = words("é naïve");
        assert_eq!(w, vec![(0, 1, "é"), (2, 7, "naïve")]);
    }
}
