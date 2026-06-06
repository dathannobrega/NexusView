//! Export the current view to disk (RF-10).
//!
//! Output is the "current view": exactly the rows in `view` (filtered + sorted)
//! across all columns. The engine writes the file itself, streaming row by row,
//! so no bulk data crosses the FFI boundary and exporting millions of rows uses
//! bounded memory.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::dataset::Dataset;
use crate::error::Result;
use crate::view::View;

/// Output formats for export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Tsv,
    /// Newline-delimited array of `{column: value}` objects.
    Json,
    /// A standalone `<table>` for pasting into incident reports.
    Html,
}

impl Format {
    /// Map the FFI integer code to a format.
    pub fn from_code(code: u32) -> Option<Format> {
        match code {
            0 => Some(Format::Csv),
            1 => Some(Format::Tsv),
            2 => Some(Format::Json),
            3 => Some(Format::Html),
            _ => None,
        }
    }
}

/// Export `view` of `dataset` to `path` in `format`. `cols` selects the columns
/// to include, in order; an empty slice exports all columns. Hidden columns are
/// excluded simply by not listing them here.
pub fn export(
    dataset: &Dataset,
    view: &View,
    format: Format,
    path: &Path,
    cols: &[usize],
) -> Result<()> {
    let selected: Vec<usize> = if cols.is_empty() {
        (0..dataset.column_count()).collect()
    } else {
        cols.iter()
            .copied()
            .filter(|&c| c < dataset.column_count())
            .collect()
    };

    let mut writer = BufWriter::new(File::create(path)?);
    match format {
        Format::Csv => write_delimited(dataset, view, b',', &selected, &mut writer)?,
        Format::Tsv => write_delimited(dataset, view, b'\t', &selected, &mut writer)?,
        Format::Json => write_json(dataset, view, &selected, &mut writer)?,
        Format::Html => write_html(dataset, view, &selected, &mut writer)?,
    }
    writer.flush()?;
    Ok(())
}

fn write_delimited<W: Write>(
    dataset: &Dataset,
    view: &View,
    delim: u8,
    selected: &[usize],
    writer: &mut W,
) -> Result<()> {
    let columns = dataset.columns();
    write_delimited_row(writer, selected.iter().map(|&c| columns[c].as_str()), delim)?;
    for data_row in view.iter() {
        let values = dataset.row_values(data_row as usize);
        write_delimited_row(
            writer,
            selected
                .iter()
                .map(|&c| values.get(c).map(String::as_str).unwrap_or("")),
            delim,
        )?;
    }
    Ok(())
}

fn write_delimited_row<'a, W: Write, I: Iterator<Item = &'a str>>(
    writer: &mut W,
    fields: I,
    delim: u8,
) -> Result<()> {
    for (i, field) in fields.enumerate() {
        if i > 0 {
            writer.write_all(&[delim])?;
        }
        write_delimited_field(writer, field, delim)?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

/// Quote a field per RFC 4180 when it contains the delimiter, a quote, or a
/// newline; an embedded quote is escaped by doubling it.
fn write_delimited_field<W: Write>(writer: &mut W, field: &str, delim: u8) -> Result<()> {
    let needs_quoting = field
        .bytes()
        .any(|b| b == delim || b == b'"' || b == b'\n' || b == b'\r');
    if !needs_quoting {
        writer.write_all(field.as_bytes())?;
        return Ok(());
    }
    writer.write_all(b"\"")?;
    let bytes = field.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' {
            writer.write_all(&bytes[start..=i])?; // include the quote...
            writer.write_all(b"\"")?; // ...then double it
            start = i + 1;
        }
    }
    writer.write_all(&bytes[start..])?;
    writer.write_all(b"\"")?;
    Ok(())
}

fn write_json<W: Write>(
    dataset: &Dataset,
    view: &View,
    selected: &[usize],
    writer: &mut W,
) -> Result<()> {
    let columns = dataset.columns();
    writer.write_all(b"[\n")?;
    let mut first = true;
    for data_row in view.iter() {
        let values = dataset.row_values(data_row as usize);
        if !first {
            writer.write_all(b",\n")?;
        }
        first = false;

        writer.write_all(b"  {")?;
        for (i, &col) in selected.iter().enumerate() {
            if i > 0 {
                writer.write_all(b", ")?;
            }
            let value = values.get(col).map(String::as_str).unwrap_or("");
            write_json_string(writer, &columns[col])?;
            writer.write_all(b": ")?;
            write_json_string(writer, value)?;
        }
        writer.write_all(b"}")?;
    }
    writer.write_all(b"\n]\n")?;
    Ok(())
}

/// Write a JSON string literal with the standard escapes.
fn write_json_string<W: Write>(writer: &mut W, s: &str) -> Result<()> {
    writer.write_all(b"\"")?;
    for c in s.chars() {
        match c {
            '"' => writer.write_all(b"\\\"")?,
            '\\' => writer.write_all(b"\\\\")?,
            '\n' => writer.write_all(b"\\n")?,
            '\r' => writer.write_all(b"\\r")?,
            '\t' => writer.write_all(b"\\t")?,
            c if (c as u32) < 0x20 => {
                write!(writer, "\\u{:04x}", c as u32)?;
            }
            c => {
                let mut buf = [0u8; 4];
                writer.write_all(c.encode_utf8(&mut buf).as_bytes())?;
            }
        }
    }
    writer.write_all(b"\"")?;
    Ok(())
}

fn write_html<W: Write>(
    dataset: &Dataset,
    view: &View,
    selected: &[usize],
    writer: &mut W,
) -> Result<()> {
    let columns = dataset.columns();
    writer.write_all(b"<table>\n<thead><tr>")?;
    for &col in selected {
        writer.write_all(b"<th>")?;
        write_html_escaped(writer, &columns[col])?;
        writer.write_all(b"</th>")?;
    }
    writer.write_all(b"</tr></thead>\n<tbody>\n")?;
    for data_row in view.iter() {
        let values = dataset.row_values(data_row as usize);
        writer.write_all(b"<tr>")?;
        for &col in selected {
            writer.write_all(b"<td>")?;
            write_html_escaped(writer, values.get(col).map(String::as_str).unwrap_or(""))?;
            writer.write_all(b"</td>")?;
        }
        writer.write_all(b"</tr>\n")?;
    }
    writer.write_all(b"</tbody>\n</table>\n")?;
    Ok(())
}

fn write_html_escaped<W: Write>(writer: &mut W, s: &str) -> Result<()> {
    for c in s.chars() {
        match c {
            '&' => writer.write_all(b"&amp;")?,
            '<' => writer.write_all(b"&lt;")?,
            '>' => writer.write_all(b"&gt;")?,
            '"' => writer.write_all(b"&quot;")?,
            '\'' => writer.write_all(b"&#39;")?,
            c => {
                let mut buf = [0u8; 4];
                writer.write_all(c.encode_utf8(&mut buf).as_bytes())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*; // brings `Write` into scope via the module's own imports
    use crate::ParserSchema;

    fn dataset(bytes: &[u8]) -> (Dataset, tempfile::NamedTempFile) {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        tf.write_all(bytes).unwrap();
        tf.flush().unwrap();
        (
            Dataset::open(tf.path(), Some(ParserSchema::csv())).unwrap(),
            tf,
        )
    }

    fn export_string(ds: &Dataset, view: &View, format: Format) -> String {
        export_cols(ds, view, format, &[])
    }

    fn export_cols(ds: &Dataset, view: &View, format: Format, cols: &[usize]) -> String {
        let out = tempfile::NamedTempFile::new().unwrap();
        export(ds, view, format, out.path(), cols).unwrap();
        std::fs::read_to_string(out.path()).unwrap()
    }

    #[test]
    fn csv_requotes_fields_with_commas() {
        let (ds, _t) = dataset(b"a,b\n1,hello\n2,\"has,comma\"\n");
        let s = export_string(&ds, &ds.view_all(), Format::Csv);
        assert_eq!(s, "a,b\n1,hello\n2,\"has,comma\"\n");
    }

    #[test]
    fn csv_escapes_embedded_quotes() {
        let (ds, _t) = dataset(b"a\n\"she said \"\"hi\"\"\"\n");
        let s = export_string(&ds, &ds.view_all(), Format::Csv);
        // value `she said "hi"` re-quoted with doubled quotes
        assert_eq!(s, "a\n\"she said \"\"hi\"\"\"\n");
    }

    #[test]
    fn tsv_uses_tabs() {
        let (ds, _t) = dataset(b"a,b\n1,x\n");
        assert_eq!(
            export_string(&ds, &ds.view_all(), Format::Tsv),
            "a\tb\n1\tx\n"
        );
    }

    #[test]
    fn json_escapes_values() {
        let (ds, _t) = dataset(b"k,v\n1,\"quote\"\"here\"\n");
        let s = export_string(&ds, &ds.view_all(), Format::Json);
        assert!(s.contains("\"k\": \"1\""));
        assert!(s.contains("\"v\": \"quote\\\"here\""));
    }

    #[test]
    fn html_escapes_markup() {
        let (ds, _t) = dataset(b"c\n<script>\n");
        let s = export_string(&ds, &ds.view_all(), Format::Html);
        assert!(s.contains("<th>c</th>"));
        assert!(s.contains("<td>&lt;script&gt;</td>"));
    }

    #[test]
    fn exports_only_view_rows() {
        let (ds, _t) = dataset(b"n\n1\n2\n3\n");
        let view = ds.search_view("2").unwrap();
        assert_eq!(export_string(&ds, &view, Format::Csv), "n\n2\n");
    }

    #[test]
    fn exports_only_selected_columns() {
        let (ds, _t) = dataset(b"a,b,c\n1,2,3\n4,5,6\n");
        // Hide column b: export only a (0) and c (2).
        assert_eq!(
            export_cols(&ds, &ds.view_all(), Format::Csv, &[0, 2]),
            "a,c\n1,3\n4,6\n"
        );
        // Reorder: c then a.
        assert_eq!(
            export_cols(&ds, &ds.view_all(), Format::Tsv, &[2, 0]),
            "c\ta\n3\t1\n6\t4\n"
        );
    }

    #[test]
    fn format_codes() {
        assert_eq!(Format::from_code(0), Some(Format::Csv));
        assert_eq!(Format::from_code(3), Some(Format::Html));
        assert_eq!(Format::from_code(9), None);
    }
}
