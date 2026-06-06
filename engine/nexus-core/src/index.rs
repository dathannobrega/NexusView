//! Zero-load line indexing (RF-02).
//!
//! Instead of parsing the file, we scan once for newline bytes and record the
//! byte offset where each line begins. Random access to any line is then O(1):
//! line `i` spans `starts[i] .. starts[i + 1]`. The scan uses `memchr`, which is
//! SIMD-accelerated on AArch64 (NEON), so even multi-GB files index in seconds.
//!
//! Memory note: the offset vector is `8 bytes × line_count`. For the very
//! largest targets a blocked/delta-compressed variant keeps the strict
//! RNF-02 budget; it slots in behind this same API. The current dense form is
//! chosen for O(1) random access, which sort and grouping rely on heavily.

use crate::mmap::MappedFile;
use memchr::memchr_iter;

/// Byte offsets of every line start, terminated by a sentinel equal to the file
/// length so the last line has a well-defined end.
pub struct LineIndex {
    /// `starts.len() == line_count + 1`. The final element is the file length.
    starts: Vec<u64>,
}

impl LineIndex {
    /// Build the index by scanning `file` once for `\n`.
    pub fn build(file: &MappedFile) -> Self {
        file.advise_sequential();
        let data = file.bytes();

        let mut starts = Vec::with_capacity(estimate_lines(data) + 1);
        starts.push(0);
        for nl in memchr_iter(b'\n', data) {
            starts.push((nl + 1) as u64);
        }

        let file_len = data.len() as u64;
        match starts.last().copied() {
            // File ended exactly on a newline: the last pushed offset already
            // equals the file length and serves as the sentinel.
            Some(last) if last == file_len => {}
            // File ended without a trailing newline: the bytes after the last
            // newline are a real (unterminated) final line; add the sentinel.
            _ => starts.push(file_len),
        }

        starts.shrink_to_fit();
        LineIndex { starts }
    }

    /// Number of lines in the file.
    #[inline]
    pub fn line_count(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    /// Raw byte span `[start, end)` of line `i`, **including** its trailing
    /// newline (and `\r`, for CRLF). Callers trim line terminators. Returns
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
}
