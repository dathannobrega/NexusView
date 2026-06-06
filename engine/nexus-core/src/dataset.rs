//! The central dataset type: ties the memory map, the line-offset index, the
//! parser schema, and search together behind a small, UI-agnostic API.
//!
//! A `Dataset` is immutable after [`Dataset::open`] and is `Send + Sync`, so it
//! can be shared across threads without locks. Searches are pure functions that
//! produce a new [`View`] rather than mutating shared state — that is what lets
//! the UI keep the main thread free (RNF-01) and swap results atomically.

use rayon::prelude::*;

use crate::error::{NexusError, Result};
use crate::index::LineIndex;
use crate::mmap::MappedFile;
use crate::parser::{self, field_ranges};
use crate::query::Query;
use crate::schema::{self, ParserSchema};
use crate::view::View;

/// Bytes sampled from the head of the file for delimiter auto-detection.
const SNIFF_BYTES: usize = 64 * 1024;

/// An opened, indexed dataset ready for querying.
pub struct Dataset {
    file: MappedFile,
    index: LineIndex,
    schema: ParserSchema,
    delim: u8,
    quote: u8,
    columns: Vec<String>,
    /// Number of leading file lines that are not data (skipped lines + header).
    first_data_line: usize,
    /// Trigram Bloom index, built lazily on the first substring search (RF-04).
    bloom: std::sync::OnceLock<crate::bloom::BlockBloom>,
    /// Row tags, allocated lazily on the first tag (Timeline Explorer-style).
    tags: std::sync::OnceLock<crate::tags::TagStore>,
}

impl Dataset {
    /// Open and index a file. When `schema` is `None` the delimiter is sniffed
    /// from the file head and a header row is assumed.
    pub fn open(path: impl AsRef<std::path::Path>, schema: Option<ParserSchema>) -> Result<Self> {
        let file = MappedFile::open(path)?;
        let index = LineIndex::build(&file);

        let schema = schema.unwrap_or_else(|| {
            let bytes = file.bytes();
            let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
            schema::sniff(head)
        });

        let delim = schema.delimiter_byte();
        let quote = schema.quote_byte();
        let first_data_line = schema.skip_lines + usize::from(schema.has_header);

        let columns = Self::determine_columns(&file, &index, &schema, delim, quote)?;

        Ok(Self {
            file,
            index,
            schema,
            delim,
            quote,
            columns,
            first_data_line,
            bloom: std::sync::OnceLock::new(),
            tags: std::sync::OnceLock::new(),
        })
    }

    /// Resolve column names: explicit schema columns win, else a header row, else
    /// generated `Column N` names sized to the first data line.
    fn determine_columns(
        file: &MappedFile,
        index: &LineIndex,
        schema: &ParserSchema,
        delim: u8,
        quote: u8,
    ) -> Result<Vec<String>> {
        if !schema.columns.is_empty() {
            return Ok(schema.columns.clone());
        }

        let trim = |span: (usize, usize)| -> &[u8] {
            let (s, e) = span;
            let mut slice = &file.bytes()[s..e];
            if slice.last() == Some(&b'\n') {
                slice = &slice[..slice.len() - 1];
            }
            if slice.last() == Some(&b'\r') {
                slice = &slice[..slice.len() - 1];
            }
            slice
        };

        if schema.has_header {
            let span = index
                .line_span(schema.skip_lines)
                .ok_or(NexusError::NoColumns)?;
            let line = trim(span);
            let mut cols: Vec<String> = field_ranges(line, delim, quote)
                .iter()
                .enumerate()
                .map(|(i, &r)| {
                    let name = parser::field_value(line, r, quote);
                    if name.trim().is_empty() {
                        format!("Column {}", i + 1)
                    } else {
                        name
                    }
                })
                .collect();
            if cols.is_empty() {
                cols.push("Column 1".to_string());
            }
            Ok(cols)
        } else {
            let span = index
                .line_span(schema.skip_lines)
                .ok_or(NexusError::NoColumns)?;
            let line = trim(span);
            let n = parser::count_fields(line, delim, quote).max(1);
            Ok((1..=n).map(|i| format!("Column {i}")).collect())
        }
    }

    // --- Metadata ---------------------------------------------------------

    /// Number of data rows (excludes skipped lines and the header).
    #[inline]
    pub fn row_count(&self) -> usize {
        self.index.line_count().saturating_sub(self.first_data_line)
    }

    /// Column names in order.
    #[inline]
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    #[inline]
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Name of column `col`, or `None` if out of range.
    #[inline]
    pub fn column_name(&self, col: usize) -> Option<&str> {
        self.columns.get(col).map(String::as_str)
    }

    /// The resolved parser schema (for inspection / export with the original
    /// delimiter).
    #[inline]
    pub fn schema(&self) -> &ParserSchema {
        &self.schema
    }

    #[inline]
    pub fn delimiter(&self) -> u8 {
        self.delim
    }

    // --- Row access -------------------------------------------------------

    /// Raw bytes of data row `data_row`, with line terminators trimmed.
    /// Returns `None` when out of range — never panics.
    #[inline]
    pub fn line_bytes(&self, data_row: usize) -> Option<&[u8]> {
        let file_line = self.first_data_line.checked_add(data_row)?;
        let (s, e) = self.index.line_span(file_line)?;
        let mut slice = &self.file.bytes()[s..e];
        if slice.last() == Some(&b'\n') {
            slice = &slice[..slice.len() - 1];
        }
        if slice.last() == Some(&b'\r') {
            slice = &slice[..slice.len() - 1];
        }
        Some(slice)
    }

    /// Display value of a single cell. Out-of-range row or column yields `""`.
    /// Extracts only the target field (stopping early), so rendering a viewport
    /// never splits the whole line per cell.
    pub fn cell(&self, data_row: usize, col: usize) -> String {
        let Some(line) = self.line_bytes(data_row) else {
            return String::new();
        };
        match parser::field_at(line, self.delim, self.quote, col) {
            Some(r) => parser::field_value(line, r, self.quote),
            None => String::new(),
        }
    }

    /// All cell values for a data row, padded/truncated to `column_count` so
    /// rows stay rectangular for export.
    pub fn row_values(&self, data_row: usize) -> Vec<String> {
        let ncols = self.column_count();
        let mut out = Vec::with_capacity(ncols);
        if let Some(line) = self.line_bytes(data_row) {
            for &r in field_ranges(line, self.delim, self.quote)
                .iter()
                .take(ncols)
            {
                out.push(parser::field_value(line, r, self.quote));
            }
        }
        out.resize(ncols, String::new());
        out
    }

    /// The full raw line for a data row, encoding-sanitized, with the original
    /// delimiters preserved (used by row-level copy, RF-09).
    pub fn raw_line(&self, data_row: usize) -> String {
        self.line_bytes(data_row)
            .map(crate::encoding::sanitize)
            .unwrap_or_default()
    }

    // --- Search -----------------------------------------------------------

    /// Run a search and return the matching data-row indices in ascending order.
    ///
    /// The scan is parallelized across all cores with `rayon` (RNF-03) and never
    /// touches the main thread. A single global substring is accelerated by the
    /// trigram Bloom index (RF-04); everything else is a full parallel scan. An
    /// empty/whitespace query matches all rows.
    pub fn search(&self, query: &str) -> Result<Vec<u32>> {
        let q = Query::compile(query, &self.columns)?;

        // Fast path: a single substring (global OR column-scoped, ≥3 bytes) → skip
        // whole blocks whose Bloom filter lacks a needle trigram (RF-04).
        if let Some((col, needle)) = q.single_substring() {
            if needle.len() >= 3 {
                let bloom = self.bloom();
                return Ok(crate::bloom::search_substring(self, bloom, col, needle));
            }
        }

        let n = self.row_count() as u32;
        let ids: Vec<u32> = (0..n)
            .into_par_iter()
            .filter(|&r| self.row_matches(&q, r as usize))
            .collect();
        Ok(ids)
    }

    /// The trigram Bloom index, built on first use (RF-04).
    fn bloom(&self) -> &crate::bloom::BlockBloom {
        self.bloom
            .get_or_init(|| crate::bloom::BlockBloom::build(self))
    }

    /// Match a single substring against a row — globally or within column `col`.
    /// Drives the Bloom-accelerated search path; allocation-free. Scoped matches
    /// extract only the target field (stopping early), so a column filter no
    /// longer pays to split the whole line.
    pub(crate) fn matches_substring(
        &self,
        data_row: usize,
        col: Option<usize>,
        needle: &[u8],
    ) -> bool {
        let Some(line) = self.line_bytes(data_row) else {
            return false;
        };
        match col {
            None => crate::encoding::ascii_icontains(line, needle),
            Some(c) => match parser::field_at(line, self.delim, self.quote, c) {
                Some((s, e)) => {
                    let field = parser::unquote_borrowed(&line[s..e], self.quote);
                    crate::encoding::ascii_icontains(field, needle)
                }
                None => false,
            },
        }
    }

    #[inline]
    fn row_matches(&self, query: &Query, data_row: usize) -> bool {
        let Some(line) = self.line_bytes(data_row) else {
            return false;
        };
        if query.has_scoped() {
            let fields = field_ranges(line, self.delim, self.quote);
            query.matches(line, Some(&fields), self.quote)
        } else {
            query.matches(line, None, self.quote)
        }
    }

    /// Run a search and return it as a ready-to-use [`View`].
    pub fn search_view(&self, query: &str) -> Result<View> {
        Ok(View::Filtered(self.search(query)?))
    }

    // --- Views ------------------------------------------------------------

    /// The identity view over every data row (no allocation).
    #[inline]
    pub fn view_all(&self) -> View {
        View::All(self.row_count() as u64)
    }

    /// Data-row index for a position within `view`.
    #[inline]
    pub fn view_row_id(&self, view: &View, view_row: u64) -> Option<u32> {
        view.row_id(view_row)
    }

    /// Cell value addressed through a view. Out-of-range yields `""`.
    pub fn view_cell(&self, view: &View, view_row: u64, col: usize) -> String {
        match view.row_id(view_row) {
            Some(r) => self.cell(r as usize, col),
            None => String::new(),
        }
    }

    /// Stable multi-column sort of `view`, returning a new sorted view (RF-05).
    pub fn sort(&self, view: &View, keys: &[crate::sort::SortKey]) -> View {
        crate::sort::sort(self, view, keys)
    }

    /// Build a multi-level grouping tree for `view` over `cols` (RF-03).
    pub fn group(&self, view: &View, cols: &[usize]) -> crate::group::GroupTree {
        crate::group::build(self, view, cols)
    }

    /// Export `view` to `path` in `format` (RF-10). `cols` selects the columns to
    /// include, in order; an empty slice exports all columns.
    pub fn export(
        &self,
        view: &View,
        format: crate::export::Format,
        path: &std::path::Path,
        cols: &[usize],
    ) -> Result<()> {
        crate::export::export(self, view, format, path, cols)
    }

    // --- Row tagging ------------------------------------------------------

    fn tag_store(&self) -> &crate::tags::TagStore {
        self.tags
            .get_or_init(|| crate::tags::TagStore::new(self.row_count()))
    }

    /// Tag or untag an absolute data row. Tags persist across filter/sort/group.
    pub fn set_tag(&self, data_row: usize, tagged: bool) {
        self.tag_store().set(data_row, tagged);
    }

    /// Is an absolute data row tagged? (Read-only — does not allocate the store.)
    pub fn is_tagged(&self, data_row: usize) -> bool {
        self.tags.get().is_some_and(|t| t.get(data_row))
    }

    /// Number of tagged rows.
    pub fn tagged_count(&self) -> usize {
        self.tags.get().map_or(0, |t| t.count())
    }

    /// Untag every row.
    pub fn clear_tags(&self) {
        if let Some(store) = self.tags.get() {
            store.clear();
        }
    }

    /// A view of all tagged rows (ascending).
    pub fn tagged_view(&self) -> View {
        let rows = self.tags.get().map(|t| t.tagged_rows()).unwrap_or_default();
        View::Filtered(rows)
    }

    /// The subset of `view` whose rows are tagged (preserves view order).
    pub fn intersect_tags(&self, view: &View) -> View {
        match self.tags.get() {
            Some(store) => View::Filtered(view.iter().filter(|&r| store.get(r as usize)).collect()),
            None => View::Filtered(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn dataset_from(
        bytes: &[u8],
        schema: Option<ParserSchema>,
    ) -> (Dataset, tempfile::NamedTempFile) {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        tf.write_all(bytes).unwrap();
        tf.flush().unwrap();
        let ds = Dataset::open(tf.path(), schema).unwrap();
        (ds, tf)
    }

    #[test]
    fn header_and_rows() {
        let (ds, _t) = dataset_from(b"host,event\nweb01,login\nweb02,logout\n", None);
        assert_eq!(ds.column_count(), 2);
        assert_eq!(ds.column_name(0), Some("host"));
        assert_eq!(ds.row_count(), 2);
        assert_eq!(ds.cell(0, 0), "web01");
        assert_eq!(ds.cell(1, 1), "logout");
    }

    #[test]
    fn no_header_generates_columns() {
        let schema = ParserSchema {
            has_header: false,
            ..ParserSchema::csv()
        };
        let (ds, _t) = dataset_from(b"a,b,c\nd,e,f\n", Some(schema));
        assert_eq!(ds.column_count(), 3);
        assert_eq!(ds.column_name(0), Some("Column 1"));
        assert_eq!(ds.row_count(), 2);
    }

    #[test]
    fn search_global_and_scoped() {
        let data = b"host,event\nweb01,login ok\nweb02,login fail\ndb01,logout\n";
        let (ds, _t) = dataset_from(data, None);
        assert_eq!(ds.search("login").unwrap(), vec![0, 1]);
        assert_eq!(ds.search("host:web02").unwrap(), vec![1]);
        assert_eq!(ds.search("login AND NOT fail").unwrap(), vec![0]);
        assert_eq!(ds.search("").unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn view_cell_through_filter() {
        let (ds, _t) = dataset_from(b"host,event\nweb01,a\nweb02,b\n", None);
        let view = ds.search_view("web02").unwrap();
        assert_eq!(view.len(), 1);
        assert_eq!(ds.view_cell(&view, 0, 0), "web02");
    }

    #[test]
    fn tags_persist_across_filtering() {
        let (ds, _t) = dataset_from(b"host,event\nweb01,a\nweb02,b\nweb01,c\n", None);
        ds.set_tag(0, true);
        ds.set_tag(2, true);
        assert_eq!(ds.tagged_count(), 2);

        // Filtering does not affect tags (keyed by absolute row).
        let filtered = ds.search_view("web01").unwrap(); // rows 0 and 2
        assert!(ds.is_tagged(0));
        assert!(ds.is_tagged(2));
        assert!(!ds.is_tagged(1));

        // tagged_view and intersect_tags.
        assert_eq!(ds.tagged_view().len(), 2);
        assert_eq!(ds.intersect_tags(&filtered).len(), 2);

        ds.set_tag(0, false);
        assert_eq!(ds.tagged_count(), 1);
    }

    #[test]
    fn malformed_lines_do_not_panic() {
        // NUL bytes, a CP1252 byte, ragged columns — must be handled gracefully.
        let data = b"host,event\nweb01,ok\x00fine\nweb\xE9,short\n,\n";
        let (ds, _t) = dataset_from(data, None);
        assert_eq!(ds.row_count(), 3);
        assert_eq!(ds.cell(0, 1), "okfine"); // NUL stripped
        assert!(!ds.cell(1, 0).is_empty()); // CP1252 decoded
        assert_eq!(ds.cell(2, 0), ""); // empty field
    }
}
