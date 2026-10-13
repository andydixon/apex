//! Character-encoding detection, decoding and encoding.
//!
//! Files are treated as untrusted input: decoding never panics, invalid
//! sequences are replaced with U+FFFD and the caller is told that happened so
//! it can warn before a lossy save.
//!
//! The document loader calls [`detect_binary`] first, then [`decode`]; the
//! saver calls [`encode`] with the encoding recorded on the document. UTF-8
//! and UTF-16 are handled here directly (BOM handling, UTF-16 heuristics);
//! everything else is delegated to `encoding_rs` by WHATWG label.
//!
//! Detection is heuristic and looks at a bounded 8 KiB sample, so a file whose
//! first 8 KiB is pure ASCII but which later contains Latin-1 bytes will be
//! decoded as UTF-8 with replacement characters where the bad bytes are.

use std::fmt;

/// A supported text encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Encoding {
    /// UTF-8 without a byte-order mark (the default for new files).
    #[default]
    Utf8,
    /// UTF-8 with a leading `EF BB BF` BOM, preserved on save.
    Utf8Bom,
    /// UTF-16 little-endian; written with a `FF FE` BOM.
    Utf16Le,
    /// UTF-16 big-endian; written with a `FE FF` BOM.
    Utf16Be,
    /// Any other encoding supported by `encoding_rs`, by WHATWG label.
    Legacy(&'static encoding_rs::Encoding),
}

/// The legacy encodings offered in the UI, as WHATWG labels.
///
/// Order is the order shown in the encoding picker; Western European first,
/// then the other single-byte code pages, then the East Asian multi-byte ones.
pub const LEGACY_LABELS: &[&str] = &[
    "windows-1252",
    "iso-8859-2",
    "iso-8859-15",
    "windows-1250",
    "windows-1251",
    "windows-1253",
    "windows-1254",
    "windows-1255",
    "windows-1256",
    "windows-1257",
    "windows-1258",
    "koi8-r",
    "koi8-u",
    "macintosh",
    "shift_jis",
    "euc-jp",
    "iso-2022-jp",
    "gbk",
    "gb18030",
    "big5",
    "euc-kr",
    "ibm866",
];

impl Encoding {
    /// All encodings in the order they are presented to the user.
    pub fn all() -> Vec<Encoding> {
        let mut v = vec![Self::Utf8, Self::Utf8Bom, Self::Utf16Le, Self::Utf16Be];
        v.extend(LEGACY_LABELS.iter().filter_map(|l| Self::from_label(l)));
        v
    }

    /// Parse a label from config or the UI, case-insensitively.
    ///
    /// Apex's own spellings (`utf-8-bom`, `utf-16le`, ...) are checked first;
    /// anything else goes through `encoding_rs`'s WHATWG label table. Labels
    /// that `encoding_rs` maps back to UTF-8/UTF-16 are folded into the
    /// dedicated variants, and the WHATWG "replacement" pseudo-encoding is
    /// rejected because it cannot decode anything.
    pub fn from_label(label: &str) -> Option<Self> {
        match label.to_ascii_lowercase().as_str() {
            "utf-8" | "utf8" => Some(Self::Utf8),
            "utf-8-bom" | "utf-8 with bom" | "utf8-bom" => Some(Self::Utf8Bom),
            "utf-16le" | "utf-16 le" => Some(Self::Utf16Le),
            "utf-16be" | "utf-16 be" => Some(Self::Utf16Be),
            other => {
                let e = encoding_rs::Encoding::for_label(other.as_bytes())?;
                if e == encoding_rs::UTF_8 {
                    Some(Self::Utf8)
                } else if e == encoding_rs::UTF_16LE {
                    Some(Self::Utf16Le)
                } else if e == encoding_rs::UTF_16BE {
                    Some(Self::Utf16Be)
                } else if e == encoding_rs::REPLACEMENT {
                    None
                } else {
                    Some(Self::Legacy(e))
                }
            }
        }
    }

    /// Human-readable name for the status bar and menus.
    pub fn label(&self) -> String {
        match self {
            Self::Utf8 => "UTF-8".into(),
            Self::Utf8Bom => "UTF-8 with BOM".into(),
            Self::Utf16Le => "UTF-16 LE".into(),
            Self::Utf16Be => "UTF-16 BE".into(),
            Self::Legacy(e) => e.name().to_string(),
        }
    }

    /// Stable identifier for configuration files.
    ///
    /// Round-trips through [`Encoding::from_label`].
    pub fn id(&self) -> String {
        match self {
            Self::Utf8 => "utf-8".into(),
            Self::Utf8Bom => "utf-8-bom".into(),
            Self::Utf16Le => "utf-16le".into(),
            Self::Utf16Be => "utf-16be".into(),
            Self::Legacy(e) => e.name().to_ascii_lowercase(),
        }
    }
}

impl fmt::Display for Encoding {
    /// Displays as [`Encoding::label`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// Result of decoding a byte buffer.
#[derive(Debug, Clone)]
pub struct DecodeOutcome {
    /// The decoded text, with any BOM stripped.
    pub text: String,
    /// The encoding actually used (detected or forced); recorded on the
    /// document so saving writes the same encoding back.
    pub encoding: Encoding,
    /// True when invalid sequences were replaced with U+FFFD.
    pub had_errors: bool,
}

/// Heuristic binary detection: a NUL byte within the sample that is not part of
/// a UTF-16 pattern marks the data as binary.
///
/// Only the first 8 KiB are inspected. An empty buffer is text.
pub fn detect_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.is_empty() {
        return false;
    }
    // UTF-16 text is full of NULs; rule it out before the NUL check.
    if bom_of(bytes).is_some() || looks_like_utf16(sample).is_some() {
        return false;
    }
    memchr::memchr(0, sample).is_some()
}

/// Identify a leading byte-order mark, returning the encoding and BOM length in bytes.
fn bom_of(bytes: &[u8]) -> Option<(Encoding, usize)> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some((Encoding::Utf8Bom, 3))
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some((Encoding::Utf16Le, 2))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some((Encoding::Utf16Be, 2))
    } else {
        None
    }
}

/// Detect BOM-less UTF-16 by the distribution of NUL bytes in even/odd positions.
///
/// Needs at least 16 bytes of sample to be meaningful. Only ASCII-heavy text is
/// recognised: UTF-16 text in a non-Latin script has few NULs and will fall
/// through to the UTF-8 check (and then the legacy fallback) instead.
fn looks_like_utf16(sample: &[u8]) -> Option<Encoding> {
    if sample.len() < 16 {
        return None;
    }
    let (mut even_nul, mut odd_nul) = (0usize, 0usize);
    let (mut even_text, mut odd_text) = (0usize, 0usize);
    for (i, &b) in sample.iter().enumerate() {
        let printable = (0x20..0x7f).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t';
        if i % 2 == 0 {
            even_nul += (b == 0) as usize;
            even_text += printable as usize;
        } else {
            odd_nul += (b == 0) as usize;
            odd_text += printable as usize;
        }
    }
    let pairs = sample.len() / 2;
    // ASCII-heavy UTF-16 text has a NUL in roughly every other byte and
    // printable ASCII in the others.
    // Thresholds: >= 30% of the high bytes are NUL, < 5% of the low bytes are
    // NUL, and >= 90% of the low bytes are printable ASCII. The NUL floor is
    // low because non-ASCII chars (accented letters, CJK) have a non-zero high
    // byte; the printable floor is high to keep binary data out.
    if odd_nul * 10 >= pairs * 3 && even_nul * 20 < pairs && even_text * 10 >= pairs * 9 {
        Some(Encoding::Utf16Le)
    } else if even_nul * 10 >= pairs * 3 && odd_nul * 20 < pairs && odd_text * 10 >= pairs * 9 {
        Some(Encoding::Utf16Be)
    } else {
        None
    }
}

/// Decode `bytes`, auto-detecting the encoding unless `forced` is given.
///
/// Detection order: BOM → UTF-16 heuristic → valid UTF-8 → `fallback`
/// (typically Windows-1252, which decodes any byte sequence without error).
pub fn decode(bytes: &[u8], forced: Option<Encoding>, fallback: Encoding) -> DecodeOutcome {
    if let Some(enc) = forced {
        return decode_as(bytes, enc);
    }
    if let Some((enc, _)) = bom_of(bytes) {
        return decode_as(bytes, enc);
    }
    if let Some(enc) = looks_like_utf16(&bytes[..bytes.len().min(8192)]) {
        return decode_as(bytes, enc);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => DecodeOutcome { text: s.to_owned(), encoding: Encoding::Utf8, had_errors: false },
        Err(_) => {
            // The bytes are not UTF-8, so a UTF-8 fallback would just produce
            // U+FFFD noise; substitute Windows-1252, which accepts every byte.
            let fallback = if matches!(fallback, Encoding::Utf8 | Encoding::Utf8Bom) { Encoding::Legacy(encoding_rs::WINDOWS_1252) } else { fallback };
            decode_as(bytes, fallback)
        }
    }
}

/// Decode with a specific encoding; strips a matching BOM.
///
/// A BOM is only stripped when it matches the requested encoding, so forcing
/// UTF-8 on a UTF-16 file shows the `FF FE` bytes as garbage rather than
/// silently dropping them.
pub fn decode_as(bytes: &[u8], encoding: Encoding) -> DecodeOutcome {
    let (rs, bom_len) = match encoding {
        Encoding::Utf8 => (encoding_rs::UTF_8, if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 }),
        Encoding::Utf8Bom => (encoding_rs::UTF_8, if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 }),
        Encoding::Utf16Le => (encoding_rs::UTF_16LE, if bytes.starts_with(&[0xFF, 0xFE]) { 2 } else { 0 }),
        Encoding::Utf16Be => (encoding_rs::UTF_16BE, if bytes.starts_with(&[0xFE, 0xFF]) { 2 } else { 0 }),
        Encoding::Legacy(e) => (e, 0),
    };
    let body = &bytes[bom_len.min(bytes.len())..];
    // We have already handled the BOM ourselves; encoding_rs must not sniff
    // for one again or it could switch encodings behind our back.
    let (cow, had_errors) = rs.decode_without_bom_handling(body);
    DecodeOutcome { text: cow.into_owned(), encoding, had_errors }
}

/// Encode `text` for writing. Returns the bytes and whether any character was
/// unmappable (replaced with a numeric reference by `encoding_rs`).
///
/// UTF-16 output always carries a BOM; UTF-8 only does for `Utf8Bom`. The
/// Unicode encodings can never be lossy, so their flag is always `false`.
pub fn encode(text: &str, encoding: Encoding) -> (Vec<u8>, bool) {
    match encoding {
        Encoding::Utf8 => (text.as_bytes().to_vec(), false),
        Encoding::Utf8Bom => {
            let mut v = Vec::with_capacity(text.len() + 3);
            v.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            v.extend_from_slice(text.as_bytes());
            (v, false)
        }
        Encoding::Utf16Le => {
            let mut v = Vec::with_capacity(text.len() * 2 + 2);
            v.extend_from_slice(&[0xFF, 0xFE]);
            for u in text.encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            (v, false)
        }
        Encoding::Utf16Be => {
            let mut v = Vec::with_capacity(text.len() * 2 + 2);
            v.extend_from_slice(&[0xFE, 0xFF]);
            for u in text.encode_utf16() {
                v.extend_from_slice(&u.to_be_bytes());
            }
            (v, false)
        }
        Encoding::Legacy(e) => {
            let (cow, _, unmappable) = e.encode(text);
            (cow.into_owned(), unmappable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each BOM selects its encoding and is stripped from the text.
    #[test]
    fn detects_boms() {
        assert_eq!(decode(b"\xEF\xBB\xBFhi", None, Encoding::Utf8).encoding, Encoding::Utf8Bom);
        let o = decode(b"\xFF\xFEh\0i\0", None, Encoding::Utf8);
        assert_eq!(o.encoding, Encoding::Utf16Le);
        assert_eq!(o.text, "hi");
        let o = decode(b"\xFE\xFF\0h\0i", None, Encoding::Utf8);
        assert_eq!(o.encoding, Encoding::Utf16Be);
        assert_eq!(o.text, "hi");
    }

    /// The NUL-distribution heuristic catches BOM-less UTF-16 and keeps it out of binary detection.
    #[test]
    fn detects_bomless_utf16() {
        let s: Vec<u8> = "hello world, this is text".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let o = decode(&s, None, Encoding::Utf8);
        assert_eq!(o.encoding, Encoding::Utf16Le);
        assert_eq!(o.text, "hello world, this is text");
        assert!(!detect_binary(&s));
    }

    /// Invalid UTF-8 with a UTF-8 fallback is decoded as Windows-1252 without errors.
    #[test]
    fn invalid_utf8_falls_back_to_cp1252() {
        let o = decode(b"caf\xE9", None, Encoding::Utf8);
        assert_eq!(o.text, "café");
        assert!(matches!(o.encoding, Encoding::Legacy(_)));
        assert!(!o.had_errors);
    }

    /// Forcing UTF-8 on bad bytes substitutes U+FFFD and reports it.
    #[test]
    fn forced_utf8_reports_errors() {
        let o = decode(b"caf\xE9", Some(Encoding::Utf8), Encoding::Utf8);
        assert!(o.had_errors);
        assert_eq!(o.text, "caf\u{FFFD}");
    }

    /// ASCII survives every offered encoding; CJK in cp1252 is flagged lossy.
    #[test]
    fn round_trips() {
        for enc in Encoding::all() {
            let text = "plain ascii\nline two\n";
            let (bytes, lossy) = encode(text, enc);
            assert!(!lossy, "{enc}");
            let back = decode_as(&bytes, enc);
            assert_eq!(back.text, text, "{enc}");
        }
        let (bytes, lossy) = encode("漢字", Encoding::from_label("windows-1252").unwrap());
        assert!(lossy);
        assert!(!bytes.is_empty());
    }

    /// A NUL in an ELF header is binary; plain text and empty input are not.
    #[test]
    fn binary_detection() {
        assert!(detect_binary(b"\x7fELF\0\0\0"));
        assert!(!detect_binary(b"plain text"));
        assert!(!detect_binary(b""));
    }
}
