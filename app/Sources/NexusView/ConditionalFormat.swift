import AppKit

/// A single tactical coloring rule (RF-06): if a cell contains `needle`
/// (case-insensitive), render it in `color`.
struct FormatRule {
    let needle: String
    let color: NSColor
}

/// Viewport-level conditional formatting. Rules are evaluated only for the
/// cells currently being drawn, so the cost scales with what's on screen, not
/// with the file size (RNF-01).
struct ConditionalFormat {
    let rules: [FormatRule]

    /// First matching rule's color, or `nil` to use the default text color.
    /// Uses a case-insensitive search that does **not** allocate a lowercased
    /// copy of the cell — important on the per-cell scroll render path.
    func color(for text: String) -> NSColor? {
        if text.isEmpty { return nil }
        for rule in rules where text.range(of: rule.needle, options: .caseInsensitive) != nil {
            return rule.color
        }
        return nil
    }

    /// A sensible starter ruleset for incident triage.
    static let `default` = ConditionalFormat(rules: [
        FormatRule(needle: "critical", color: .systemRed),
        FormatRule(needle: "error", color: .systemRed),
        FormatRule(needle: "denied", color: .systemRed),
        FormatRule(needle: "fail", color: .systemOrange),
        FormatRule(needle: "warn", color: .systemYellow),
        FormatRule(needle: "success", color: .systemGreen),
        FormatRule(needle: "allow", color: .systemGreen),
    ])
}
