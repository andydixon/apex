//! Text-based bracket matching (used until a syntax tree offers better data).

use ropey::Rope;

/// Opening bracket characters; index `k` pairs with `CLOSE`'s index `k`.
const OPEN: &str = "([{";
/// Closing bracket characters, in the same order as `OPEN`.
const CLOSE: &str = ")]}";

/// Upper bound on characters scanned so a pathological file cannot stall the UI.
const SCAN_LIMIT: usize = 2_000_000;

/// Partner of bracket `c` and the scan direction needed to find it.
///
/// Returns `(partner, true)` for an opening bracket (search forwards),
/// `(partner, false)` for a closing one (search backwards), and `None` for
/// any other character.
fn partner(c: char) -> Option<(char, bool)> {
    match c {
        '(' => Some((')', true)),
        '[' => Some((']', true)),
        '{' => Some(('}', true)),
        ')' => Some(('(', false)),
        ']' => Some(('[', false)),
        '}' => Some(('{', false)),
        _ => None,
    }
}

/// Find the bracket matching the one at `pos` (or just before it). Returns
/// `(open_pos, close_pos)`. Strings and comments are not understood here.
///
/// `pos` is a char index. The character at `pos` is preferred; the one at
/// `pos - 1` is tried next so a cursor just after a bracket still matches.
/// Both returned values are char indices. Returns `None` when there is no
/// bracket at either position, it is unbalanced, or the scan exceeds
/// `SCAN_LIMIT`.
pub fn matching_bracket(rope: &Rope, pos: usize) -> Option<(usize, usize)> {
    let n = rope.len_chars();
    let at = |p: usize| rope.get_char(p);
    let candidate = if pos < n && partner(at(pos)?).is_some() {
        pos
    } else if pos > 0 && at(pos - 1).and_then(partner).is_some() {
        pos - 1
    } else {
        return None;
    };
    let c = at(candidate)?;
    let (want, forward) = partner(c)?;
    let mut depth = 0usize;
    let mut scanned = 0usize;
    if forward {
        // The scan starts on the bracket itself, so the first iteration takes
        // `depth` to 1 and `i` is the offset from `candidate`.
        for (i, ch) in rope.chars_at(candidate).enumerate() {
            scanned += 1;
            if scanned > SCAN_LIMIT {
                return None;
            }
            if ch == c {
                depth += 1;
            } else if ch == want {
                depth -= 1;
                if depth == 0 {
                    return Some((candidate, candidate + i));
                }
            }
        }
        None
    } else {
        // Start just after the bracket and walk backwards so the bracket itself is
        // the first character seen; `i` tracks the absolute char index.
        let mut it = rope.chars_at(candidate + 1);
        let mut i = candidate + 1;
        while let Some(ch) = it.prev() {
            i -= 1;
            scanned += 1;
            if scanned > SCAN_LIMIT {
                return None;
            }
            if ch == c {
                depth += 1;
            } else if ch == want {
                depth -= 1;
                if depth == 0 {
                    return Some((i, candidate));
                }
            }
        }
        None
    }
}

/// Innermost bracket pair enclosing `pos` (both positions are the brackets
/// themselves). Scans outward with a nesting count.
///
/// `pos` is a char index (clamped to the document length); returned values
/// are char indices. Each bracket kind keeps its own nesting count, so
/// mismatched kinds do not cancel one another. Returns `None` when nothing
/// encloses `pos`, the innermost opener is unbalanced, or the scan exceeds
/// `SCAN_LIMIT`.
pub fn enclosing_brackets(rope: &Rope, pos: usize) -> Option<(usize, usize)> {
    let mut it = rope.chars_at(pos.min(rope.len_chars()));
    let mut i = pos.min(rope.len_chars());
    let mut depth: [isize; 3] = [0; 3];
    let mut scanned = 0usize;
    let idx = |c: char| OPEN.find(c).or_else(|| CLOSE.find(c));
    while let Some(ch) = it.prev() {
        i -= 1;
        scanned += 1;
        if scanned > SCAN_LIMIT {
            return None;
        }
        if let Some(k) = idx(ch) {
            // Walking backwards, a closer opens a nested pair we must skip, and an
            // opener with no pending closer of its kind is the enclosing one.
            if CLOSE.contains(ch) {
                depth[k] += 1;
            } else if depth[k] > 0 {
                depth[k] -= 1;
            } else {
                let (open, close) = matching_bracket(rope, i)?;
                // An opener whose partner closes before `pos` does not enclose it.
                return if close >= pos { Some((open, close)) } else { None };
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins down matching from either side of a bracket, nested enclosure,
    /// non-bracket positions and an unbalanced opener.
    #[test]
    fn matches_and_encloses() {
        let r = Rope::from_str("f(a, [b, {c}], d)");
        assert_eq!(matching_bracket(&r, 1), Some((1, 16)));
        assert_eq!(matching_bracket(&r, 17), Some((1, 16)));
        assert_eq!(matching_bracket(&r, 5), Some((5, 12)));
        assert_eq!(matching_bracket(&r, 3), None);
        assert_eq!(enclosing_brackets(&r, 10), Some((9, 11)));
        assert_eq!(enclosing_brackets(&r, 7), Some((5, 12)));
        assert_eq!(enclosing_brackets(&r, 14), Some((1, 16)));
        assert_eq!(enclosing_brackets(&r, 0), None);
        assert_eq!(matching_bracket(&Rope::from_str("(("), 0), None);
    }
}
