//! Dynamic, injectable parser schemas (RF-01).
//!
//! A schema describes how to split a record into fields. It can be loaded from
//! JSON or YAML at runtime, so the tool adapts to new telemetry formats without
//! recompiling. Built-in presets cover the common DFIR formats, and [`sniff`]
//! auto-detects a delimiter when no schema is supplied.

use crate::error::{NexusError, Result};
use serde::{Deserialize, Serialize};

/// How to interpret a delimited text file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ParserSchema {
    /// Optional human label (e.g. "bodyfile", "evtxecmd").
    pub name: Option<String>,
    /// Field delimiter. Accepts a single character (`","`, `"|"`, `"\t"`) or a
    /// friendly name (`"comma"`, `"tab"`, `"pipe"`, `"semicolon"`, `"space"`).
    pub delimiter: String,
    /// Quote character; delimiters inside a quoted field are ignored.
    pub quote: char,
    /// Whether the first non-skipped line holds column names.
    pub has_header: bool,
    /// Explicit column names. When non-empty these win over a header row.
    pub columns: Vec<String>,
    /// Number of leading lines to ignore before the header/data (banners, notes).
    pub skip_lines: usize,
}

impl Default for ParserSchema {
    fn default() -> Self {
        Self {
            name: None,
            delimiter: "comma".to_string(),
            quote: '"',
            has_header: true,
            columns: Vec::new(),
            skip_lines: 0,
        }
    }
}

impl ParserSchema {
    /// Parse a schema from a JSON document.
    pub fn from_json(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|e| NexusError::InvalidSchema(e.to_string()))
    }

    /// Parse a schema from a YAML document.
    pub fn from_yaml(s: &str) -> Result<Self> {
        serde_yaml::from_str(s).map_err(|e| NexusError::InvalidSchema(e.to_string()))
    }

    /// Parse from JSON or YAML, auto-detecting which by a leading `{`.
    pub fn from_str_auto(s: &str) -> Result<Self> {
        let trimmed = s.trim_start();
        if trimmed.starts_with('{') {
            Self::from_json(s)
        } else {
            Self::from_yaml(s)
        }
    }

    /// The delimiter resolved to a single byte.
    pub fn delimiter_byte(&self) -> u8 {
        match self.delimiter.as_str() {
            "comma" | "," => b',',
            "tab" | "\t" | "\\t" => b'\t',
            "pipe" | "|" => b'|',
            "semicolon" | ";" => b';',
            "space" => b' ',
            other => other.as_bytes().first().copied().unwrap_or(b','),
        }
    }

    /// The quote character as a byte (`"` if the configured char is non-ASCII).
    pub fn quote_byte(&self) -> u8 {
        let c = self.quote as u32;
        if c <= 0x7F {
            c as u8
        } else {
            b'"'
        }
    }

    // --- Built-in presets for common DFIR formats -------------------------

    /// Comma-separated values with a header row.
    pub fn csv() -> Self {
        Self {
            name: Some("csv".into()),
            delimiter: "comma".into(),
            ..Self::default()
        }
    }

    /// Tab-separated values with a header row.
    pub fn tsv() -> Self {
        Self {
            name: Some("tsv".into()),
            delimiter: "tab".into(),
            ..Self::default()
        }
    }

    /// Pipe-separated values with a header row.
    pub fn psv() -> Self {
        Self {
            name: Some("psv".into()),
            delimiter: "pipe".into(),
            ..Self::default()
        }
    }

    /// The Sleuth Kit `mactime` bodyfile: 11 pipe-separated fields, no header.
    pub fn bodyfile() -> Self {
        Self {
            name: Some("bodyfile".into()),
            delimiter: "pipe".into(),
            quote: '"',
            has_header: false,
            columns: vec![
                "MD5".into(),
                "name".into(),
                "inode".into(),
                "mode".into(),
                "UID".into(),
                "GID".into(),
                "size".into(),
                "atime".into(),
                "mtime".into(),
                "ctime".into(),
                "crtime".into(),
            ],
            skip_lines: 0,
        }
    }

    /// EvtxECmd CSV output (standard CSV with a header row).
    pub fn evtxecmd() -> Self {
        Self {
            name: Some("evtxecmd".into()),
            ..Self::csv()
        }
    }

    /// Look up a preset by name (case-insensitive).
    pub fn preset(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::csv()),
            "tsv" => Some(Self::tsv()),
            "psv" => Some(Self::psv()),
            "bodyfile" => Some(Self::bodyfile()),
            "evtxecmd" => Some(Self::evtxecmd()),
            _ => None,
        }
    }
}

/// Auto-detect a delimiter from a sample of the file's first lines (RF-01).
///
/// Picks whichever candidate delimiter appears most consistently across the
/// sampled lines. Defaults to comma when nothing stands out.
pub fn sniff(sample: &[u8]) -> ParserSchema {
    const CANDIDATES: [u8; 4] = [b',', b'\t', b'|', b';'];
    let mut totals = [0usize; CANDIDATES.len()];

    for line in sample.split(|&b| b == b'\n').take(16) {
        if line.is_empty() {
            continue;
        }
        for (k, &c) in CANDIDATES.iter().enumerate() {
            totals[k] += memchr::memchr_iter(c, line).count();
        }
    }

    let best = totals
        .iter()
        .enumerate()
        .max_by_key(|(_, &n)| n)
        .filter(|(_, &n)| n > 0)
        .map_or(b',', |(i, _)| CANDIDATES[i]);

    ParserSchema {
        delimiter: byte_to_delimiter_name(best).to_string(),
        ..ParserSchema::default()
    }
}

fn byte_to_delimiter_name(b: u8) -> &'static str {
    match b {
        b',' => "comma",
        b'\t' => "tab",
        b'|' => "pipe",
        b';' => "semicolon",
        b' ' => "space",
        _ => "comma",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiter_resolution() {
        assert_eq!(ParserSchema::csv().delimiter_byte(), b',');
        assert_eq!(ParserSchema::tsv().delimiter_byte(), b'\t');
        assert_eq!(ParserSchema::psv().delimiter_byte(), b'|');
        assert_eq!(ParserSchema::bodyfile().delimiter_byte(), b'|');
    }

    #[test]
    fn sniff_detects_pipe_and_tab() {
        assert_eq!(sniff(b"a|b|c\n1|2|3").delimiter_byte(), b'|');
        assert_eq!(sniff(b"a\tb\tc\n1\t2\t3").delimiter_byte(), b'\t');
        assert_eq!(sniff(b"a,b,c\n1,2,3").delimiter_byte(), b',');
    }

    #[test]
    fn json_roundtrip() {
        let s = ParserSchema::from_json(r#"{"delimiter":"pipe","has_header":false}"#).unwrap();
        assert_eq!(s.delimiter_byte(), b'|');
        assert!(!s.has_header);
    }

    #[test]
    fn yaml_parse() {
        let s = ParserSchema::from_yaml("delimiter: tab\nhas_header: true\n").unwrap();
        assert_eq!(s.delimiter_byte(), b'\t');
        assert!(s.has_header);
    }

    #[test]
    fn bodyfile_preset_shape() {
        let s = ParserSchema::bodyfile();
        assert_eq!(s.columns.len(), 11);
        assert!(!s.has_header);
    }
}
