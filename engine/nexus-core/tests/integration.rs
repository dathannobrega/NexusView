//! End-to-end tests over the public `nexus-core` API, exercising the full
//! pipeline (open → index → parse → search → view) against realistic,
//! deliberately messy forensic input.

use nexus_core::{Dataset, ParserSchema};
use std::io::Write;

/// Write `bytes` to a temp file and open it as a dataset.
fn open(bytes: &[u8], schema: Option<ParserSchema>) -> (Dataset, tempfile::NamedTempFile) {
    let mut tf = tempfile::NamedTempFile::new().unwrap();
    tf.write_all(bytes).unwrap();
    tf.flush().unwrap();
    let ds = Dataset::open(tf.path(), schema).unwrap();
    (ds, tf)
}

#[test]
fn full_pipeline_csv() {
    // Mixed hazards: a quoted field containing the delimiter, CRLF line endings,
    // a NUL byte, and a Windows-1252 byte (0xE9 = 'é').
    let data: &[u8] = b"timestamp,host,message\r\n\
        2026-06-05T10:00:00,web01,\"login ok, session=1\"\r\n\
        2026-06-05T10:01:00,web02,login fail\x00 retry\r\n\
        2026-06-05T10:02:00,db\xE9,disk error\r\n\
        2026-06-05T10:03:00,web01,timeout\r\n";

    let (ds, _t) = open(data, None);

    // Structure.
    assert_eq!(ds.column_count(), 3);
    assert_eq!(ds.columns(), &["timestamp", "host", "message"]);
    assert_eq!(ds.row_count(), 4);

    // Cell extraction handles quotes, NUL, and CP1252.
    assert_eq!(ds.cell(0, 2), "login ok, session=1");
    assert_eq!(ds.cell(1, 2), "login fail retry"); // NUL stripped
    assert_eq!(ds.cell(2, 1), "dbé"); // CP1252 decoded

    // Global substring (case-insensitive).
    assert_eq!(ds.search("LOGIN").unwrap(), vec![0, 1]);

    // Boolean operators.
    assert_eq!(ds.search("login AND NOT fail").unwrap(), vec![0]);
    assert_eq!(ds.search("timeout OR disk").unwrap(), vec![2, 3]);

    // Column-scoped.
    assert_eq!(ds.search("host:web01").unwrap(), vec![0, 3]);

    // Regex.
    assert_eq!(ds.search(r"/web\d+/").unwrap(), vec![0, 1, 3]);

    // Empty query → all rows.
    assert_eq!(ds.search("").unwrap(), vec![0, 1, 2, 3]);
}

#[test]
fn bodyfile_preset() {
    let data: &[u8] = b"0|/etc/passwd|2|r--|0|0|1024|100|200|300|400\n\
        0|/var/log/auth.log|3|rw-|0|0|4096|101|201|301|401\n";
    let (ds, _t) = open(data, Some(ParserSchema::bodyfile()));

    assert_eq!(ds.column_count(), 11);
    assert_eq!(ds.column_name(1), Some("name"));
    assert_eq!(ds.row_count(), 2);
    assert_eq!(ds.cell(0, 1), "/etc/passwd");
    assert_eq!(ds.search("name:/auth/").unwrap(), vec![1]);
}

#[test]
fn view_addressing_is_stable() {
    let data: &[u8] = b"id,kind\n1,alpha\n2,beta\n3,alpha\n4,gamma\n";
    let (ds, _t) = open(data, None);

    let view = ds.search_view("alpha").unwrap();
    assert_eq!(view.len(), 2);
    // Filtered view rows map back to original rows 0 and 2.
    assert_eq!(ds.view_cell(&view, 0, 0), "1");
    assert_eq!(ds.view_cell(&view, 1, 0), "3");
    // Out-of-range view access is safe.
    assert_eq!(ds.view_cell(&view, 99, 0), "");
}

#[test]
fn large_synthetic_file_scales() {
    // 100k rows: verifies indexing + parallel search stay correct at volume.
    let mut data = String::from("n,parity\n");
    for i in 0..100_000u32 {
        data.push_str(&format!(
            "{i},{}\n",
            if i % 2 == 0 { "even" } else { "odd" }
        ));
    }
    let (ds, _t) = open(data.as_bytes(), None);

    assert_eq!(ds.row_count(), 100_000);
    let evens = ds.search("parity:even").unwrap();
    assert_eq!(evens.len(), 50_000);
    assert_eq!(evens[0], 0);
    assert_eq!(evens[1], 2);
}

#[test]
fn invalid_query_surfaces_error() {
    let (ds, _t) = open(b"a,b\n1,2\n", None);
    assert!(ds.search("/(/").is_err());
    assert!(ds.search("(a OR b").is_err());
}

#[test]
fn multiline_quoted_records_full_pipeline() {
    // Mirrors the shape of a Sophos datalake export: UTF-8 BOM, ISO timestamps
    // carrying a trailing `TUTC` annotation, and quoted PID-list fields that
    // span several physical lines (RFC 4180 embedded newlines).
    let data: &[u8] = b"\xEF\xBB\xBFDevice,FirstSeen,Cmd,PIDs,LastSeen\n\
        web01,2026-06-10T14:48:57ZTUTC,\"run a,b\",\"18452:134256190834473343\n\
330:16480:134255987192750638\n\
72:134254749000664849\",2026-06-11T02:38:03ZTUTC\n\
        web02,2026-06-10T12:46:51ZTUTC,calc,14712:134255830517435285,2026-06-10T13:00:00ZTUTC\n";
    let (ds, _t) = open(data, None);

    // Structure: BOM stripped from the first column, 2 records — not 4 lines.
    assert_eq!(ds.column_count(), 5);
    assert_eq!(ds.column_name(0), Some("Device"));
    assert_eq!(ds.row_count(), 2);

    // The multi-line field is a single cell with its newlines preserved.
    let pids = ds.cell(0, 3);
    assert_eq!(pids.lines().count(), 3);
    assert!(pids.contains("330:16480:134255987192750638"));
    assert_eq!(ds.cell(0, 4), "2026-06-11T02:38:03ZTUTC");

    // Search reaches continuation lines, scoped and global.
    assert_eq!(ds.search("PIDs:134254749000664849").unwrap(), vec![0]);
    assert_eq!(ds.search("14712").unwrap(), vec![1]);

    // Sort by FirstSeen: ISO text keys order chronologically.
    let sorted = ds.sort(
        &ds.view_all(),
        &[nexus_core::SortKey {
            col: 1,
            ascending: true,
        }],
    );
    assert_eq!(ds.view_cell(&sorted, 0, 0), "web02");

    // Export the view as CSV and re-open it: the multi-line cell must
    // round-trip byte-identically.
    let out = tempfile::NamedTempFile::new().unwrap();
    ds.export(
        &ds.view_all(),
        nexus_core::export::Format::Csv,
        out.path(),
        &[],
    )
    .unwrap();
    let ds2 = Dataset::open(out.path(), None).unwrap();
    assert_eq!(ds2.row_count(), 2);
    assert_eq!(ds2.cell(0, 3), pids);
    assert_eq!(ds2.cell(1, 0), "web02");
}
