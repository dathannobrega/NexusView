import CNexusEngine
import Foundation

/// Safe Swift wrapper over the `nexus-ffi` C ABI.
///
/// Ownership is mirrored in Swift's lifecycle: `Engine` owns the dataset handle
/// and frees it on `deinit`; `DataView` owns a view handle and frees it on
/// `deinit`. Every C string returned by the engine is released immediately after
/// being copied into a Swift `String`.
///
/// Thread-safety: the underlying dataset is immutable, so `search(_:)` may run on
/// a background queue while the main thread reads cells of a *different*
/// (previous) `DataView`. Only swap the active view on the main thread.
final class Engine {
    /// Opaque `NexusDataset *`.
    private let handle: OpaquePointer

    /// Open and index a file. `schemaJSON` is an optional JSON/YAML schema.
    /// Returns `nil` on failure; `Engine.lastError` then holds the reason.
    init?(path: String, schemaJSON: String? = nil) {
        guard let handle = nexus_open(path, schemaJSON) else { return nil }
        self.handle = handle
    }

    deinit {
        nexus_close(handle)
    }

    /// Total number of data rows.
    var rowCount: Int { Int(nexus_row_count(handle)) }

    /// Number of columns.
    var columnCount: Int { Int(nexus_column_count(handle)) }

    /// Name of column `index` (empty if out of range).
    func columnName(_ index: Int) -> String {
        guard let c = nexus_column_name(handle, UInt32(index)) else { return "" }
        defer { nexus_string_free(c) }
        return String(cString: c)
    }

    /// The identity view over all rows (no allocation in the engine).
    func allRows() -> DataView {
        // nexus_view_all only returns null for a null dataset, which cannot
        // happen here; fall back defensively to an empty view.
        guard let v = nexus_view_all(handle) else { return DataView.empty }
        return DataView(v)
    }

    /// Run a search, returning a filtered view or a typed error.
    func search(_ query: String) -> Result<DataView, EngineError> {
        if let v = nexus_search(handle, query) {
            return .success(DataView(v))
        }
        return .failure(EngineError(message: String(cString: nexus_last_error())))
    }

    /// Value of the cell at (`row`, `col`) within `view`.
    func cell(view: DataView, row: Int, col: Int) -> String {
        guard let v = view.handle,
              let c = nexus_view_cell(handle, v, UInt64(row), UInt32(col))
        else { return "" }
        defer { nexus_string_free(c) }
        return String(cString: c)
    }

    /// All column values for a view row in a single FFI call (split on 0x01).
    /// Used by the grid render path so a row costs one call, not one per cell.
    func rowCells(view: DataView, row: Int) -> [String] {
        guard let v = view.handle,
              let c = nexus_view_row_cells(handle, v, UInt64(row))
        else { return [] }
        defer { nexus_string_free(c) }
        return String(cString: c).components(separatedBy: "\u{01}")
    }

    /// Value of the cell at an absolute data-row index (used by grouped leaves).
    func cell(dataRow: Int, col: Int) -> String {
        guard let c = nexus_cell(handle, UInt64(dataRow), UInt32(col)) else { return "" }
        defer { nexus_string_free(c) }
        return String(cString: c)
    }

    /// Absolute data-row index for a position within `view`, or `nil`.
    func dataRowId(view: DataView, viewRow: Int) -> Int? {
        guard let v = view.handle else { return nil }
        let id = nexus_view_row_id(v, UInt64(viewRow))
        return id >= 0 ? Int(id) : nil
    }

    /// Build a multi-level grouping tree over `view`. `nil` if `cols` is empty
    /// or on error.
    func group(view: DataView, cols: [Int]) -> GroupTree? {
        guard let v = view.handle, !cols.isEmpty else { return nil }
        let columns = cols.map { UInt32($0) }
        let result = columns.withUnsafeBufferPointer { ptr in
            nexus_group(handle, v, ptr.baseAddress, cols.count)
        }
        guard let result else { return nil }
        return GroupTree(result)
    }

    /// Stable multi-column sort of `view`. Returns the same view when `keys` is
    /// empty or on error (so the caller never loses its current view).
    func sort(view: DataView, keys: [(col: Int, ascending: Bool)]) -> DataView {
        guard let v = view.handle, !keys.isEmpty else { return view }
        let cols = keys.map { UInt32($0.col) }
        let asc = keys.map { UInt8($0.ascending ? 1 : 0) }
        let result = cols.withUnsafeBufferPointer { colPtr in
            asc.withUnsafeBufferPointer { ascPtr in
                nexus_sort(handle, v, colPtr.baseAddress, ascPtr.baseAddress, keys.count)
            }
        }
        guard let result else { return view }
        return DataView(result)
    }

    /// The full raw record for a view row (original delimiters preserved).
    func rawLine(view: DataView, row: Int) -> String {
        guard let v = view.handle,
              let c = nexus_view_row_raw(handle, v, UInt64(row))
        else { return "" }
        defer { nexus_string_free(c) }
        return String(cString: c)
    }

    /// Export `view` to `path`. `format`: 0=CSV, 1=TSV, 2=JSON, 3=HTML.
    /// `columns` selects and orders the columns to include (empty = all).
    /// Returns `true` on success.
    func export(view: DataView, format: UInt32, columns: [Int], path: String) -> Bool {
        guard let v = view.handle else { return false }
        let cols = columns.map { UInt32($0) }
        let rc = cols.withUnsafeBufferPointer { ptr in
            nexus_export(handle, v, format, ptr.baseAddress, cols.count, path)
        }
        return rc == 0
    }

    // MARK: Row tagging (persists across filter/sort/group)

    func setTag(dataRow: Int, tagged: Bool) {
        nexus_set_tag(handle, UInt64(dataRow), tagged ? 1 : 0)
    }

    func isTagged(dataRow: Int) -> Bool {
        nexus_is_tagged(handle, UInt64(dataRow)) != 0
    }

    var taggedCount: Int { Int(nexus_tagged_count(handle)) }

    func clearTags() { nexus_clear_tags(handle) }

    func taggedView() -> DataView {
        guard let v = nexus_tagged_view(handle) else { return .empty }
        return DataView(v)
    }

    func intersectTags(view: DataView) -> DataView {
        guard let viewHandle = view.handle,
              let result = nexus_intersect_tags(handle, viewHandle)
        else { return .empty }
        return DataView(result)
    }

    /// All tagged absolute data-row indices (for session save).
    func taggedRows() -> [Int] {
        let view = taggedView()
        guard let handle = view.handle else { return [] }
        let count = Int(nexus_view_count(handle))
        var rows: [Int] = []
        rows.reserveCapacity(count)
        for i in 0..<count {
            let id = nexus_view_row_id(handle, UInt64(i))
            if id >= 0 { rows.append(Int(id)) }
        }
        return rows
    }

    /// Engine version string.
    static var version: String { String(cString: nexus_version()) }

    /// Most recent engine error on the current thread.
    static var lastError: String { String(cString: nexus_last_error()) }
}

/// A typed engine error carrying the message from `nexus_last_error`.
struct EngineError: Error, CustomStringConvertible {
    let message: String
    var description: String { message }
}

/// A multi-level grouping tree (RF-03), addressed by `Int64` items: `>= 0` is a
/// group node id; `< 0` is the data row `(-item - 1)`. Mirrors the engine's
/// encoding so the same `Int64` can be used directly as an `NSOutlineView` item.
final class GroupTree {
    fileprivate let handle: OpaquePointer

    fileprivate init(_ handle: OpaquePointer) {
        self.handle = handle
    }

    deinit {
        nexus_group_free(handle)
    }

    /// Sentinel for an out-of-range child (matches `NEXUS_ITEM_INVALID`).
    static let invalid = Int64.min

    var rootCount: Int { Int(nexus_group_root_count(handle)) }
    func rootChild(_ index: Int) -> Int64 { nexus_group_root_child(handle, UInt64(index)) }
    func childCount(_ item: Int64) -> Int { Int(nexus_group_child_count(handle, item)) }
    func child(_ item: Int64, _ index: Int) -> Int64 { nexus_group_child(handle, item, UInt64(index)) }
    func isGroup(_ item: Int64) -> Bool { nexus_group_is_group(handle, item) != 0 }
    func count(_ item: Int64) -> Int { Int(nexus_group_count(handle, item)) }

    func label(_ item: Int64) -> String {
        guard let c = nexus_group_label(handle, item) else { return "" }
        defer { nexus_string_free(c) }
        return String(cString: c)
    }

    /// Data-row id for a row item, or `nil` for a group.
    func rowId(_ item: Int64) -> Int? {
        let id = nexus_group_row(handle, item)
        return id >= 0 ? Int(id) : nil
    }
}

/// A view (search result) handle with automatic cleanup.
final class DataView {
    /// Opaque `NexusView *`; nil only for the sentinel empty view.
    fileprivate let handle: OpaquePointer?

    fileprivate init(_ handle: OpaquePointer) {
        self.handle = handle
    }

    private init() {
        self.handle = nil
    }

    /// A safe empty placeholder used when a handle could not be created.
    static let empty = DataView()

    deinit {
        if let handle { nexus_view_free(handle) }
    }

    /// Number of rows in the view.
    var count: Int {
        guard let handle else { return 0 }
        return Int(nexus_view_count(handle))
    }
}
