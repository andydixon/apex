//! Line-ending detection and conversion.

use ropey::Rope;

/// The line terminator used when writing a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
            Self::Cr => "CR",
        }
    }

    /// Platform-native default (`CRLF` on Windows, `LF` elsewhere).
    pub const fn native() -> Self {
        if cfg!(windows) { Self::CrLf } else { Self::Lf }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "LF" | "UNIX" => Some(Self::Lf),
            "CRLF" | "WINDOWS" | "DOS" => Some(Self::CrLf),
            "CR" | "MAC" => Some(Self::Cr),
            _ => None,
        }
    }
}

/// Result of scanning text for line terminators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineEndingStats {
    pub lf: usize,
    pub crlf: usize,
    pub cr: usize,
}

impl LineEndingStats {
    /// Dominant terminator, or `fallback` if the text has none.
    pub fn dominant(&self, fallback: LineEnding) -> LineEnding {
        if self.lf == 0 && self.crlf == 0 && self.cr == 0 {
            return fallback;
        }
        if self.crlf >= self.lf && self.crlf >= self.cr {
            LineEnding::CrLf
        } else if self.lf >= self.cr {
            LineEnding::Lf
        } else {
            LineEnding::Cr
        }
    }

    /// True when more than one terminator style is present.
    pub fn is_mixed(&self) -> bool {
        (self.lf > 0) as u8 + (self.crlf > 0) as u8 + (self.cr > 0) as u8 > 1
    }
}

/// Count terminators in `text`. Bounded to the first `limit` bytes for speed on
/// huge files (pass `usize::MAX` for the whole thing).
pub fn scan(text: &str, limit: usize) -> LineEndingStats {
    let bytes = &text.as_bytes()[..text.len().min(limit)];
    let mut s = LineEndingStats { lf: 0, crlf: 0, cr: 0 };
    let mut i = 0;
    for pos in memchr::memchr2_iter(b'\n', b'\r', bytes) {
        if pos < i {
            continue; // the \n of a \r\n already consumed
        }
        match bytes[pos] {
            b'\n' => s.lf += 1,
            _ if bytes.get(pos + 1) == Some(&b'\n') => {
                s.crlf += 1;
                i = pos + 2;
            }
            _ => s.cr += 1,
        }
    }
    s
}

/// Scan a rope without materialising it.
pub fn scan_rope(rope: &Rope, limit: usize) -> LineEndingStats {
    let mut s = LineEndingStats { lf: 0, crlf: 0, cr: 0 };
    let mut seen = 0usize;
    let mut prev_cr = false;
    for chunk in rope.chunks() {
        if seen >= limit {
            break;
        }
        for &b in chunk.as_bytes() {
            match b {
                b'\n' if prev_cr => {
                    s.cr -= 1;
                    s.crlf += 1;
                }
                b'\n' => s.lf += 1,
                b'\r' => {
                    s.cr += 1;
                    prev_cr = true;
                    continue;
                }
                _ => {}
            }
            prev_cr = false;
        }
        seen += chunk.len();
    }
    s
}

/// Rewrite every terminator in `text` to `target`.
pub fn normalise(text: &str, target: LineEnding) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str(target.as_str());
            }
            '\n' => out.push_str(target.as_str()),
            _ => out.push(c),
        }
    }
    out
}

/// Terminator currently ending `line` in `rope`, if any.
pub fn terminator_of_line(rope: &Rope, line: usize) -> Option<LineEnding> {
    if line >= rope.len_lines() {
        return None;
    }
    let l = rope.line(line);
    let n = l.len_chars();
    if n == 0 {
        return None;
    }
    match l.char(n - 1) {
        '\n' if n >= 2 && l.char(n - 2) == '\r' => Some(LineEnding::CrLf),
        '\n' => Some(LineEnding::Lf),
        '\r' => Some(LineEnding::Cr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dominant_and_mixed() {
        let s = scan("a\r\nb\r\nc\nd\re", usize::MAX);
        assert_eq!(s, LineEndingStats { lf: 1, crlf: 2, cr: 1 });
        assert_eq!(s.dominant(LineEnding::Lf), LineEnding::CrLf);
        assert!(s.is_mixed());
        assert_eq!(scan("", usize::MAX).dominant(LineEnding::Cr), LineEnding::Cr);
        assert_eq!(scan_rope(&Rope::from_str("a\r\nb\r\nc\nd\re"), usize::MAX), s);
    }

    #[test]
    fn normalises() {
        assert_eq!(normalise("a\r\nb\rc\nd", LineEnding::Lf), "a\nb\nc\nd");
        assert_eq!(normalise("a\nb", LineEnding::CrLf), "a\r\nb");
        assert_eq!(normalise("a\r\nb", LineEnding::Cr), "a\rb");
    }
}
