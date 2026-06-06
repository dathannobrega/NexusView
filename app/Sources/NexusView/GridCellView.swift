import AppKit

/// A lightweight, directly-drawn table cell.
///
/// `NSTextField` is a heavy control (its own cell, autolayout, target/action,
/// accessibility, formatter machinery); reconfiguring and redrawing hundreds of
/// them per frame is what makes a data grid stutter during fast scrolling. This
/// view holds three plain values and draws the string itself in `draw(_:)`, so
/// configuring a cell is just three property assignments + `needsDisplay`.
final class GridCellView: NSView {
    static let font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
    private static let selectionColor = NSColor.controlAccentColor.withAlphaComponent(0.30)
    private static let paragraph: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return style
    }()

    private var text = ""
    private var color: NSColor = .labelColor
    private var selected = false

    override var isFlipped: Bool { true }

    func configure(_ text: String, color: NSColor, selected: Bool) {
        self.text = text
        self.color = color
        self.selected = selected
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        if selected {
            Self.selectionColor.setFill()
            bounds.fill()
        }
        if text.isEmpty { return }

        let inset: CGFloat = 5
        let lineHeight = Self.font.ascender - Self.font.descender
        let y = ((bounds.height - lineHeight) / 2).rounded(.down)
        let rect = NSRect(x: inset, y: y, width: max(0, bounds.width - inset * 2), height: lineHeight + 1)
        (text as NSString).draw(in: rect, withAttributes: [
            .font: Self.font,
            .foregroundColor: color,
            .paragraphStyle: Self.paragraph,
        ])
    }
}
