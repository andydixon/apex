//! Text-based bracket matching (used until a syntax tree offers better data).

use ropey::Rope;

const OPEN: &str = "([{";
const CLOSE: &str = ")]}";

/// Upper bound on characters scanned so a pathological file cannot stall the UI.
const SCAN_LIMIT: usize = 2_000_000;

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
            if CLOSE.contains(ch) {
                depth[k] += 1;
            } else if depth[k] > 0 {
                depth[k] -= 1;
            } else {
                let (open, close) = matching_bracket(rope, i)?;
                return if close >= pos { Some((open, close)) } else { None };
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
