//! Zero-load record indexing (RF-02).
//!
//! Instead of parsing the file, we scan once and record the byte offset where
//! each record begins. Random access to any record is then O(1): record `i`
//! spans `starts[i] .. starts[i + 1]`. The scan uses `memchr`/`memchr2`, which
//! are SIMD-accelerated on AArch64 (NEON), so even multi-GB files index in
//! seconds.
//!
//! Two scanners share this representation:
//! - [`LineIndex::build`]: newline = record boundary, unconditionally.
//! - [`LineIndex::build_quoted`]: RFC 4180-aware — a newline inside a quoted
//!   field is field data, so one record may span several physical lines. Its
//!   quoting grammar is exactly the one [`crate::parser::split_fields`] uses
//!   (quotes only open a quoted field at the start of a field; `""` is an
//!   escaped quote), so the indexer and the field splitter always agree on
//!   what is inside quotes.
//!
//! Memory note: the offset vector is `8 bytes × record_count`. For the very
//! largest targets a blocked/delta-compressed variant keeps the strict
//! RNF-02 budget; it slots in behind this same API. The current dense form is
//! chosen for O(1) random access, which sort and grouping rely on heavily.

use crate::mmap::MappedFile;
use memchr::{memchr2_iter, memchr_iter};

/// Upper bound, in bytes, of a quoted region that may absorb newlines in
/// [`LineIndex::build_quoted`]. Malformed or truncated forensic input (a stray
/// unterminated quote at the start of a field) must not fuse the remainder of a
/// multi-GB file into a single record: past this span the opening quote is
/// demoted to literal data and the absorbed newlines become record boundaries
/// again. 1 MiB comfortably exceeds any real quoted field (script blobs,
/// certificate dumps, PID lists) seen in DFIR telemetry.
const MAX_QUOTED_SPAN_BYTES: usize = 1 << 20;

/// The UTF-8 byte-order mark, tolerated at the very start of the file.
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Byte offsets of every record start, terminated by a sentinel equal to the
/// file length so the last record has a well-defined end.
pub struct LineIndex {
    /// `starts.len() == record_count + 1`. The final element is the file length.
    starts: Vec<u64>,
}

impl LineIndex {
    /// Build the index by scanning `file` once for `\n`. Every newline is a
    /// record boundary; quoting is ignored.
    // Production opens go through `build_quoted`; this baseline scanner is kept
    // as the cross-check oracle for the quote-aware one (see tests) and as the
    // entry point for a future quoting-off dialect.
    #[allow(dead_code)]
    pub fn build(file: &MappedFile) -> Self {
        file.advise_sequential();
        let data = file.bytes();

        let mut starts = Vec::with_capacity(estimate_lines(data) + 1);
        starts.push(0);
        for nl in memchr_iter(b'\n', data) {
            starts.push((nl + 1) as u64);
        }

        Self::finish(starts, data.len() as u64)
    }

    /// Build the index with RFC 4180 quote handling: a newline inside a quoted
    /// field is part of the field, so the surrounding record spans multiple
    /// physical lines. See the module docs for the grammar shared with
    /// [`crate::parser::split_fields`] and the runaway-quote guard.
    pub fn build_quoted(file: &MappedFile, delim: u8, quote: u8) -> Self {
        file.advise_sequential();
        let data = file.bytes();

        let mut starts = Vec::with_capacity(estimate_lines(data) + 1);
        starts.push(0u64);

        // A leading UTF-8 BOM must not stop a quote from opening the very
        // first field of the file.
        let first_field_pos = if data.starts_with(UTF8_BOM) {
            UTF8_BOM.len()
        } else {
            0
        };

        let mut in_quotes = false;
        // Byte offset of the quote that opened the current quoted region.
        let mut open_pos = 0usize;
        // Newline successors provisionally absorbed by the open quoted region;
        // discarded when the region closes, promoted to boundaries when the
        // runaway guard demotes the opening quote.
        let mut pending: Vec<u64> = Vec::new();
        // Skips the second byte of an escaped quote pair (`""`).
        let mut skip_until = 0usize;

        for pos in memchr2_iter(quote, b'\n', data) {
            if pos < skip_until {
                continue;
            }
            if data[pos] == b'\n' {
                if !in_quotes {
                    starts.push((pos + 1) as u64);
                } else if pos - open_pos > MAX_QUOTED_SPAN_BYTES {
                    // Runaway quote: demote it to a literal (see const docs).
                    starts.append(&mut pending);
                    starts.push((pos + 1) as u64);
                    in_quotes = false;
                } else {
                    pending.push((pos + 1) as u64);
                }
            } else if in_quotes {
                if data.get(pos + 1) == Some(&quote) {
                    skip_until = pos + 2; // escaped quote, still inside the field
                } else {
                    in_quotes = false; // closing quote: absorbed newlines are data
                    pending.clear();
                }
            } else {
                // A quote only opens a quoted field at the start of a field:
                // start of the data, right after a delimiter, or right after a
                // record boundary. Anywhere else it is literal data — exactly
                // like `parser::split_fields`.
                let at_field_start = pos == first_field_pos
                    || (pos > 0 && (data[pos - 1] == delim || data[pos - 1] == b'\n'));
                if at_field_start {
                    in_quotes = true;
                    open_pos = pos;
                }
            }
        }

        // EOF with an unterminated quote: within the guard span the trailing
        // region is one (truncated) record; past it, demote as above.
        if in_quotes && data.len() - open_pos > MAX_QUOTED_SPAN_BYTES {
            starts.append(&mut pending);
        }

        Self::finish(starts, data.len() as u64)
    }

    /// Append the end-of-file sentinel and finalize the offset vector.
    fn finish(mut starts: Vec<u64>, file_len: u64) -> Self {
        match starts.last().copied() {
            // File ended exactly on a record boundary: the last pushed offset
            // already equals the file length and serves as the sentinel.
            Some(last) if last == file_len => {}
            // File ended mid-record: the bytes after the last boundary are a
            // real (unterminated) final record; add the sentinel.
            _ => starts.push(file_len),
        }

        starts.shrink_to_fit();
        LineIndex { starts }
    }

    /// Number of records in the file.
    #[inline]
    pub fn line_count(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    /// Raw byte span `[start, end)` of record `i`, **including** its trailing
    /// newline (and `\r`, for CRLF). Callers trim record terminators. Returns
    /// `None` when `i` is out of range — never panics.
    #[inline]
    pub fn line_span(&self, i: usize) -> Option<(usize, usize)> {
        let start = *self.starts.get(i)? as usize;
        let end = *self.starts.get(i + 1)? as usize;
        Some((start, end))
    }
}

/// Estimate the line count from a 64 KiB sample to size the offset vector and
/// avoid repeated reallocations during the scan.
fn estimate_lines(data: &[u8]) -> usize {
    const SAMPLE: usize = 64 * 1024;
    let sample = &data[..data.len().min(SAMPLE)];
    let newlines = memchr_iter(b'\n', sample).count();
    if newlines == 0 {
        return 16;
    }
    let avg_line = (sample.len() / newlines).max(1);
    (data.len() / avg_line).max(16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn index_of(bytes: &[u8]) -> (LineIndex, MappedFile) {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        tf.write_all(bytes).unwrap();
        tf.flush().unwrap();
        let mf = MappedFile::open(tf.path()).unwrap();
        let idx = LineIndex::build(&mf);
        (idx, mf)
    }

    fn line_str(idx: &LineIndex, mf: &MappedFile, i: usize) -> String {
        let (s, e) = idx.line_span(i).unwrap();
        let mut slice = &mf.bytes()[s..e];
        if slice.last() == Some(&b'\n') {
            slice = &slice[..slice.len() - 1];
        }
        if slice.last() == Some(&b'\r') {
            slice = &slice[..slice.len() - 1];
        }
        String::from_utf8_lossy(slice).into_owned()
    }

    #[test]
    fn trailing_newline() {
        let (idx, mf) = index_of(b"a\nb\n");
        assert_eq!(idx.line_count(), 2);
        assert_eq!(line_str(&idx, &mf, 0), "a");
        assert_eq!(line_str(&idx, &mf, 1), "b");
        assert!(idx.line_span(2).is_none());
    }

    #[test]
    fn no_trailing_newline() {
        let (idx, mf) = index_of(b"a\nb");
        assert_eq!(idx.line_count(), 2);
        assert_eq!(line_str(&idx, &mf, 1), "b");
    }

    #[test]
    fn crlf_endings() {
        let (idx, mf) = index_of(b"a\r\nb\r\n");
        assert_eq!(idx.line_count(), 2);
        assert_eq!(line_str(&idx, &mf, 0), "a");
        assert_eq!(line_str(&idx, &mf, 1), "b");
    }

    #[test]
    fn single_line() {
        let (idx, mf) = index_of(b"only line");
        assert_eq!(idx.line_count(), 1);
        assert_eq!(line_str(&idx, &mf, 0), "only line");
    }

    // --- build_quoted: RFC 4180 record boundaries --------------------------

    fn quoted_index_of(bytes: &[u8]) -> (LineIndex, MappedFile) {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        tf.write_all(bytes).unwrap();
        tf.flush().unwrap();
        let mf = MappedFile::open(tf.path()).unwrap();
        let idx = LineIndex::build_quoted(&mf, b',', b'"');
        (idx, mf)
    }

    #[test]
    fn quoted_newline_is_field_data() {
        // One record whose quoted field spans two physical lines, then a
        // normal record.
        let (idx, mf) = quoted_index_of(b"a,\"x\ny\",b\nc,d,e\n");
        assert_eq!(idx.line_count(), 2);
        assert_eq!(line_str(&idx, &mf, 0), "a,\"x\ny\",b");
        assert_eq!(line_str(&idx, &mf, 1), "c,d,e");
    }

    #[test]
    fn quoted_crlf_is_field_data() {
        let (idx, mf) = quoted_index_of(b"a,\"x\r\ny\",b\r\nc,d,e\r\n");
        assert_eq!(idx.line_count(), 2);
        assert_eq!(line_str(&idx, &mf, 0), "a,\"x\r\ny\",b");
    }

    #[test]
    fn escaped_quotes_stay_inside_the_field() {
        // `""` inside the quoted field must not close it, so the newline after
        // it is still field data.
        let (idx, mf) = quoted_index_of(b"a,\"he said \"\"hi\"\"\nbye\"\nz,w\n");
        assert_eq!(idx.line_count(), 2);
        assert_eq!(line_str(&idx, &mf, 0), "a,\"he said \"\"hi\"\"\nbye\"");
        assert_eq!(line_str(&idx, &mf, 1), "z,w");
    }

    #[test]
    fn stray_mid_field_quote_is_literal() {
        // The quote in `b"c` is not at a field start: it must not absorb the
        // newline (a stray quote in a command line must stay harmless).
        let (idx, mf) = quoted_index_of(b"a,b\"c\nd,e\n");
        assert_eq!(idx.line_count(), 2);
        assert_eq!(line_str(&idx, &mf, 0), "a,b\"c");
        assert_eq!(line_str(&idx, &mf, 1), "d,e");
    }

    #[test]
    fn doubled_quote_reads_as_escape_not_close() {
        // In `"a""b…` the `""` is an escaped quote (RFC 4180), so the field —
        // and therefore the record — stays open across both newlines and the
        // whole input is a single unterminated record.
        let (idx, mf) = quoted_index_of(b"\"a\"\"b,c\nd,e\n");
        assert_eq!(idx.line_count(), 1);
        assert_eq!(line_str(&idx, &mf, 0), "\"a\"\"b,c\nd,e");
    }

    #[test]
    fn bom_does_not_hide_a_quoted_first_field() {
        let (idx, mf) = quoted_index_of(b"\xEF\xBB\xBF\"x\ny\",a\nz,w\n");
        assert_eq!(idx.line_count(), 2);
        assert_eq!(line_str(&idx, &mf, 0), "\u{FEFF}\"x\ny\",a");
        assert_eq!(line_str(&idx, &mf, 1), "z,w");
    }

    #[test]
    fn unterminated_quote_under_cap_runs_to_eof() {
        // Truncated capture: the open quote legitimately absorbs the newline
        // and the final record runs to EOF.
        let (idx, mf) = quoted_index_of(b"a,\"x\ny");
        assert_eq!(idx.line_count(), 1);
        assert_eq!(line_str(&idx, &mf, 0), "a,\"x\ny");
    }

    #[test]
    fn runaway_quote_is_demoted_to_literal() {
        // An unterminated quote spanning more than MAX_QUOTED_SPAN_BYTES must
        // not fuse the file: the provisionally absorbed newlines become record
        // boundaries again.
        let mut data = Vec::new();
        data.extend_from_slice(b"a,\"p\nq\n");
        data.extend(std::iter::repeat_n(b'z', MAX_QUOTED_SPAN_BYTES + 16));
        data.extend_from_slice(b"\nrest,row\n");
        let (idx, mf) = quoted_index_of(&data);
        assert_eq!(idx.line_count(), 4);
        assert_eq!(line_str(&idx, &mf, 0), "a,\"p");
        assert_eq!(line_str(&idx, &mf, 1), "q");
        assert_eq!(line_str(&idx, &mf, 3), "rest,row");
    }

    #[test]
    fn no_quotes_matches_plain_build() {
        let data: &[u8] = b"a,b\nc,d\ne,f";
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        tf.write_all(data).unwrap();
        tf.flush().unwrap();
        let mf = MappedFile::open(tf.path()).unwrap();
        let plain = LineIndex::build(&mf);
        let quoted = LineIndex::build_quoted(&mf, b',', b'"');
        assert_eq!(plain.starts, quoted.starts);
    }

    /// Reference scanner: same grammar as `build_quoted`, written as a plain
    /// per-byte state machine (no `memchr2` jumps, no `skip_until`). Used to
    /// cross-check the optimized implementation on generated inputs.
    fn reference_record_starts(data: &[u8], delim: u8, quote: u8) -> Vec<u64> {
        let first_field_pos = if data.starts_with(UTF8_BOM) {
            UTF8_BOM.len()
        } else {
            0
        };
        let mut starts = vec![0u64];
        let mut in_quotes = false;
        let mut open_pos = 0usize;
        let mut pending: Vec<u64> = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let b = data[i];
            if in_quotes {
                if b == quote {
                    if data.get(i + 1) == Some(&quote) {
                        i += 2;
                        continue;
                    }
                    in_quotes = false;
                    pending.clear();
                } else if b == b'\n' {
                    if i - open_pos > MAX_QUOTED_SPAN_BYTES {
                        starts.append(&mut pending);
                        starts.push((i + 1) as u64);
                        in_quotes = false;
                    } else {
                        pending.push((i + 1) as u64);
                    }
                }
            } else if b == b'\n' {
                starts.push((i + 1) as u64);
            } else if b == quote {
                let at_field_start = i == first_field_pos
                    || (i > 0 && (data[i - 1] == delim || data[i - 1] == b'\n'));
                if at_field_start {
                    in_quotes = true;
                    open_pos = i;
                }
            }
            i += 1;
        }
        if in_quotes && data.len() - open_pos > MAX_QUOTED_SPAN_BYTES {
            starts.append(&mut pending);
        }
        let file_len = data.len() as u64;
        if starts.last().copied() != Some(file_len) {
            starts.push(file_len);
        }
        starts
    }

    #[test]
    fn build_quoted_agrees_with_reference_on_generated_soup() {
        // Deterministic LCG so the test is reproducible without new deps.
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        // Alphabet biased toward the interesting bytes.
        const ALPHABET: &[u8] = b"\"\"\"\n\n,,xy\r";
        for case in 0..200 {
            // MappedFile rejects empty files, so generate at least one byte.
            let len = 1 + next() % 63;
            let data: Vec<u8> = (0..len)
                .map(|_| ALPHABET[next() % ALPHABET.len()])
                .collect();
            let mut tf = tempfile::NamedTempFile::new().unwrap();
            tf.write_all(&data).unwrap();
            tf.flush().unwrap();
            let mf = MappedFile::open(tf.path()).unwrap();
            let idx = LineIndex::build_quoted(&mf, b',', b'"');
            let expected = reference_record_starts(&data, b',', b'"');
            assert_eq!(
                idx.starts,
                expected,
                "case {case}: input {:?}",
                String::from_utf8_lossy(&data)
            );
        }
    }
}
