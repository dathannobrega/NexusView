//! Byte → text sanitization (RNF-05).
//!
//! Forensic inputs are messy: mixed UTF-8 / Windows-1252, stray NUL bytes,
//! physical corruption. These helpers always produce valid UTF-8 and never
//! panic. The fast path (clean UTF-8) does not allocate.

use std::borrow::Cow;

/// Decode an arbitrary byte slice into a sanitized owned `String`.
///
/// - Valid UTF-8 (with no NUL bytes) is copied through directly.
/// - NUL bytes (`\0`) are stripped.
/// - Bytes that are not valid UTF-8 are decoded as Windows-1252, which maps
///   every byte to a defined code point — the most common legacy encoding in
///   Windows-sourced telemetry.
pub fn sanitize(bytes: &[u8]) -> String {
    let cleaned = strip_nuls(bytes);
    match std::str::from_utf8(&cleaned) {
        Ok(s) => s.to_owned(),
        Err(_) => {
            // WINDOWS_1252 decoding is total and lossless at the byte level; the
            // returned `had_errors` flag only signals undefined CP1252 slots,
            // which `encoding_rs` already replaced with U+FFFD.
            let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(&cleaned);
            decoded.into_owned()
        }
    }
}

/// Borrow the slice unchanged when there are no NUL bytes; otherwise return an
/// owned copy with NULs removed. NUL is checked first because it is rare, so the
/// common case stays allocation-free.
fn strip_nuls(bytes: &[u8]) -> Cow<'_, [u8]> {
    if memchr::memchr(0, bytes).is_some() {
        Cow::Owned(bytes.iter().copied().filter(|&b| b != 0).collect())
    } else {
        Cow::Borrowed(bytes)
    }
}

/// ASCII-case-insensitive substring search over raw bytes.
///
/// `needle` must be pre-lowercased (ASCII letters only); the haystack is folded
/// on the fly. This avoids any per-cell allocation in the search hot path while
/// matching the case-insensitive behavior analysts expect. Non-ASCII bytes are
/// compared verbatim.
pub fn ascii_icontains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last_start {
        if haystack[i].to_ascii_lowercase() == first
            && haystack[i + 1..i + needle.len()]
                .iter()
                .zip(&needle[1..])
                .all(|(h, n)| h.to_ascii_lowercase() == *n)
        {
            return true;
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_utf8_passthrough() {
        assert_eq!(sanitize(b"hello world"), "hello world");
    }

    #[test]
    fn strips_nul_bytes() {
        assert_eq!(sanitize(b"a\0b\0c"), "abc");
    }

    #[test]
    fn windows_1252_fallback() {
        // 0xE9 is 'é' in Windows-1252 but invalid as standalone UTF-8.
        assert_eq!(sanitize(b"caf\xE9"), "café");
    }

    #[test]
    fn icontains_folds_ascii() {
        assert!(ascii_icontains(b"ERROR: disk full", b"error"));
        assert!(ascii_icontains(b"Timeout", b"out"));
        assert!(!ascii_icontains(b"ok", b"error"));
        assert!(ascii_icontains(b"anything", b""));
    }
}
