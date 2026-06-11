//! Field splitting within a single record (RF-01).
//!
//! The engine is record-offset indexed (RF-02): [`crate::index::LineIndex`]
//! hands out one *record* at a time, where a record is normally one physical
//! line but may span several when a quoted field contains newlines (RFC 4180).
//! Splitting operates on a single record's bytes with the same quoting grammar
//! the indexer uses — the two must agree or a record correctly joined by the
//! index would be mis-split here:
//!
//! - A quote is only special at the **start of a field** (record start or right
//!   after a delimiter). A field that starts with `quote` is a quoted field.
//! - Inside a quoted field, a doubled quote (`""`) is an escaped literal quote;
//!   delimiters and newlines are field data.
//! - A quote anywhere else (mid-field, or after the closing quote) is literal
//!   data. This lenient reading keeps stray quotes in unquoted DFIR fields
//!   (command lines, paths) from corrupting the rest of the record.
//! - An unterminated quoted field runs to the end of the record.
//!
//! Splitting yields byte ranges (no allocation); callers materialize a `String`
//! only for the cells they actually need (visible viewport / export rows).

use smallvec::SmallVec;

/// Field byte ranges for a single record, inline-allocated for up to 32 columns.
pub type FieldRanges = SmallVec<[(usize, usize); 32]>;

/// Split `line` into field ranges, invoking `emit(start, end)` for each field.
///
/// Ranges are relative to `line` and include any surrounding quotes (use
/// [`field_value`] / [`unquote_borrowed`] to strip them). See the module docs
/// for the exact quoting grammar.
pub fn split_fields<F: FnMut(usize, usize)>(line: &[u8], delim: u8, quote: u8, mut emit: F) {
    let n = line.len();
    let mut i = 0;
    let mut field_start = 0;

    while i < n {
        // Loop invariant: `i` is at the first byte of a field here.
        if line[i] == quote {
            // Quoted field: scan to its closing quote; `""` is an escaped
            // literal quote. Newlines inside are field data.
            i += 1;
            while i < n {
                if line[i] == quote {
                    if line.get(i + 1) == Some(&quote) {
                        i += 2; // escaped quote
                        continue;
                    }
                    i += 1; // step past the closing quote
                    break;
                }
                i += 1;
            }
        }
        // Unquoted field body — or the (malformed) tail after a closing quote.
        // Quotes seen here are literal data.
        while i < n && line[i] != delim {
            i += 1;
        }
        if i < n {
            emit(field_start, i);
            i += 1;
            field_start = i;
        }
    }
    emit(field_start, n);
}

/// Collect every field range of `line` into a [`FieldRanges`].
pub fn field_ranges(line: &[u8], delim: u8, quote: u8) -> FieldRanges {
    let mut out = FieldRanges::new();
    split_fields(line, delim, quote, |s, e| out.push((s, e)));
    out
}

/// Number of fields in `line`.
pub fn count_fields(line: &[u8], delim: u8, quote: u8) -> usize {
    let mut count = 0;
    split_fields(line, delim, quote, |_, _| count += 1);
    count
}

/// Byte range of the `n`-th field (0-based), stopping as soon as it is found —
/// avoids splitting the whole line when only one column is needed (scoped search).
/// Same quoting grammar as [`split_fields`].
pub fn field_at(line: &[u8], delim: u8, quote: u8, n: usize) -> Option<(usize, usize)> {
    let len = line.len();
    let mut i = 0;
    let mut field_start = 0;
    let mut index = 0;

    while i < len {
        // Loop invariant: `i` is at the first byte of a field here.
        if line[i] == quote {
            i += 1;
            while i < len {
                if line[i] == quote {
                    if line.get(i + 1) == Some(&quote) {
                        i += 2; // escaped quote
                        continue;
                    }
                    i += 1; // step past the closing quote
                    break;
                }
                i += 1;
            }
        }
        while i < len && line[i] != delim {
            i += 1;
        }
        if i < len {
            if index == n {
                return Some((field_start, i));
            }
            index += 1;
            i += 1;
            field_start = i;
        }
    }
    if index == n {
        Some((field_start, len))
    } else {
        None
    }
}

/// Strip one layer of surrounding quotes from a field slice, without allocating.
/// Used on the search hot path where escaped-quote unescaping is not required.
#[inline]
pub fn unquote_borrowed(field: &[u8], quote: u8) -> &[u8] {
    if field.len() >= 2 && field[0] == quote && field[field.len() - 1] == quote {
        &field[1..field.len() - 1]
    } else {
        field
    }
}

/// Materialize a field slice into a clean, display-ready `String`.
///
/// Removes a trailing `\r` (CRLF), strips surrounding quotes, unescapes doubled
/// quotes, and sanitizes encoding (RNF-05).
pub fn field_value(line: &[u8], (start, end): (usize, usize), quote: u8) -> String {
    let mut raw = &line[start..end];
    if raw.last() == Some(&b'\r') {
        raw = &raw[..raw.len() - 1];
    }

    if raw.len() >= 2 && raw[0] == quote && raw[raw.len() - 1] == quote {
        let inner = &raw[1..raw.len() - 1];
        if memchr::memchr(quote, inner).is_some() {
            // Unescape doubled quotes into a temporary buffer.
            let mut buf = Vec::with_capacity(inner.len());
            let mut j = 0;
            while j < inner.len() {
                if inner[j] == quote && inner.get(j + 1) == Some(&quote) {
                    buf.push(quote);
                    j += 2;
                } else {
                    buf.push(inner[j]);
                    j += 1;
                }
            }
            return crate::encoding::sanitize(&buf);
        }
        return crate::encoding::sanitize(inner);
    }
    crate::encoding::sanitize(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(line: &[u8], delim: u8) -> Vec<String> {
        field_ranges(line, delim, b'"')
            .iter()
            .map(|&r| field_value(line, r, b'"'))
            .collect()
    }

    #[test]
    fn plain_csv() {
        assert_eq!(fields(b"a,b,c", b','), vec!["a", "b", "c"]);
    }

    #[test]
    fn empty_fields_preserved() {
        assert_eq!(fields(b"a,,c", b','), vec!["a", "", "c"]);
        assert_eq!(fields(b",,", b','), vec!["", "", ""]);
    }

    #[test]
    fn quoted_delimiter() {
        assert_eq!(
            fields(b"\"a,b\",c", b','),
            vec!["a,b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn escaped_quotes() {
        assert_eq!(
            fields(b"\"she said \"\"hi\"\"\",x", b','),
            vec!["she said \"hi\"".to_string(), "x".to_string()]
        );
    }

    #[test]
    fn trailing_cr_trimmed() {
        assert_eq!(fields(b"a,b\r", b','), vec!["a", "b"]);
    }

    #[test]
    fn pipe_bodyfile() {
        let line = b"0|/etc/passwd|12|r--|0|0|1024|100|200|300|400";
        assert_eq!(count_fields(line, b'|', b'"'), 11);
    }

    #[test]
    fn multiline_quoted_field_value() {
        // The record-aware index (RF-02) hands us a record whose quoted field
        // spans physical lines; the newline is field data.
        assert_eq!(
            fields(b"a,\"x\ny\",b", b','),
            vec!["a".to_string(), "x\ny".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn crlf_inside_quoted_field_is_preserved() {
        assert_eq!(
            fields(b"a,\"x\r\ny\"", b','),
            vec!["a".to_string(), "x\r\ny".to_string()]
        );
    }

    #[test]
    fn mid_field_quote_is_literal() {
        // RFC 4180 lenient: a quote that does not start the field is data and
        // must not swallow the following delimiter.
        assert_eq!(
            fields(b"x,ab\"cd,e", b','),
            vec!["x".to_string(), "ab\"cd".to_string(), "e".to_string()]
        );
    }

    #[test]
    fn junk_after_closing_quote_is_literal() {
        assert_eq!(
            fields(b"\"a\"b,c", b','),
            vec!["\"a\"b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn unterminated_quote_runs_to_record_end() {
        assert_eq!(
            fields(b"a,\"bc", b','),
            vec!["a".to_string(), "\"bc".to_string()]
        );
    }

    #[test]
    fn field_at_agrees_with_field_ranges() {
        let lines: &[&[u8]] = &[
            b"a,b,c",
            b"a,\"x\ny\",b",
            b"x,ab\"cd,e",
            b"\"a\"b,c",
            b"a,\"he said \"\"hi\"\"\",x",
            b",,",
            b"\"unterminated",
            b"",
        ];
        for line in lines {
            let ranges = field_ranges(line, b',', b'"');
            for (i, &r) in ranges.iter().enumerate() {
                assert_eq!(
                    field_at(line, b',', b'"', i),
                    Some(r),
                    "line {:?} field {i}",
                    String::from_utf8_lossy(line)
                );
            }
            assert_eq!(field_at(line, b',', b'"', ranges.len()), None);
        }
    }
}
