import Foundation

/// A serialized per-tab session: the file plus all UI state needed to restore a
/// triage view exactly (filters, sort, grouping, hidden columns, tags). Written
/// as JSON so sessions are human-readable and diff-friendly.
struct SessionState: Codable {
    var filePath: String
    var search: String
    var hiddenColumns: [Int]
    var columnFilters: [ColumnFilterState]
    var sort: [SortKeyState]
    var groupColumns: [Int]
    var taggedRows: [Int]
    var taggedOnly: Bool

    static let fileExtension = "nexussession"

    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(self)
    }

    static func decoded(from data: Data) throws -> SessionState {
        try JSONDecoder().decode(SessionState.self, from: data)
    }
}

struct ColumnFilterState: Codable {
    var col: Int
    var value: String
}

struct SortKeyState: Codable {
    var col: Int
    var ascending: Bool
}
