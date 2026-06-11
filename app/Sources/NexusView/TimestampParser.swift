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
    /// Called per visible timestamp cell while rendering, so the formatters are
    /// cached (immutable after init — safe to share; `DateFormatter` is
    /// thread-safe on macOS 10.9+).
    static func convert(_ value: String, format: TimeFormat, mode: TimeZoneMode) -> String? {
        guard mode != .raw, let date = parse(value, format: format) else { return nil }
        if mode == .utc {
            return utcDisplayFormatter.string(from: date) + " UTC"
        }
        // The local formatter is rebuilt per call so a system timezone change
        // mid-session is always honored.
        let formatter = makeDisplayFormatter(timeZone: .current)
        return formatter.string(from: date) + " " + (TimeZone.current.abbreviation() ?? "local")
    }

    private static let utcDisplayFormatter = makeDisplayFormatter(
        timeZone: TimeZone(identifier: "UTC")
    )

    private static func makeDisplayFormatter(timeZone: TimeZone?) -> DateFormatter {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        formatter.timeZone = timeZone
        return formatter
    }

    /// Matches one trailing timezone-name annotation: an optional `T` or space
    /// separator followed by `UTC` or `GMT`. Anchored to the end so nothing in
    /// the middle of a value can ever match.
    private static let trailingTimezoneAnnotation = try! NSRegularExpression(
        pattern: #"[ T]?(?:UTC|GMT)$"#,
        options: [.caseInsensitive]
    )

    /// Strip timezone *annotations* some exporters glue after an already
    /// complete time, e.g. Sophos datalake's `2026-06-10T14:48:57ZTUTC`, or
    /// `… 14:48:57 UTC` / `…57TUTC` / `…57GMT`. Applied repeatedly so stacked
    /// annotations (`…57Z UTC`) also reduce. A bare `Z` and numeric offsets
    /// (`+02:00`) are kept — the ISO parser consumes those.
    private static func stripTimezoneAnnotations(_ value: String) -> String {
        var v = value
        while true {
            let range = NSRange(v.startIndex..., in: v)
            guard let match = trailingTimezoneAnnotation.firstMatch(in: v, range: range),
                  let found = Range(match.range, in: v), !found.isEmpty
            else { return v }
            v.removeSubrange(found)
        }
    }

    // Parsing formatters, cached for the render path (immutable after init).
    private static let isoFractional: ISO8601DateFormatter = {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return iso
    }()
    private static let isoBasic: ISO8601DateFormatter = {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime]
        return iso
    }()
    private static let utcNoZoneFormatters: [DateFormatter] = ["yyyy-MM-dd'T'HH:mm:ss.SSS", "yyyy-MM-dd'T'HH:mm:ss"].map { pattern in
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "UTC")
        formatter.dateFormat = pattern
        return formatter
    }

    /// Full anchored shape of an ISO-8601 date-time after annotation stripping:
    /// date, `T`, time, optional fractional seconds, optional `Z` or numeric
    /// offset. Foundation's ISO parsers are lenient about trailing junk on some
    /// macOS versions (`…57UTCX` parses as UTC); anchoring here keeps the
    /// accepted grammar strict and identical across versions — anything else
    /// stays raw rather than being guessed at.
    private static let isoShape = try! NSRegularExpression(
        pattern: #"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}(?::?\d{2})?)?$"#
    )

    private static func parseISO(_ value: String) -> Date? {
        let cleaned = stripTimezoneAnnotations(value)
        let normalized = cleaned.replacingOccurrences(of: " ", with: "T")
        let range = NSRange(normalized.startIndex..., in: normalized)
        guard isoShape.firstMatch(in: normalized, range: range) != nil else { return nil }

        if let date = isoFractional.date(from: normalized) { return date }
        if let date = isoBasic.date(from: normalized) { return date }

        // Shape matched but carried no timezone → assume UTC.
        for formatter in utcNoZoneFormatters {
            if let date = formatter.date(from: normalized) { return date }
        }
        return nil
    }
}
