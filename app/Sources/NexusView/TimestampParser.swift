import Foundation

/// A recognized timestamp encoding for a column.
enum TimeFormat: Hashable {
    case iso8601
    case epochSeconds // Unix / "linux" time
    case epochMillis
}

/// How recognized timestamps are displayed.
enum TimeZoneMode: String {
    case raw   // original text, no conversion
    case utc
    case local
}

/// Detects and converts timestamp values (ISO-8601 and Unix epoch in seconds or
/// milliseconds). Conversion is display-only; the underlying data is never
/// modified, so exports remain faithful to the source.
enum TimestampParser {
    // Plausible epoch window so ordinary integers aren't mistaken for time.
    private static let minEpoch: Double = 946_684_800    // 2000-01-01
    private static let maxEpoch: Double = 4_102_444_800  // 2100-01-01

    private static let isoRegex = try! NSRegularExpression(
        pattern: #"^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}"#
    )

    /// Classify a single value's timestamp format, or `nil` if it isn't one.
    static func detect(_ value: String) -> TimeFormat? {
        let v = value.trimmingCharacters(in: .whitespaces)
        if v.isEmpty { return nil }

        if v.allSatisfy(\.isNumber), let n = Double(v) {
            if v.count >= 9, v.count <= 11, n >= minEpoch, n <= maxEpoch { return .epochSeconds }
            if v.count >= 12, v.count <= 14, n / 1000 >= minEpoch, n / 1000 <= maxEpoch { return .epochMillis }
            return nil
        }

        let range = NSRange(v.startIndex..., in: v)
        return isoRegex.firstMatch(in: v, range: range) != nil ? .iso8601 : nil
    }

    /// Parse a value to a `Date` given its known format.
    static func parse(_ value: String, format: TimeFormat) -> Date? {
        let v = value.trimmingCharacters(in: .whitespaces)
        switch format {
        case .epochSeconds: return Double(v).map { Date(timeIntervalSince1970: $0) }
        case .epochMillis:  return Double(v).map { Date(timeIntervalSince1970: $0 / 1000) }
        case .iso8601:      return parseISO(v)
        }
    }

    /// Display string for a value in the given mode, or `nil` to keep the raw text.
    static func convert(_ value: String, format: TimeFormat, mode: TimeZoneMode) -> String? {
        guard mode != .raw, let date = parse(value, format: format) else { return nil }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        if mode == .utc {
            formatter.timeZone = TimeZone(identifier: "UTC")
            return formatter.string(from: date) + " UTC"
        }
        formatter.timeZone = .current
        return formatter.string(from: date) + " " + (TimeZone.current.abbreviation() ?? "local")
    }

    private static func parseISO(_ value: String) -> Date? {
        let normalized = value.replacingOccurrences(of: " ", with: "T")
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = iso.date(from: normalized) { return date }
        iso.formatOptions = [.withInternetDateTime]
        if let date = iso.date(from: normalized) { return date }

        // No timezone present → assume UTC.
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "UTC")
        for pattern in ["yyyy-MM-dd'T'HH:mm:ss.SSS", "yyyy-MM-dd'T'HH:mm:ss"] {
            formatter.dateFormat = pattern
            if let date = formatter.date(from: normalized) { return date }
        }
        return nil
    }
}
