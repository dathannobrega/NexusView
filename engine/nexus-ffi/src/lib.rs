//! C-ABI bridge over `nexus-core` (RNF-04).
//!
//! Contract with the Swift/AppKit front end:
//! - Opaque handles only — `NexusDataset` and `NexusView` are never dereferenced
//!   by the caller. The heavy data stays in Rust; the bridge moves pointers,
//!   metadata, and exact viewport slices, never bulk rows (PRD exclusion #3).
//! - Strings returned by the engine are heap-allocated C strings the caller must
//!   release with [`nexus_string_free`]. Handles are released with
//!   [`nexus_close`] / [`nexus_view_free`].
//! - No panic ever crosses the boundary: every entry point is wrapped in
//!   `catch_unwind` (RNF-05). On failure, functions return a null/sentinel value
//!   and [`nexus_last_error`] holds a human-readable message (thread-local).
//!
//! Threading: a `Dataset` is immutable and `Sync`, so the UI may run
//! [`nexus_search`] on a background thread while the main thread reads cells of
//! the *previous* view. Views are immutable; the UI swaps the active pointer and
//! frees the old one.

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::OnceLock;

use nexus_core::{Dataset, GroupTree, Item, ParserSchema, View};

/// Opaque dataset handle.
pub struct NexusDataset(Dataset);
/// Opaque view (filter result) handle.
pub struct NexusView(View);
/// Opaque grouping-tree handle (RF-03).
pub struct NexusGroupTree(GroupTree);

/// Sentinel returned by group child accessors for an invalid/out-of-range item.
const ITEM_INVALID: i64 = i64::MIN;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

/// Record the most recent error message for this thread.
fn set_error(msg: impl Into<String>) {
    let cleaned = msg.into().replace('\0', " ");
    let c = CString::new(cleaned).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|e| *e.borrow_mut() = c);
}

fn clear_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::default());
}

/// Run `f`, converting any panic into the function's sentinel value (RNF-05).
fn guard<T>(sentinel: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            set_error("internal engine panic");
            sentinel
        }
    }
}

/// Borrow a `&str` from a C string pointer, or `None` if null / not UTF-8.
///
/// # Safety
/// `ptr` must be null or a valid NUL-terminated C string.
unsafe fn borrow_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Move a Rust `String` into a freshly allocated C string the caller owns.
fn to_owned_cstr(s: String) -> *mut c_char {
    match CString::new(s.replace('\0', "")) {
        Ok(c) => c.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Engine version string (static, never freed).
#[no_mangle]
pub extern "C" fn nexus_version() -> *const c_char {
    static VERSION: OnceLock<CString> = OnceLock::new();
    VERSION
        .get_or_init(|| CString::new(nexus_core::VERSION).unwrap_or_default())
        .as_ptr()
}

/// The last error on the current thread. Valid until the next engine call on
/// this thread; copy it immediately. Never null.
#[no_mangle]
pub extern "C" fn nexus_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Open and index a file. `schema_json` may be null (auto-detect) or a JSON/YAML
/// schema document. Returns null on failure (see [`nexus_last_error`]).
///
/// # Safety
/// `path` must be a valid C string; `schema_json` null or a valid C string.
#[no_mangle]
pub unsafe extern "C" fn nexus_open(
    path: *const c_char,
    schema_json: *const c_char,
) -> *mut NexusDataset {
    guard(ptr::null_mut(), || {
        clear_error();
        let Some(path) = (unsafe { borrow_str(path) }) else {
            set_error("path is null or not valid UTF-8");
            return ptr::null_mut();
        };

        let schema = match unsafe { borrow_str(schema_json) } {
            None => None,
            Some(doc) if doc.trim().is_empty() => None,
            Some(doc) => match ParserSchema::from_str_auto(doc) {
                Ok(s) => Some(s),
                Err(e) => {
                    set_error(e.to_string());
                    return ptr::null_mut();
                }
            },
        };

        match Dataset::open(path, schema) {
            Ok(ds) => Box::into_raw(Box::new(NexusDataset(ds))),
            Err(e) => {
                set_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// Release a dataset handle. Safe to call with null.
///
/// # Safety
/// `ds` must be null or a handle from [`nexus_open`], not previously closed.
#[no_mangle]
pub unsafe extern "C" fn nexus_close(ds: *mut NexusDataset) {
    if ds.is_null() {
        return;
    }
    guard((), || {
        drop(unsafe { Box::from_raw(ds) });
    });
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Total number of data rows.
///
/// # Safety
/// `ds` must be a valid dataset handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_row_count(ds: *const NexusDataset) -> u64 {
    guard(0, || match unsafe { ds.as_ref() } {
        Some(d) => d.0.row_count() as u64,
        None => 0,
    })
}

/// Number of columns.
///
/// # Safety
/// `ds` must be a valid dataset handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_column_count(ds: *const NexusDataset) -> u32 {
    guard(0, || match unsafe { ds.as_ref() } {
        Some(d) => d.0.column_count() as u32,
        None => 0,
    })
}

/// Name of column `col` as an owned C string (free with [`nexus_string_free`]),
/// or null if out of range.
///
/// # Safety
/// `ds` must be a valid dataset handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_column_name(ds: *const NexusDataset, col: u32) -> *mut c_char {
    guard(ptr::null_mut(), || match unsafe { ds.as_ref() } {
        Some(d) => match d.0.column_name(col as usize) {
            Some(name) => to_owned_cstr(name.to_string()),
            None => ptr::null_mut(),
        },
        None => ptr::null_mut(),
    })
}

// ---------------------------------------------------------------------------
// Search & views
// ---------------------------------------------------------------------------

/// The identity view (all rows). Free with [`nexus_view_free`]. Null on error.
///
/// # Safety
/// `ds` must be a valid dataset handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_view_all(ds: *const NexusDataset) -> *mut NexusView {
    guard(ptr::null_mut(), || match unsafe { ds.as_ref() } {
        Some(d) => Box::into_raw(Box::new(NexusView(d.0.view_all()))),
        None => {
            set_error("null dataset");
            ptr::null_mut()
        }
    })
}

/// Run a search, returning a new filtered view. Free with [`nexus_view_free`].
/// Null on error (invalid query, etc.).
///
/// # Safety
/// `ds` must be a valid dataset handle; `query` a valid C string.
#[no_mangle]
pub unsafe extern "C" fn nexus_search(
    ds: *const NexusDataset,
    query: *const c_char,
) -> *mut NexusView {
    guard(ptr::null_mut(), || {
        clear_error();
        let Some(d) = (unsafe { ds.as_ref() }) else {
            set_error("null dataset");
            return ptr::null_mut();
        };
        let Some(q) = (unsafe { borrow_str(query) }) else {
            set_error("query is null or not valid UTF-8");
            return ptr::null_mut();
        };
        match d.0.search_view(q) {
            Ok(view) => Box::into_raw(Box::new(NexusView(view))),
            Err(e) => {
                set_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// Number of rows in a view.
///
/// # Safety
/// `view` must be a valid view handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_view_count(view: *const NexusView) -> u64 {
    guard(0, || match unsafe { view.as_ref() } {
        Some(v) => v.0.len(),
        None => 0,
    })
}

/// Underlying data-row index for a view position, or -1 if out of range.
///
/// # Safety
/// `view` must be a valid view handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_view_row_id(view: *const NexusView, row: u64) -> i64 {
    guard(-1, || match unsafe { view.as_ref() } {
        Some(v) => v.0.row_id(row).map_or(-1, |id| id as i64),
        None => -1,
    })
}

/// Value of the cell at view position `row`, column `col`, as an owned C string
/// (free with [`nexus_string_free`]). Out-of-range yields an empty string; null
/// is returned only when a handle is null.
///
/// # Safety
/// `ds` and `view` must be valid handles or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_view_cell(
    ds: *const NexusDataset,
    view: *const NexusView,
    row: u64,
    col: u32,
) -> *mut c_char {
    guard(ptr::null_mut(), || {
        match (unsafe { ds.as_ref() }, unsafe { view.as_ref() }) {
            (Some(d), Some(v)) => to_owned_cstr(d.0.view_cell(&v.0, row, col as usize)),
            _ => ptr::null_mut(),
        }
    })
}

/// All column values for a view row, joined by `\u{1}` (0x01), as one owned C
/// string (free with [`nexus_string_free`]). Lets the UI fetch a whole row in a
/// single call instead of one call per cell — the hot path during horizontal
/// scroll. Any literal 0x01 inside a value is replaced with a space so the
/// caller can split safely.
///
/// # Safety
/// `ds` and `view` must be valid handles or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_view_row_cells(
    ds: *const NexusDataset,
    view: *const NexusView,
    row: u64,
) -> *mut c_char {
    guard(ptr::null_mut(), || {
        match (unsafe { ds.as_ref() }, unsafe { view.as_ref() }) {
            (Some(d), Some(v)) => match v.0.row_id(row) {
                Some(rid) => {
                    let values = d.0.row_values(rid as usize);
                    let joined = values
                        .iter()
                        .map(|s| s.replace('\u{1}', " "))
                        .collect::<Vec<_>>()
                        .join("\u{1}");
                    to_owned_cstr(joined)
                }
                None => to_owned_cstr(String::new()),
            },
            _ => ptr::null_mut(),
        }
    })
}

/// Value of the cell at absolute data-row `row`, column `col`, as an owned C
/// string (free with [`nexus_string_free`]). Out-of-range yields `""`. Used by
/// the grouping outline, whose leaves address absolute data rows.
///
/// # Safety
/// `ds` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_cell(ds: *const NexusDataset, row: u64, col: u32) -> *mut c_char {
    guard(ptr::null_mut(), || match unsafe { ds.as_ref() } {
        Some(d) => to_owned_cstr(d.0.cell(row as usize, col as usize)),
        None => ptr::null_mut(),
    })
}

/// The full raw record for a view position (original delimiters preserved) as an
/// owned C string. Used for row-level copy (RF-09).
///
/// # Safety
/// `ds` and `view` must be valid handles or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_view_row_raw(
    ds: *const NexusDataset,
    view: *const NexusView,
    row: u64,
) -> *mut c_char {
    guard(ptr::null_mut(), || {
        match (unsafe { ds.as_ref() }, unsafe { view.as_ref() }) {
            (Some(d), Some(v)) => match v.0.row_id(row) {
                Some(id) => to_owned_cstr(d.0.raw_line(id as usize)),
                None => to_owned_cstr(String::new()),
            },
            _ => ptr::null_mut(),
        }
    })
}

/// Stable multi-column sort of a view (RF-05). `cols[i]` is a column index and
/// `ascending[i]` is 0 (descending) or non-zero (ascending), for `count` keys.
/// Returns a new sorted view (free with [`nexus_view_free`]); null on error.
///
/// # Safety
/// `ds` and `view` must be valid handles. When `count > 0`, `cols` and
/// `ascending` must each point to at least `count` elements.
#[no_mangle]
pub unsafe extern "C" fn nexus_sort(
    ds: *const NexusDataset,
    view: *const NexusView,
    cols: *const u32,
    ascending: *const u8,
    count: usize,
) -> *mut NexusView {
    guard(ptr::null_mut(), || {
        let (Some(d), Some(v)) = (unsafe { ds.as_ref() }, unsafe { view.as_ref() }) else {
            set_error("null dataset or view");
            return ptr::null_mut();
        };
        if count > 0 && (cols.is_null() || ascending.is_null()) {
            set_error("null sort key arrays");
            return ptr::null_mut();
        }
        let keys: Vec<nexus_core::SortKey> = (0..count)
            .map(|i| nexus_core::SortKey {
                col: unsafe { *cols.add(i) } as usize,
                ascending: unsafe { *ascending.add(i) } != 0,
            })
            .collect();
        Box::into_raw(Box::new(NexusView(d.0.sort(&v.0, &keys))))
    })
}

/// Release a view handle. Safe with null.
///
/// # Safety
/// `view` must be null or a handle from [`nexus_search`] / [`nexus_view_all`].
#[no_mangle]
pub unsafe extern "C" fn nexus_view_free(view: *mut NexusView) {
    if view.is_null() {
        return;
    }
    guard((), || {
        drop(unsafe { Box::from_raw(view) });
    });
}

// ---------------------------------------------------------------------------
// Grouping (RF-03) — drives an NSOutlineView.
//
// Tree positions are passed as `i64` items: a value >= 0 is a group node id; a
// value < 0 is the data row `(-item - 1)`. Child accessors return
// `ITEM_INVALID` (= INT64_MIN) when out of range.
// ---------------------------------------------------------------------------

/// Build a grouping tree for `view` over `count` columns. Free with
/// [`nexus_group_free`]. Null on error.
///
/// # Safety
/// `ds` and `view` must be valid handles; when `count > 0`, `cols` must point to
/// at least `count` elements.
#[no_mangle]
pub unsafe extern "C" fn nexus_group(
    ds: *const NexusDataset,
    view: *const NexusView,
    cols: *const u32,
    count: usize,
) -> *mut NexusGroupTree {
    guard(ptr::null_mut(), || {
        let (Some(d), Some(v)) = (unsafe { ds.as_ref() }, unsafe { view.as_ref() }) else {
            set_error("null dataset or view");
            return ptr::null_mut();
        };
        if count > 0 && cols.is_null() {
            set_error("null group column array");
            return ptr::null_mut();
        }
        let columns: Vec<usize> = (0..count)
            .map(|i| unsafe { *cols.add(i) } as usize)
            .collect();
        Box::into_raw(Box::new(NexusGroupTree(d.0.group(&v.0, &columns))))
    })
}

/// Number of top-level groups.
///
/// # Safety
/// `tree` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_group_root_count(tree: *const NexusGroupTree) -> u64 {
    guard(0, || match unsafe { tree.as_ref() } {
        Some(t) => t.0.root_count() as u64,
        None => 0,
    })
}

/// The i-th top-level group as an encoded item, or `ITEM_INVALID`.
///
/// # Safety
/// `tree` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_group_root_child(tree: *const NexusGroupTree, index: u64) -> i64 {
    guard(ITEM_INVALID, || match unsafe { tree.as_ref() } {
        Some(t) => {
            t.0.root_child(index as usize)
                .map_or(ITEM_INVALID, Item::encode)
        }
        None => ITEM_INVALID,
    })
}

/// Number of children of `item` (sub-groups or rows).
///
/// # Safety
/// `tree` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_group_child_count(tree: *const NexusGroupTree, item: i64) -> u64 {
    guard(0, || match unsafe { tree.as_ref() } {
        Some(t) => t.0.child_count(Item::decode(item)) as u64,
        None => 0,
    })
}

/// The i-th child of `item` as an encoded item, or `ITEM_INVALID`.
///
/// # Safety
/// `tree` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_group_child(
    tree: *const NexusGroupTree,
    item: i64,
    index: u64,
) -> i64 {
    guard(ITEM_INVALID, || match unsafe { tree.as_ref() } {
        Some(t) => {
            t.0.child(Item::decode(item), index as usize)
                .map_or(ITEM_INVALID, Item::encode)
        }
        None => ITEM_INVALID,
    })
}

/// 1 if `item` is an expandable group node, else 0.
///
/// # Safety
/// `tree` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_group_is_group(tree: *const NexusGroupTree, item: i64) -> u8 {
    guard(0, || match unsafe { tree.as_ref() } {
        Some(t) => u8::from(t.0.is_group(Item::decode(item))),
        None => 0,
    })
}

/// Group label as an owned C string (free with [`nexus_string_free`]); empty for
/// row items.
///
/// # Safety
/// `tree` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_group_label(tree: *const NexusGroupTree, item: i64) -> *mut c_char {
    guard(ptr::null_mut(), || match unsafe { tree.as_ref() } {
        Some(t) => to_owned_cstr(t.0.label(Item::decode(item)).to_string()),
        None => ptr::null_mut(),
    })
}

/// Aggregate row count under `item` (1 for a row item).
///
/// # Safety
/// `tree` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_group_count(tree: *const NexusGroupTree, item: i64) -> u64 {
    guard(0, || match unsafe { tree.as_ref() } {
        Some(t) => t.0.count(Item::decode(item)),
        None => 0,
    })
}

/// Data row id for a row item, or -1 for a group / invalid.
///
/// # Safety
/// `tree` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_group_row(tree: *const NexusGroupTree, item: i64) -> i64 {
    guard(-1, || match unsafe { tree.as_ref() } {
        Some(t) => t.0.row(Item::decode(item)).map_or(-1, |r| r as i64),
        None => -1,
    })
}

/// Export `view` to `path` (RF-10). `format`: 0=CSV, 1=TSV, 2=JSON, 3=HTML.
/// `cols` selects the columns to include, in order; pass `ncols == 0` (or a null
/// `cols`) to export all columns. Hidden columns are excluded by omitting them.
/// Returns 0 on success, -1 on error (see [`nexus_last_error`]).
///
/// # Safety
/// `ds` and `view` must be valid handles; `path` a valid C string; when
/// `ncols > 0`, `cols` must point to at least `ncols` elements.
#[no_mangle]
pub unsafe extern "C" fn nexus_export(
    ds: *const NexusDataset,
    view: *const NexusView,
    format: u32,
    cols: *const u32,
    ncols: usize,
    path: *const c_char,
) -> i32 {
    guard(-1, || {
        clear_error();
        let (Some(d), Some(v)) = (unsafe { ds.as_ref() }, unsafe { view.as_ref() }) else {
            set_error("null dataset or view");
            return -1;
        };
        let Some(path) = (unsafe { borrow_str(path) }) else {
            set_error("path is null or not valid UTF-8");
            return -1;
        };
        let Some(format) = nexus_core::export::Format::from_code(format) else {
            set_error("unknown export format");
            return -1;
        };
        let columns: Vec<usize> = if ncols == 0 || cols.is_null() {
            Vec::new()
        } else {
            (0..ncols)
                .map(|i| unsafe { *cols.add(i) } as usize)
                .collect()
        };
        match d
            .0
            .export(&v.0, format, std::path::Path::new(path), &columns)
        {
            Ok(()) => 0,
            Err(e) => {
                set_error(e.to_string());
                -1
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Row tagging — persists across filter/sort/group (Timeline Explorer style).
// ---------------------------------------------------------------------------

/// Tag (`tagged != 0`) or untag an absolute data row.
///
/// # Safety
/// `ds` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_set_tag(ds: *const NexusDataset, row: u64, tagged: u8) {
    guard((), || {
        if let Some(d) = unsafe { ds.as_ref() } {
            d.0.set_tag(row as usize, tagged != 0);
        }
    })
}

/// 1 if the absolute data row is tagged, else 0.
///
/// # Safety
/// `ds` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_is_tagged(ds: *const NexusDataset, row: u64) -> u8 {
    guard(0, || match unsafe { ds.as_ref() } {
        Some(d) => u8::from(d.0.is_tagged(row as usize)),
        None => 0,
    })
}

/// Number of tagged rows.
///
/// # Safety
/// `ds` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_tagged_count(ds: *const NexusDataset) -> u64 {
    guard(0, || match unsafe { ds.as_ref() } {
        Some(d) => d.0.tagged_count() as u64,
        None => 0,
    })
}

/// Untag every row.
///
/// # Safety
/// `ds` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_clear_tags(ds: *const NexusDataset) {
    guard((), || {
        if let Some(d) = unsafe { ds.as_ref() } {
            d.0.clear_tags();
        }
    })
}

/// A view of all tagged rows (free with [`nexus_view_free`]). Null on error.
///
/// # Safety
/// `ds` must be a valid handle or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_tagged_view(ds: *const NexusDataset) -> *mut NexusView {
    guard(ptr::null_mut(), || match unsafe { ds.as_ref() } {
        Some(d) => Box::into_raw(Box::new(NexusView(d.0.tagged_view()))),
        None => ptr::null_mut(),
    })
}

/// The tagged subset of `view` (free with [`nexus_view_free`]). Null on error.
///
/// # Safety
/// `ds` and `view` must be valid handles or null.
#[no_mangle]
pub unsafe extern "C" fn nexus_intersect_tags(
    ds: *const NexusDataset,
    view: *const NexusView,
) -> *mut NexusView {
    guard(ptr::null_mut(), || {
        match (unsafe { ds.as_ref() }, unsafe { view.as_ref() }) {
            (Some(d), Some(v)) => Box::into_raw(Box::new(NexusView(d.0.intersect_tags(&v.0)))),
            _ => ptr::null_mut(),
        }
    })
}

/// Release a grouping tree (NULL-safe).
///
/// # Safety
/// `tree` must be null or a handle from [`nexus_group`].
#[no_mangle]
pub unsafe extern "C" fn nexus_group_free(tree: *mut NexusGroupTree) {
    if tree.is_null() {
        return;
    }
    guard((), || {
        drop(unsafe { Box::from_raw(tree) });
    });
}

/// Free a C string previously returned by the engine. Safe with null.
///
/// # Safety
/// `s` must be null or a string returned by this library, freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn nexus_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    guard((), || {
        drop(unsafe { CString::from_raw(s) });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Copy an engine-owned C string into a Rust `String` and free it.
    ///
    /// # Safety
    /// `p` must be a non-null string returned by the engine.
    unsafe fn take(p: *mut c_char) -> String {
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_owned();
        unsafe { nexus_string_free(p) };
        s
    }

    #[test]
    fn ffi_roundtrip_and_null_safety() {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        write!(tf, "host,event\nweb01,login\nweb02,logout\nweb01,error\n").unwrap();
        tf.flush().unwrap();
        let path = CString::new(tf.path().to_str().unwrap()).unwrap();

        unsafe {
            // Open + metadata.
            let ds = nexus_open(path.as_ptr(), ptr::null());
            assert!(!ds.is_null());
            assert_eq!(nexus_row_count(ds), 3);
            assert_eq!(nexus_column_count(ds), 2);
            assert_eq!(take(nexus_column_name(ds, 0)), "host");
            assert!(nexus_column_name(ds, 99).is_null());

            // Search + view addressing.
            let q = CString::new("host:web01").unwrap();
            let view = nexus_search(ds, q.as_ptr());
            assert!(!view.is_null());
            assert_eq!(nexus_view_count(view), 2);
            assert_eq!(take(nexus_view_cell(ds, view, 0, 1)), "login");
            assert_eq!(nexus_view_row_id(view, 0), 0);
            assert_eq!(nexus_view_row_id(view, 1), 2);
            assert_eq!(nexus_view_row_id(view, 99), -1);
            nexus_view_free(view);

            // Identity view.
            let all = nexus_view_all(ds);
            assert_eq!(nexus_view_count(all), 3);
            nexus_view_free(all);

            // Invalid query → null + non-empty error.
            let bad = CString::new("/(/").unwrap();
            assert!(nexus_search(ds, bad.as_ptr()).is_null());
            assert!(!CStr::from_ptr(nexus_last_error()).to_bytes().is_empty());

            // Null inputs must never crash.
            assert_eq!(nexus_row_count(ptr::null()), 0);
            assert!(nexus_search(ptr::null(), q.as_ptr()).is_null());
            nexus_close(ptr::null_mut());
            nexus_view_free(ptr::null_mut());
            nexus_string_free(ptr::null_mut());

            nexus_close(ds);
        }
    }

    #[test]
    fn ffi_sort() {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        write!(tf, "n\n10\n2\n1\n").unwrap();
        tf.flush().unwrap();
        let path = CString::new(tf.path().to_str().unwrap()).unwrap();
        unsafe {
            let ds = nexus_open(path.as_ptr(), ptr::null());
            let all = nexus_view_all(ds);
            let cols = [0u32];
            let asc = [1u8];
            let sorted = nexus_sort(ds, all, cols.as_ptr(), asc.as_ptr(), 1);
            assert!(!sorted.is_null());
            // Numeric, not lexicographic: 1, 2, 10.
            assert_eq!(take(nexus_view_cell(ds, sorted, 0, 0)), "1");
            assert_eq!(take(nexus_view_cell(ds, sorted, 1, 0)), "2");
            assert_eq!(take(nexus_view_cell(ds, sorted, 2, 0)), "10");
            nexus_view_free(sorted);
            nexus_view_free(all);
            nexus_close(ds);
        }
    }

    #[test]
    fn ffi_group() {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        write!(tf, "host,sev\nweb01,INFO\nweb01,ERROR\nweb02,INFO\n").unwrap();
        tf.flush().unwrap();
        let path = CString::new(tf.path().to_str().unwrap()).unwrap();
        unsafe {
            let ds = nexus_open(path.as_ptr(), ptr::null());
            let all = nexus_view_all(ds);
            let cols = [0u32]; // group by host
            let tree = nexus_group(ds, all, cols.as_ptr(), 1);
            assert!(!tree.is_null());
            assert_eq!(nexus_group_root_count(tree), 2);

            let g0 = nexus_group_root_child(tree, 0);
            assert_ne!(g0, ITEM_INVALID);
            assert_eq!(nexus_group_is_group(tree, g0), 1);
            assert_eq!(take(nexus_group_label(tree, g0)), "web01");
            assert_eq!(nexus_group_count(tree, g0), 2);
            assert_eq!(nexus_group_child_count(tree, g0), 2);

            // A child of the web01 group is a (non-group) row item.
            let row = nexus_group_child(tree, g0, 0);
            assert_eq!(nexus_group_is_group(tree, row), 0);
            assert!(nexus_group_row(tree, row) >= 0);

            // Out-of-range child returns the invalid sentinel.
            assert_eq!(nexus_group_child(tree, g0, 99), ITEM_INVALID);

            nexus_group_free(tree);
            nexus_view_free(all);
            nexus_close(ds);
        }
    }

    #[test]
    fn ffi_export_csv() {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        write!(tf, "a,b\n1,x\n2,y\n").unwrap();
        tf.flush().unwrap();
        let path = CString::new(tf.path().to_str().unwrap()).unwrap();
        let out = tempfile::NamedTempFile::new().unwrap();
        let out_path = CString::new(out.path().to_str().unwrap()).unwrap();
        unsafe {
            let ds = nexus_open(path.as_ptr(), ptr::null());
            let all = nexus_view_all(ds);
            // All columns.
            assert_eq!(
                nexus_export(ds, all, 0, ptr::null(), 0, out_path.as_ptr()),
                0
            );
            assert_eq!(
                std::fs::read_to_string(out.path()).unwrap(),
                "a,b\n1,x\n2,y\n"
            );
            // Only column b (index 1) — the hide-columns path.
            let cols = [1u32];
            assert_eq!(
                nexus_export(ds, all, 0, cols.as_ptr(), 1, out_path.as_ptr()),
                0
            );
            assert_eq!(std::fs::read_to_string(out.path()).unwrap(), "b\nx\ny\n");
            assert_eq!(
                nexus_export(ds, all, 99, ptr::null(), 0, out_path.as_ptr()),
                -1
            ); // bad format
            nexus_view_free(all);
            nexus_close(ds);
        }
    }

    #[test]
    fn ffi_tags() {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        write!(tf, "host\nweb01\nweb02\nweb03\n").unwrap();
        tf.flush().unwrap();
        let path = CString::new(tf.path().to_str().unwrap()).unwrap();
        unsafe {
            let ds = nexus_open(path.as_ptr(), ptr::null());
            assert_eq!(nexus_tagged_count(ds), 0);
            nexus_set_tag(ds, 0, 1);
            nexus_set_tag(ds, 2, 1);
            assert_eq!(nexus_is_tagged(ds, 0), 1);
            assert_eq!(nexus_is_tagged(ds, 1), 0);
            assert_eq!(nexus_tagged_count(ds), 2);

            let tagged = nexus_tagged_view(ds);
            assert_eq!(nexus_view_count(tagged), 2);
            assert_eq!(nexus_view_row_id(tagged, 0), 0);
            assert_eq!(nexus_view_row_id(tagged, 1), 2);
            nexus_view_free(tagged);

            nexus_set_tag(ds, 0, 0);
            assert_eq!(nexus_tagged_count(ds), 1);
            nexus_clear_tags(ds);
            assert_eq!(nexus_tagged_count(ds), 0);

            // null-safety
            nexus_set_tag(ptr::null(), 0, 1);
            assert_eq!(nexus_is_tagged(ptr::null(), 0), 0);
            nexus_close(ds);
        }
    }

    #[test]
    fn ffi_row_cells() {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        write!(tf, "a,b,c\n1,2,3\n4,5,6\n").unwrap();
        tf.flush().unwrap();
        let path = CString::new(tf.path().to_str().unwrap()).unwrap();
        unsafe {
            let ds = nexus_open(path.as_ptr(), ptr::null());
            let all = nexus_view_all(ds);
            assert_eq!(take(nexus_view_row_cells(ds, all, 0)), "1\u{1}2\u{1}3");
            assert_eq!(take(nexus_view_row_cells(ds, all, 1)), "4\u{1}5\u{1}6");
            nexus_view_free(all);
            nexus_close(ds);
        }
    }

    #[test]
    fn open_missing_file_reports_error() {
        let path = CString::new("/nonexistent/path/to/file.csv").unwrap();
        unsafe {
            let ds = nexus_open(path.as_ptr(), ptr::null());
            assert!(ds.is_null());
            assert!(!CStr::from_ptr(nexus_last_error()).to_bytes().is_empty());
        }
    }
}
