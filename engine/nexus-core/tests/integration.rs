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
