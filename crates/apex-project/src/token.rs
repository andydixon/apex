//! Secrets for local authentication: the single-instance socket token and
//! the per-job askpass token.
//!
//! Both guard a loopback listener against other local processes, so they
//! must be unguessable: 256 bits from the operating system's CSPRNG
//! (`getrandom`), never a hash of the pid and time. Comparison is constant
//! time so a wrong guess learns nothing from how long the check took.

/// 64 lowercase hex digits (256 bits) from the OS random source, or `None`
/// if the OS cannot supply randomness. Callers fail closed on `None` (no
/// listener is started) rather than fall back to a weaker token.
pub fn random_token() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether `a` equals `b`, taking the same time for every input of the same
/// length (the length itself is not secret: every token is 64 digits).
pub fn tokens_equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokens are 256-bit hex, distinct per call, and compare correctly.
    #[test]
    fn tokens_are_random_hex_and_compare_exactly() {
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(tokens_equal(&a, &a.clone()));
        assert!(!tokens_equal(&a, &b));
        assert!(!tokens_equal(&a, &a[..63]));
        assert!(!tokens_equal("", "x"));
    }
}
