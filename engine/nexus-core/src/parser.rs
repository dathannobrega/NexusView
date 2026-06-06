//! Field splitting within a single record (RF-01).
//!
//! The engine is line-offset indexed (RF-02), so one newline-terminated line is
//! one record. Splitting therefore operates on a single line's bytes and honors
//! the quote character so delimiters *inside* quotes are not treated as field
//! separators. Embedded newlines inside quoted fields are intentionally not
//! supported — that is the correct trade-off for the offset-mapping
//! architecture and matches how newline-delimited DFIR telemetry behaves.
//!
//! Splitting yields byte ranges (no allocation); callers materialize a `String`
//! only for the cells they actually need (visible viewport / export rows).

use smallvec::SmallVec;

/// Field byte ranges for a single record, inline-allocated for up to 32 columns.
pub type FieldRanges = SmallVec<[(usize, usize); 32]>;

/// Split `line` into field ranges, invoking `emit(start, end)` for each field.
///
/// Ranges are relative to `line`. Quoted regions (delimited by `quote`) suppress
/// delimiter handling; a doubled quote (`""`) inside a quoted field is an escaped
/// quote and does not end the field.
pub fn split_fields<F: FnMut(usize, usize)>(line: &[u8], delim: u8, quote: u8, mut emit: F) {
    let n = line.len();
    let mut i = 0;
    let mut field_start = 0;
    let mut in_quotes = false;

    while i < n {
        let b = line[i];
        if in_quotes {
            if b == quote {
                if i + 1 < n && line[i + 1] == quote {
                    i += 2; // escaped quote
                    continue;
                }
                in_quotes = false;
            }
            i += 1;
        } else if b == quote {
            in_quotes = true;
            i += 1;
        } else if b == delim {
            emit(field_start, i);
            i += 1;
            field_start = i;
        } else {
            i += 1;
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
pub fn field_at(line: &[u8], delim: u8, quote: u8, n: usize) -> Option<(usize, usize)> {
    let len = line.len();
    let mut i = 0;
    let mut field_start = 0;
    let mut index = 0;
    let mut in_quotes = false;

    while i < len {
        let b = line[i];
        if in_quotes {
            if b == quote {
                if i + 1 < len && line[i + 1] == quote {
                    i += 2;
                    continue;
                }
                in_quotes = false;
            }
            i += 1;
        } else if b == quote {
            in_quotes = true;
            i += 1;
        } else if b == delim {
            if index == n {
                return Some((field_start, i));
            }
            index += 1;
            i += 1;
            field_start = i;
        } else {
            i += 1;
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
}
