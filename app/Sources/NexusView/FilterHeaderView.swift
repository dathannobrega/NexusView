import AppKit

/// Supplies a per-column context menu for a right-clicked table header.
protocol FilterHeaderViewDelegate: AnyObject {
    /// `tableColumnIndex` is the column's position in the table's column array.
    func headerMenu(forTableColumn tableColumnIndex: Int) -> NSMenu?
}

/// An `NSTableHeaderView` that, on right-click, asks its delegate for a menu
/// scoped to the column under the cursor — enabling per-column sort, filter, and
/// hide actions (Timeline Explorer-style).
final class FilterHeaderView: NSTableHeaderView {
    weak var menuDelegate: FilterHeaderViewDelegate?

    override func menu(for event: NSEvent) -> NSMenu? {
        let point = convert(event.locationInWindow, from: nil)
        let columnIndex = column(at: point)
        guard columnIndex >= 0 else { return super.menu(for: event) }
        return menuDelegate?.headerMenu(forTableColumn: columnIndex) ?? super.menu(for: event)
    }
}
