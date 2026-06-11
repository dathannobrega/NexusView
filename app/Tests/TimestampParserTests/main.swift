// Test harness for TimestampParser — compiled together with the source file by
// scripts/test_app.sh. The project builds with Command Line Tools only (no
// Xcode.app), which ships no XCTest, so app-side unit tests are plain
// executables that exit non-zero on failure.
import Foundation

var failures = 0
var passes = 0

func expect<T: Equatable>(_ actual: T, _ expected: T, _ label: String, line: Int = #line) {
    if actual == expected {
        passes += 1
    } else {
        failures += 1
        print("FAIL (line \(line)): \(label)")
        print("  expected: \(expected)")
        print("  actual:   \(actual)")
    }
}

/// Convert through the parser and render in UTC, or "nil" when unparseable.
func utc(_ value: String, _ format: TimeFormat = .iso8601) -> String {
    TimestampParser.convert(value, format: format, mode: .utc) ?? "nil"
}

// MARK: detect — classification

expect(TimestampParser.detect("2026-06-10T14:48:57ZTUTC"), .iso8601, "detect: ZTUTC suffix is ISO")
expect(TimestampParser.detect("2026-06-10 14:48:57"), .iso8601, "detect: space-separated ISO")
expect(TimestampParser.detect("1717777777"), .epochSeconds, "detect: epoch seconds")
expect(TimestampParser.detect("1717777777123"), .epochMillis, "detect: epoch millis")
expect(TimestampParser.detect("svchost.exe"), nil, "detect: plain text is not a timestamp")
expect(TimestampParser.detect("12345"), nil, "detect: short integer is not an epoch")

// MARK: the Sophos datalake `ZTUTC` family (the regression being fixed)

expect(utc("2026-06-10T14:48:57ZTUTC"), "2026-06-10 14:48:57 UTC", "parse: Z + TUTC annotation")
expect(utc("2026-06-11T02:38:03ZTUTC"), "2026-06-11 02:38:03 UTC", "parse: Z + TUTC annotation (2)")
expect(utc("2026-06-10T14:48:57.250ZTUTC"), "2026-06-10 14:48:57 UTC", "parse: fractional + ZTUTC")
expect(utc("2026-06-10T14:48:57Ztutc"), "2026-06-10 14:48:57 UTC", "parse: annotation is case-insensitive")
expect(utc("2026-06-10T14:48:57TUTC"), "2026-06-10 14:48:57 UTC", "parse: TUTC without Z (assume UTC)")
expect(utc("2026-06-10T14:48:57 UTC"), "2026-06-10 14:48:57 UTC", "parse: space + UTC annotation")
expect(utc("2026-06-10T14:48:57GMT"), "2026-06-10 14:48:57 UTC", "parse: GMT annotation")
expect(utc("2026-06-10T14:48:57Z UTC"), "2026-06-10 14:48:57 UTC", "parse: stacked Z + UTC annotations")

// MARK: well-formed inputs must be untouched by the annotation stripper

expect(utc("2026-06-10T14:48:57Z"), "2026-06-10 14:48:57 UTC", "parse: plain Z unchanged")
expect(utc("2026-06-10T14:48:57+02:00"), "2026-06-10 12:48:57 UTC", "parse: numeric offset honored")
expect(utc("2026-06-10T14:48:57"), "2026-06-10 14:48:57 UTC", "parse: no timezone assumes UTC")
expect(utc("2026-06-10 14:48:57"), "2026-06-10 14:48:57 UTC", "parse: space separator")

// MARK: malformed inputs must stay raw (convert → nil), never mis-parse

expect(utc("2026-06-10T14:48:57XYZ"), "nil", "parse: unknown suffix is rejected")
expect(utc("2026-06-10T14:48:57UTCX"), "nil", "parse: non-trailing annotation is rejected")
expect(utc("2026-06-10T14:48:57 EST"), "nil", "parse: named zones must not be mis-assumed as UTC")
expect(utc("not a date"), "nil", "parse: garbage is rejected")

// MARK: epoch paths and raw mode are unaffected

expect(utc("946684800", .epochSeconds), "2000-01-01 00:00:00 UTC", "parse: epoch seconds")
expect(utc("946684800000", .epochMillis), "2000-01-01 00:00:00 UTC", "parse: epoch millis")
expect(
    TimestampParser.convert("2026-06-10T14:48:57ZTUTC", format: .iso8601, mode: .raw),
    nil,
    "convert: raw mode keeps original text"
)

print("\(passes + failures) tests, \(passes) passed, \(failures) failed")
exit(failures == 0 ? 0 : 1)
