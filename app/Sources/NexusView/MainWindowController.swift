import AppKit

/// A selected cell: a view-row position and a data-column index.
struct CellRef: Hashable {
    let row: Int
    let col: Int
}

/// The main window: a virtualized data grid (flat `NSTableView`) that switches to
/// a grouped `NSOutlineView` when columns are grouped. Adds Timeline Explorer-
/// style triage features: a persistent **Tag** column, a **Line #** column,
/// per-column **sort/filter/hide** via the header menu, column visibility, and a
/// **Tagged only** view. Only on-screen cells are ever materialized (RNF-01).
final class MainWindowController: NSWindowController,
    NSTableViewDataSource, NSTableViewDelegate,
    NSOutlineViewDataSource, NSOutlineViewDelegate,
    NSSearchFieldDelegate, FilterHeaderViewDelegate, NSMenuDelegate {

    // Special leading columns of the flat table.
    private static let tagColumnID = NSUserInterfaceItemIdentifier("tag")
    private static let lineColumnID = NSUserInterfaceItemIdentifier("ln")

    // MARK: UI
    private let scrollView = NSScrollView()
    private let tableView = NSTableView()
    private let outlineScroll = NSScrollView()
    private let outlineView = NSOutlineView()
    private let searchField = NSSearchField()
    private let groupButton = NSPopUpButton(frame: .zero, pullsDown: true)
    private let viewButton = NSPopUpButton(frame: .zero, pullsDown: true)
    private let statusLabel = NSTextField(labelWithString: "Open a file to begin  (⌘O)")
    private let loadSpinner = NSProgressIndicator()
    private let gridContainer = NSView()
    private let detailScroll = NSScrollView()
    private let detailTextView = NSTextView()
    private var showDetailPanel = true

    // MARK: State
    private var engine: Engine?
    /// Result of the current text/column filters, with "tagged only" applied.
    private var baseView: DataView?
    /// `baseView` with the active sort applied — what the flat table renders.
    private var currentView: DataView?
    private var groupColumns: [Int] = []
    private var groupTree: GroupTree?
    private var groupGeneration = 0
    /// Drops a stale async open if another file is opened first.
    private var loadGeneration = 0
    private var searchDebounce: DispatchWorkItem?

    /// Hidden data-column indices (excluded from the grid and from export).
    private var hiddenColumns: Set<Int> = []
    /// Per-column filter text (data-column index → value), AND-combined.
    private var columnFilters: [Int: String] = [:]
    /// When on, the view is restricted to tagged rows.
    private var taggedOnly = false
    /// Selected cells (view row + data column) for cell-level copy (②).
    private var cellSelection: Set<CellRef> = []
    private var cellAnchor: CellRef?
    /// Render-path cache: view row → its column values (one FFI call per row,
    /// not per cell). Cleared whenever the view's row content changes.
    private var rowCache: [Int: [String]] = [:]
    /// The opened file's URL (nil = empty tab). Drives the title and sessions.
    private(set) var fileURL: URL?
    var hasFile: Bool { fileURL != nil }
    /// Columns auto-detected as timestamps → their encoding.
    private var timestampColumns: [Int: TimeFormat] = [:]
    private var timeMode: TimeZoneMode = .raw

    private let format = ConditionalFormat.default
    private static let cellFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
    /// Reused across cells — creating an `NumberFormatter` per call is expensive.
    private static let decimalFormatter: NumberFormatter = {
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        return formatter
    }()

    // MARK: Init

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1200, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "NexusView"
        window.tabbingMode = .preferred
        window.tabbingIdentifier = "NexusView"
        window.center()
        super.init(window: window)
        buildLayout()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not supported") }

    // MARK: Layout

    private func buildLayout() {
        guard let content = window?.contentView else { return }

        let header = NSView()
        header.translatesAutoresizingMaskIntoConstraints = false

        searchField.translatesAutoresizingMaskIntoConstraints = false
        searchField.placeholderString = "Search  —  error AND NOT timeout   host:web01   /regex/"
        searchField.delegate = self
        searchField.sendsSearchStringImmediately = false
        searchField.sendsWholeSearchString = false

        groupButton.translatesAutoresizingMaskIntoConstraints = false
        groupButton.bezelStyle = .rounded
        viewButton.translatesAutoresizingMaskIntoConstraints = false
        viewButton.bezelStyle = .rounded
        rebuildGroupMenu()
        rebuildViewMenu()

        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.font = .systemFont(ofSize: 11)
        statusLabel.alignment = .right
        statusLabel.lineBreakMode = .byTruncatingHead

        loadSpinner.translatesAutoresizingMaskIntoConstraints = false
        loadSpinner.style = .spinning
        loadSpinner.controlSize = .small
        loadSpinner.isDisplayedWhenStopped = false

        header.addSubview(searchField)
        header.addSubview(groupButton)
        header.addSubview(viewButton)
        header.addSubview(loadSpinner)
        header.addSubview(statusLabel)

        configureTable()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = false
        scrollView.borderType = .noBorder

        configureOutline()
        outlineScroll.translatesAutoresizingMaskIntoConstraints = false
        outlineScroll.documentView = outlineView
        outlineScroll.hasVerticalScroller = true
        outlineScroll.hasHorizontalScroller = true
        outlineScroll.autohidesScrollers = false
        outlineScroll.borderType = .noBorder
        outlineScroll.isHidden = true

        configureDetail()
        gridContainer.translatesAutoresizingMaskIntoConstraints = false
        gridContainer.addSubview(scrollView)
        gridContainer.addSubview(outlineScroll)
        gridContainer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)

        // Grid on top, detail panel below, in a vertical stack so the detail can
        // collapse (View ▸ Show Detail Panel) and the grid takes the rest.
        let vstack = NSStackView(views: [gridContainer, detailScroll])
        vstack.orientation = .vertical
        vstack.distribution = .fill
        vstack.spacing = 0
        vstack.translatesAutoresizingMaskIntoConstraints = false

        content.addSubview(header)
        content.addSubview(vstack)

        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: content.topAnchor),
            header.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            header.heightAnchor.constraint(equalToConstant: 44),

            searchField.leadingAnchor.constraint(equalTo: header.leadingAnchor, constant: 12),
            searchField.centerYAnchor.constraint(equalTo: header.centerYAnchor),
            searchField.widthAnchor.constraint(greaterThanOrEqualToConstant: 300),

            groupButton.leadingAnchor.constraint(equalTo: searchField.trailingAnchor, constant: 10),
            groupButton.centerYAnchor.constraint(equalTo: header.centerYAnchor),
            groupButton.widthAnchor.constraint(equalToConstant: 140),

            viewButton.leadingAnchor.constraint(equalTo: groupButton.trailingAnchor, constant: 8),
            viewButton.centerYAnchor.constraint(equalTo: header.centerYAnchor),
            viewButton.widthAnchor.constraint(equalToConstant: 88),

            statusLabel.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: -14),
            statusLabel.centerYAnchor.constraint(equalTo: header.centerYAnchor),
            statusLabel.leadingAnchor.constraint(greaterThanOrEqualTo: loadSpinner.trailingAnchor, constant: 6),

            loadSpinner.trailingAnchor.constraint(equalTo: statusLabel.leadingAnchor, constant: -6),
            loadSpinner.centerYAnchor.constraint(equalTo: header.centerYAnchor),
            loadSpinner.widthAnchor.constraint(equalToConstant: 16),
            loadSpinner.heightAnchor.constraint(equalToConstant: 16),
            loadSpinner.leadingAnchor.constraint(greaterThanOrEqualTo: viewButton.trailingAnchor, constant: 8),

            vstack.topAnchor.constraint(equalTo: header.bottomAnchor),
            vstack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            vstack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            vstack.bottomAnchor.constraint(equalTo: content.bottomAnchor),

            detailScroll.heightAnchor.constraint(equalToConstant: 150),
        ])

        for grid in [scrollView, outlineScroll] {
            NSLayoutConstraint.activate([
                grid.topAnchor.constraint(equalTo: gridContainer.topAnchor),
                grid.leadingAnchor.constraint(equalTo: gridContainer.leadingAnchor),
                grid.trailingAnchor.constraint(equalTo: gridContainer.trailingAnchor),
                grid.bottomAnchor.constraint(equalTo: gridContainer.bottomAnchor),
            ])
        }
    }

    /// The bottom detail panel: a read-only text view showing the selected row's
    /// full field values (great for long EVTX messages).
    private func configureDetail() {
        detailScroll.translatesAutoresizingMaskIntoConstraints = false
        detailScroll.hasVerticalScroller = true
        detailScroll.borderType = .noBorder

        detailTextView.minSize = NSSize(width: 0, height: 0)
        detailTextView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        detailTextView.isEditable = false
        detailTextView.isSelectable = true
        detailTextView.isVerticallyResizable = true
        detailTextView.isHorizontallyResizable = false
        detailTextView.autoresizingMask = [.width]
        detailTextView.textContainer?.widthTracksTextView = true
        detailTextView.font = Self.cellFont
        detailTextView.textContainerInset = NSSize(width: 8, height: 6)
        detailTextView.string = "Select a row to see its fields here."
        detailScroll.documentView = detailTextView
    }

    private func configureTable() {
        tableView.dataSource = self
        tableView.delegate = self
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.usesAutomaticRowHeights = false
        tableView.rowHeight = 18
        tableView.intercellSpacing = NSSize(width: 8, height: 0)
        tableView.allowsMultipleSelection = true
        tableView.columnAutoresizingStyle = .noColumnAutoresizing
        tableView.gridStyleMask = [.solidVerticalGridLineMask]
        tableView.style = .plain
        tableView.target = self
        tableView.action = #selector(handleCellClick(_:))
        let contextMenu = NSMenu()
        contextMenu.delegate = self
        tableView.menu = contextMenu
        let headerView = FilterHeaderView()
        headerView.menuDelegate = self
        tableView.headerView = headerView
    }

    private func configureOutline() {
        outlineView.dataSource = self
        outlineView.delegate = self
        outlineView.usesAlternatingRowBackgroundColors = true
        outlineView.rowHeight = 18
        outlineView.intercellSpacing = NSSize(width: 8, height: 0)
        outlineView.allowsMultipleSelection = true
        outlineView.columnAutoresizingStyle = .noColumnAutoresizing
        outlineView.indentationPerLevel = 14
        outlineView.style = .plain
    }

    // MARK: File loading

    func load(url: URL, session: SessionState? = nil) {
        // Show the window immediately and build the index OFF the main thread, so
        // opening a multi-GB file never freezes the UI (RNF-01). A spinner +
        // "Loading…" status reassure the user while the engine indexes.
        fileURL = url
        window?.title = "NexusView — \(url.lastPathComponent)"
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.stringValue = "Loading \(url.lastPathComponent)…"
        loadSpinner.startAnimation(nil)

        // Tear down any current dataset while the new one loads.
        engine = nil
        baseView = nil
        currentView = nil
        groupTree = nil
        cellSelection = []
        cellAnchor = nil
        rowCache.removeAll(keepingCapacity: true)
        timestampColumns = [:]
        for column in tableView.tableColumns { tableView.removeTableColumn(column) }
        tableView.reloadData()
        detailTextView.string = ""

        loadGeneration += 1
        let generation = loadGeneration
        let path = url.path
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let engine = Engine(path: path) // heavy: mmap + line-offset index build
            let errorMessage = engine == nil ? Engine.lastError : ""
            DispatchQueue.main.async {
                guard let self = self, generation == self.loadGeneration else { return }
                self.loadSpinner.stopAnimation(nil)
                guard let engine = engine else {
                    self.statusLabel.stringValue = "Open a file to begin  (⌘O)"
                    self.presentError(errorMessage)
                    return
                }
                self.finishLoad(engine: engine, session: session)
            }
        }
    }

    /// Wire up the UI once the engine has finished indexing (runs on the main
    /// thread; all of this is O(visible) or O(columns), never O(rows)).
    private func finishLoad(engine: Engine, session: SessionState?) {
        self.engine = engine
        hiddenColumns = []
        columnFilters = [:]
        taggedOnly = false
        groupColumns = []
        searchField.stringValue = ""
        tableView.sortDescriptors = []
        baseView = engine.allRows()
        currentView = baseView
        rebuildColumns(for: engine)
        detectTimestampColumns()
        rebuildGroupMenu()
        rebuildViewMenu()
        if let session = session {
            applySession(session)
        } else {
            refresh()
        }
        tableView.scrollRowToVisible(0)
        updateDetail()
    }

    // MARK: Session save / load

    /// Capture the current tab's state for serialization (nil if no file open).
    func sessionState() -> SessionState? {
        guard let engine = engine, let url = fileURL else { return nil }
        let sort: [SortKeyState] = tableView.sortDescriptors.compactMap { descriptor in
            guard let key = descriptor.key, let col = dataColumnIndex(NSUserInterfaceItemIdentifier(key)) else { return nil }
            return SortKeyState(col: col, ascending: descriptor.ascending)
        }
        return SessionState(
            filePath: url.path,
            search: searchField.stringValue,
            hiddenColumns: hiddenColumns.sorted(),
            columnFilters: columnFilters.map { ColumnFilterState(col: $0.key, value: $0.value) },
            sort: sort,
            groupColumns: groupColumns,
            taggedRows: engine.taggedRows(),
            taggedOnly: taggedOnly
        )
    }

    private func applySession(_ state: SessionState) {
        guard let engine = engine else { return }
        searchField.stringValue = state.search
        hiddenColumns = Set(state.hiddenColumns)
        for col in 0..<engine.columnCount {
            let id = NSUserInterfaceItemIdentifier("c\(col)")
            let hidden = hiddenColumns.contains(col)
            tableView.tableColumns.first { $0.identifier == id }?.isHidden = hidden
            outlineView.tableColumns.first { $0.identifier == id }?.isHidden = hidden
        }
        columnFilters = Dictionary(state.columnFilters.map { ($0.col, $0.value) }, uniquingKeysWith: { a, _ in a })
        tableView.sortDescriptors = state.sort.map { NSSortDescriptor(key: "c\($0.col)", ascending: $0.ascending) }
        groupColumns = state.groupColumns
        taggedOnly = state.taggedOnly
        engine.clearTags()
        for row in state.taggedRows { engine.setTag(dataRow: row, tagged: true) }
        rebuildGroupMenu()
        rebuildViewMenu()
        reapply()
    }

    // MARK: Columns

    private func rebuildColumns(for engine: Engine) {
        // Flat table: Tag + Line# + data columns.
        for column in tableView.tableColumns {
            tableView.removeTableColumn(column)
        }
        tableView.addTableColumn(makeTagColumn())
        tableView.addTableColumn(makeLineColumn())
        for index in 0..<engine.columnCount {
            tableView.addTableColumn(makeDataColumn(engine, index: index, sortable: true))
        }

        // Grouped outline: data columns only (group labels render in the first).
        for column in outlineView.tableColumns {
            outlineView.removeTableColumn(column)
        }
        for index in 0..<engine.columnCount {
            outlineView.addTableColumn(makeDataColumn(engine, index: index, sortable: false))
        }
        outlineView.outlineTableColumn = outlineView.tableColumns.first
    }

    private func makeTagColumn() -> NSTableColumn {
        let column = NSTableColumn(identifier: Self.tagColumnID)
        column.title = "✓"
        column.width = 26
        column.minWidth = 26
        column.maxWidth = 26
        return column
    }

    private func makeLineColumn() -> NSTableColumn {
        let column = NSTableColumn(identifier: Self.lineColumnID)
        column.title = "#"
        column.width = 64
        column.minWidth = 40
        column.maxWidth = 140
        return column
    }

    private func makeDataColumn(_ engine: Engine, index: Int, sortable: Bool) -> NSTableColumn {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("c\(index)"))
        column.title = engine.columnName(index)
        column.width = 160
        column.minWidth = 44
        column.maxWidth = 1400
        column.isHidden = hiddenColumns.contains(index)
        if sortable {
            column.sortDescriptorPrototype = NSSortDescriptor(key: "c\(index)", ascending: true)
        }
        return column
    }

    /// Visible data columns in display order — used for export (RF-10 + hide).
    private func visibleDataColumns() -> [Int] {
        tableView.tableColumns.compactMap { column in
            guard !column.isHidden, let col = dataColumnIndex(column.identifier) else { return nil }
            return col
        }
    }

    private func setColumnHidden(_ col: Int, _ hidden: Bool) {
        if hidden { hiddenColumns.insert(col) } else { hiddenColumns.remove(col) }
        let id = NSUserInterfaceItemIdentifier("c\(col)")
        tableView.tableColumns.first { $0.identifier == id }?.isHidden = hidden
        outlineView.tableColumns.first { $0.identifier == id }?.isHidden = hidden
        rebuildViewMenu()
        updateStatus()
    }

    // MARK: Grouping menu (RF-03)

    private func rebuildGroupMenu() {
        let menu = NSMenu()
        menu.addItem(withTitle: groupTitle(), action: nil, keyEquivalent: "")
        if let engine = engine {
            for index in 0..<engine.columnCount {
                let item = NSMenuItem(title: engine.columnName(index),
                                      action: #selector(toggleGroupColumn(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = index
                item.state = groupColumns.contains(index) ? .on : .off
                menu.addItem(item)
            }
            menu.addItem(.separator())
            let clear = NSMenuItem(title: "Clear grouping", action: #selector(clearGrouping(_:)), keyEquivalent: "")
            clear.target = self
            menu.addItem(clear)
        }
        groupButton.menu = menu
    }

    private func groupTitle() -> String {
        guard !groupColumns.isEmpty, let engine = engine else { return "Group by ▾" }
        return "Group: " + groupColumns.map { engine.columnName($0) }.joined(separator: " › ") + " ▾"
    }

    @objc private func toggleGroupColumn(_ sender: NSMenuItem) {
        guard let index = sender.representedObject as? Int else { return }
        if let position = groupColumns.firstIndex(of: index) {
            groupColumns.remove(at: position)
        } else {
            groupColumns.append(index)
        }
        rebuildGroupMenu()
        refresh()
    }

    @objc private func clearGrouping(_ sender: Any?) {
        groupColumns.removeAll()
        rebuildGroupMenu()
        refresh()
    }

    // MARK: View menu (columns visibility + tags)

    private func rebuildViewMenu() {
        let menu = NSMenu()
        menu.addItem(withTitle: "View ▾", action: nil, keyEquivalent: "")
        if let engine = engine {
            let columnsHeader = NSMenuItem(title: "Columns", action: nil, keyEquivalent: "")
            columnsHeader.isEnabled = false
            menu.addItem(columnsHeader)
            for index in 0..<engine.columnCount {
                let item = NSMenuItem(title: engine.columnName(index),
                                      action: #selector(toggleColumnVisibility(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = index
                item.state = hiddenColumns.contains(index) ? .off : .on
                menu.addItem(item)
            }
            menu.addItem(withTitle: "Show All Columns", action: #selector(showAllColumns(_:)), keyEquivalent: "")
                .target = self
            menu.addItem(.separator())
            let tagItem = NSMenuItem(title: "Tagged only", action: #selector(toggleTaggedOnly(_:)), keyEquivalent: "")
            tagItem.target = self
            tagItem.state = taggedOnly ? .on : .off
            menu.addItem(tagItem)
            let clearTags = NSMenuItem(title: "Clear All Tags", action: #selector(clearAllTags(_:)), keyEquivalent: "")
            clearTags.target = self
            menu.addItem(clearTags)
            menu.addItem(.separator())
            let detailItem = NSMenuItem(title: "Show Detail Panel", action: #selector(toggleDetailPanel(_:)), keyEquivalent: "")
            detailItem.target = self
            detailItem.state = showDetailPanel ? .on : .off
            menu.addItem(detailItem)

            if !timestampColumns.isEmpty {
                menu.addItem(.separator())
                let tsHeader = NSMenuItem(title: "Timestamps (\(timestampColumns.count) col\(timestampColumns.count == 1 ? "" : "s"))", action: nil, keyEquivalent: "")
                tsHeader.isEnabled = false
                menu.addItem(tsHeader)
                let modes: [(String, TimeZoneMode)] = [("Original", .raw), ("Convert to UTC", .utc), ("Convert to Local", .local)]
                for (title, mode) in modes {
                    let item = NSMenuItem(title: title, action: #selector(setTimeMode(_:)), keyEquivalent: "")
                    item.target = self
                    item.representedObject = mode.rawValue
                    item.state = (timeMode == mode) ? .on : .off
                    menu.addItem(item)
                }
            }
        }
        viewButton.menu = menu
    }

    @objc private func toggleDetailPanel(_ sender: Any?) {
        showDetailPanel.toggle()
        detailScroll.isHidden = !showDetailPanel
        rebuildViewMenu()
        if showDetailPanel { updateDetail() }
    }

    // MARK: Timestamp recognition & timezone conversion

    /// Sample the head of each column and flag those that are mostly timestamps
    /// (ISO-8601 or Unix epoch in seconds/milliseconds).
    private func detectTimestampColumns() {
        timestampColumns = [:]
        timeMode = .raw
        guard let engine = engine else { return }
        let sampleRows = min(engine.rowCount, 100)
        guard sampleRows > 0 else { return }
        for col in 0..<engine.columnCount {
            var counts: [TimeFormat: Int] = [:]
            var nonEmpty = 0
            for row in 0..<sampleRows {
                let value = engine.cell(dataRow: row, col: col)
                if value.isEmpty { continue }
                nonEmpty += 1
                if let fmt = TimestampParser.detect(value) { counts[fmt, default: 0] += 1 }
            }
            if nonEmpty >= 3, let best = counts.max(by: { $0.value < $1.value }),
               Double(best.value) / Double(nonEmpty) >= 0.7 {
                timestampColumns[col] = best.key
            }
        }
    }

    @objc private func setTimeMode(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let mode = TimeZoneMode(rawValue: raw) else { return }
        timeMode = mode
        rebuildViewMenu()
        tableView.reloadData()
        updateDetail()
    }

    /// Apply timezone conversion to a cell value if its column is a timestamp.
    private func displayValue(_ value: String, col: Int) -> String {
        if let fmt = timestampColumns[col], let converted = TimestampParser.convert(value, format: fmt, mode: timeMode) {
            return converted
        }
        return value
    }

    /// Single-line rendition of a cell value for the grid rows: embedded line
    /// breaks (legal inside quoted CSV fields, RFC 4180) are shown as ⏎ so the
    /// whole value stays visible and recognizably multi-line. Copies, exports,
    /// and the detail panel keep the real newlines. `\r\n` is a single Swift
    /// `Character`, so the cheap guard scans unicode scalars.
    private func singleLine(_ value: String) -> String {
        guard value.unicodeScalars.contains(where: { $0 == "\n" || $0 == "\r" }) else { return value }
        return value
            .replacingOccurrences(of: "\r\n", with: " ⏎ ")
            .replacingOccurrences(of: "\n", with: " ⏎ ")
            .replacingOccurrences(of: "\r", with: " ⏎ ")
    }

    // MARK: Detail panel (selected-row fields)

    func tableViewSelectionDidChange(_ notification: Notification) { updateDetail() }
    func outlineViewSelectionDidChange(_ notification: Notification) { updateDetail() }

    private func updateDetail() {
        guard showDetailPanel else { return }
        guard let engine = engine, let dataRow = firstSelectedDataRow() else {
            detailTextView.string = "Select a row to see its fields here."
            return
        }
        let count = engine.columnCount
        let nameWidth = (0..<count).map { engine.columnName($0).count }.max() ?? 0
        var lines: [String] = []
        for col in 0..<count {
            let name = engine.columnName(col).padding(toLength: max(nameWidth, 1), withPad: " ", startingAt: 0)
            lines.append("\(name)   \(displayValue(engine.cell(dataRow: dataRow, col: col), col: col))")
        }
        detailTextView.string = lines.joined(separator: "\n")
    }

    private func firstSelectedDataRow() -> Int? {
        guard let engine = engine else { return nil }
        if groupColumns.isEmpty {
            guard let view = currentView, tableView.selectedRow >= 0 else { return nil }
            return engine.dataRowId(view: view, viewRow: tableView.selectedRow)
        }
        guard let tree = groupTree, outlineView.selectedRow >= 0,
              let item = outlineView.item(atRow: outlineView.selectedRow) as? NSNumber else { return nil }
        return tree.rowId(item.int64Value)
    }

    @objc private func toggleColumnVisibility(_ sender: NSMenuItem) {
        guard let col = sender.representedObject as? Int else { return }
        setColumnHidden(col, !hiddenColumns.contains(col))
    }

    @objc private func showAllColumns(_ sender: Any?) {
        for col in hiddenColumns { setColumnHidden(col, false) }
        hiddenColumns.removeAll()
        rebuildViewMenu()
    }

    @objc private func toggleTaggedOnly(_ sender: Any?) {
        taggedOnly.toggle()
        rebuildViewMenu()
        reapply()
    }

    @objc private func clearAllTags(_ sender: Any?) {
        engine?.clearTags()
        tableView.reloadData()
        if taggedOnly { reapply() } else { updateStatus() }
    }

    // MARK: Filtering (global search + per-column filters)

    func controlTextDidChange(_ obj: Notification) {
        guard obj.object as? NSSearchField === searchField else { return }
        searchDebounce?.cancel()
        let item = DispatchWorkItem { [weak self] in self?.reapply() }
        searchDebounce = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25, execute: item)
    }

    /// Combine the global search and all per-column filters into one engine query.
    private func effectiveQuery() -> String {
        var clauses: [String] = []
        let global = searchField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if !global.isEmpty { clauses.append("(\(global))") }
        for (col, value) in columnFilters.sorted(by: { $0.key < $1.key }) {
            let v = value.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "\"", with: "")
            if v.isEmpty { continue }
            clauses.append("#\(col):\"\(v)\"") // index scope is robust to spaces in names
        }
        return clauses.joined(separator: " AND ")
    }

    /// Re-run the filter pipeline (text + column filters + tagged-only) and render.
    private func reapply() {
        guard let engine = engine else { return }
        let query = effectiveQuery()

        if query.isEmpty {
            baseView = taggedOnly ? engine.taggedView() : engine.allRows()
            refresh()
            return
        }

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result = engine.search(query)
            DispatchQueue.main.async {
                guard let self = self, let engine = self.engine else { return }
                switch result {
                case .success(let view):
                    self.baseView = self.taggedOnly ? engine.intersectTags(view: view) : view
                    self.refresh()
                case .failure(let error):
                    self.statusLabel.stringValue = "Query error: \(error.message)"
                    self.statusLabel.textColor = .systemRed
                }
            }
        }
    }

    // MARK: Sort (RF-05)

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange old: [NSSortDescriptor]) {
        refresh()
    }

    private func applySort() {
        guard let engine = engine, let base = baseView else { return }
        let keys: [(col: Int, ascending: Bool)] = tableView.sortDescriptors.compactMap { descriptor in
            guard let key = descriptor.key, let col = dataColumnIndex(NSUserInterfaceItemIdentifier(key)) else { return nil }
            return (col, descriptor.ascending)
        }
        currentView = keys.isEmpty ? base : engine.sort(view: base, keys: keys)
    }

    // MARK: Presentation

    private func refresh() {
        // Cell selection and the row cache are addressed by view row, which
        // changes on any filter/sort/group — clear them.
        cellSelection.removeAll()
        cellAnchor = nil
        rowCache.removeAll(keepingCapacity: true)
        applySort()
        if groupColumns.isEmpty {
            outlineScroll.isHidden = true
            scrollView.isHidden = false
            groupTree = nil
            tableView.reloadData()
            updateStatus()
        } else {
            scrollView.isHidden = true
            outlineScroll.isHidden = false
            rebuildGroupTree()
        }
    }

    private func rebuildGroupTree() {
        guard let engine = engine, let view = currentView else {
            groupTree = nil
            outlineView.reloadData()
            updateStatus()
            return
        }
        groupGeneration += 1
        let generation = groupGeneration
        let cols = groupColumns
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let tree = engine.group(view: view, cols: cols)
            DispatchQueue.main.async {
                guard let self = self, generation == self.groupGeneration else { return }
                self.groupTree = tree
                self.outlineView.reloadData()
                if let tree = tree, tree.rootCount <= 100 {
                    for i in 0..<tree.rootCount {
                        self.outlineView.expandItem(NSNumber(value: tree.rootChild(i)))
                    }
                }
                self.updateStatus()
            }
        }
    }

    private func updateStatus() {
        statusLabel.textColor = .secondaryLabelColor
        guard let engine = engine else {
            statusLabel.stringValue = "Open a file to begin  (⌘O)"
            return
        }
        let total = engine.rowCount
        let shown = currentView?.count ?? total
        var parts = shown == total
            ? "\(decimalString(total)) rows"
            : "\(decimalString(shown)) of \(decimalString(total)) rows"
        let tagged = engine.taggedCount
        if tagged > 0 { parts += " · \(decimalString(tagged)) tagged" }
        if !groupColumns.isEmpty, let tree = groupTree {
            parts += " · \(decimalString(tree.rootCount)) groups"
        }
        let hidden = hiddenColumns.count
        if hidden > 0 { parts += " · \(hidden) hidden col\(hidden == 1 ? "" : "s")" }
        if !cellSelection.isEmpty { parts += " · \(cellSelection.count) cell\(cellSelection.count == 1 ? "" : "s") selected" }
        statusLabel.stringValue = parts
    }

    private func decimalString(_ value: Int) -> String {
        Self.decimalFormatter.string(from: NSNumber(value: value)) ?? "\(value)"
    }

    // MARK: NSTableView (flat) data source / delegate

    func numberOfRows(in tableView: NSTableView) -> Int {
        currentView?.count ?? 0
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let engine = engine, let view = currentView, let column = tableColumn else { return nil }

        switch column.identifier {
        case Self.tagColumnID:
            let dataRow = engine.dataRowId(view: view, viewRow: row)
            let checkbox = reusableCheckbox(in: tableView)
            checkbox.tag = dataRow ?? -1
            checkbox.state = (dataRow.map { engine.isTagged(dataRow: $0) } ?? false) ? .on : .off
            return checkbox
        case Self.lineColumnID:
            let dataRow = engine.dataRowId(view: view, viewRow: row)
            let cell = reusableGridCell(in: tableView, id: column.identifier)
            cell.configure(dataRow.map { decimalString($0 + 1) } ?? "", color: .secondaryLabelColor, selected: false)
            return cell
        default:
            guard let colIndex = dataColumnIndex(column.identifier) else { return nil }
            let values = cachedRowValues(engine: engine, view: view, row: row)
            let raw = colIndex < values.count ? values[colIndex] : ""
            let text = singleLine(displayValue(raw, col: colIndex))
            let selected = cellSelection.contains(CellRef(row: row, col: colIndex))
            let cell = reusableGridCell(in: tableView, id: column.identifier)
            cell.configure(text, color: format.color(for: text) ?? .labelColor, selected: selected)
            return cell
        }
    }

    private func reusableGridCell(in view: NSTableView, id: NSUserInterfaceItemIdentifier) -> GridCellView {
        if let reused = view.makeView(withIdentifier: id, owner: self) as? GridCellView {
            return reused
        }
        let cell = GridCellView()
        cell.identifier = id
        return cell
    }

    // MARK: NSOutlineView (grouped) data source / delegate

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        guard let tree = groupTree else { return 0 }
        guard let item = item else { return tree.rootCount }
        return tree.childCount(itemValue(item))
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        guard let tree = groupTree else { return NSNumber(value: GroupTree.invalid) }
        let value = item == nil ? tree.rootChild(index) : tree.child(itemValue(item!), index)
        return NSNumber(value: value)
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        guard let tree = groupTree else { return false }
        let value = itemValue(item)
        return tree.isGroup(value) && tree.childCount(value) > 0
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        guard let tree = groupTree, let engine = engine, let column = tableColumn,
              let colIndex = dataColumnIndex(column.identifier) else { return nil }
        let value = itemValue(item)
        let field = reusableField(in: outlineView, id: column.identifier)

        if tree.isGroup(value) {
            if colIndex == groupColumns.first {
                field.stringValue = "\(tree.label(value))   (\(decimalString(tree.count(value))))"
                field.textColor = .labelColor
                field.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .semibold)
            } else {
                field.stringValue = ""
            }
        } else if let dataRow = tree.rowId(value) {
            let text = singleLine(engine.cell(dataRow: dataRow, col: colIndex))
            field.stringValue = text
            field.textColor = format.color(for: text) ?? .labelColor
            field.font = Self.cellFont
        }
        return field
    }

    private func itemValue(_ item: Any) -> Int64 {
        (item as? NSNumber)?.int64Value ?? GroupTree.invalid
    }

    // MARK: Cell views

    private func reusableField(in view: NSTableView, id: NSUserInterfaceItemIdentifier) -> NSTextField {
        if let reused = view.makeView(withIdentifier: id, owner: self) as? NSTextField {
            return reused
        }
        let field = NSTextField(labelWithString: "")
        field.identifier = id
        field.lineBreakMode = .byTruncatingTail
        field.isSelectable = true
        field.drawsBackground = false
        field.isBordered = false
        return field
    }

    private func reusableCheckbox(in view: NSTableView) -> NSButton {
        if let reused = view.makeView(withIdentifier: Self.tagColumnID, owner: self) as? NSButton {
            return reused
        }
        let button = NSButton(checkboxWithTitle: "", target: self, action: #selector(toggleTag(_:)))
        button.identifier = Self.tagColumnID
        button.imagePosition = .imageOnly
        return button
    }

    private func dataColumnIndex(_ id: NSUserInterfaceItemIdentifier) -> Int? {
        guard id.rawValue.hasPrefix("c") else { return nil }
        return Int(id.rawValue.dropFirst())
    }

    /// Fetch a view row's column values once (FFI) and cache them, so rendering
    /// all of a row's cells costs one call instead of one per cell.
    private func cachedRowValues(engine: Engine, view: DataView, row: Int) -> [String] {
        if let cached = rowCache[row] { return cached }
        if rowCache.count > 4096 { rowCache.removeAll(keepingCapacity: true) }
        let values = engine.rowCells(view: view, row: row)
        rowCache[row] = values
        return values
    }

    // MARK: Tagging (① persists across filter/sort/group)

    @objc private func toggleTag(_ sender: NSButton) {
        guard let engine = engine, sender.tag >= 0 else { return }
        engine.setTag(dataRow: sender.tag, tagged: sender.state == .on)
        if taggedOnly {
            reapply() // the row leaves/enters the filtered set
        } else {
            updateStatus()
        }
    }

    // MARK: Per-column header menu (③ filter + sort + hide)

    func headerMenu(forTableColumn tableColumnIndex: Int) -> NSMenu? {
        guard tableColumnIndex < tableView.tableColumns.count,
              let col = dataColumnIndex(tableView.tableColumns[tableColumnIndex].identifier),
              let engine = engine else { return nil }
        let name = engine.columnName(col)

        let menu = NSMenu()
        let title = NSMenuItem(title: name, action: nil, keyEquivalent: "")
        title.isEnabled = false
        menu.addItem(title)
        menu.addItem(.separator())

        let asc = NSMenuItem(title: "Sort Ascending", action: #selector(sortColumnAsc(_:)), keyEquivalent: "")
        asc.target = self; asc.representedObject = col; menu.addItem(asc)
        let desc = NSMenuItem(title: "Sort Descending", action: #selector(sortColumnDesc(_:)), keyEquivalent: "")
        desc.target = self; desc.representedObject = col; menu.addItem(desc)
        menu.addItem(.separator())

        let filter = NSMenuItem(title: "Filter…", action: #selector(filterColumn(_:)), keyEquivalent: "")
        filter.target = self; filter.representedObject = col; menu.addItem(filter)
        if columnFilters[col]?.isEmpty == false {
            let clear = NSMenuItem(title: "Clear Column Filter", action: #selector(clearColumnFilter(_:)), keyEquivalent: "")
            clear.target = self; clear.representedObject = col; menu.addItem(clear)
        }
        menu.addItem(.separator())

        let hide = NSMenuItem(title: "Hide “\(name)”", action: #selector(hideColumn(_:)), keyEquivalent: "")
        hide.target = self; hide.representedObject = col; menu.addItem(hide)
        return menu
    }

    @objc private func sortColumnAsc(_ sender: NSMenuItem) { sortColumn(sender, ascending: true) }
    @objc private func sortColumnDesc(_ sender: NSMenuItem) { sortColumn(sender, ascending: false) }

    private func sortColumn(_ sender: NSMenuItem, ascending: Bool) {
        guard let col = sender.representedObject as? Int else { return }
        tableView.sortDescriptors = [NSSortDescriptor(key: "c\(col)", ascending: ascending)]
    }

    @objc private func filterColumn(_ sender: NSMenuItem) {
        guard let col = sender.representedObject as? Int, let engine = engine, let window = window else { return }
        let alert = NSAlert()
        alert.messageText = "Filter “\(engine.columnName(col))”"
        alert.informativeText = "Show rows where this column contains the text below. Leave empty to clear."
        alert.addButton(withTitle: "Apply")
        alert.addButton(withTitle: "Cancel")
        let input = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 24))
        input.stringValue = columnFilters[col] ?? ""
        alert.accessoryView = input
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            let value = input.stringValue.trimmingCharacters(in: .whitespaces)
            if value.isEmpty { self?.columnFilters[col] = nil } else { self?.columnFilters[col] = value }
            self?.reapply()
        }
    }

    @objc private func clearColumnFilter(_ sender: NSMenuItem) {
        guard let col = sender.representedObject as? Int else { return }
        columnFilters[col] = nil
        reapply()
    }

    @objc private func hideColumn(_ sender: NSMenuItem) {
        guard let col = sender.representedObject as? Int else { return }
        setColumnHidden(col, true)
    }

    // MARK: Cell selection & copy (② specific cell / multiple cells)

    /// ⌘C: copy the selected cells if any, otherwise the whole selected rows.
    @objc func copy(_ sender: Any?) {
        if !cellSelection.isEmpty { copySelectedCells() } else { writeSelection(includeHeaders: false) }
    }

    /// ⌘⌥C: copy just the cell value(s).
    @objc func copyCell(_ sender: Any?) {
        if !cellSelection.isEmpty { copySelectedCells() } else if let cell = clickedDataCell() {
            setPasteboard(cell.value)
        }
    }

    @objc func copyWithHeaders(_ sender: Any?) { writeSelection(includeHeaders: true) }

    /// Click handler driving cell selection (flat table only): plain = one cell,
    /// ⌘ = toggle/add, ⇧ = rectangular range; clicking the Tag/# column resets
    /// to row mode.
    @objc private func handleCellClick(_ sender: Any?) {
        guard groupColumns.isEmpty else { return }
        let row = tableView.clickedRow
        let displayCol = tableView.clickedColumn
        guard row >= 0, displayCol >= 0, displayCol < tableView.tableColumns.count else { return }
        guard let dataCol = dataColumnIndex(tableView.tableColumns[displayCol].identifier) else {
            if !cellSelection.isEmpty {
                cellSelection.removeAll(); cellAnchor = nil; tableView.reloadData(); updateStatus()
            }
            return
        }
        let clicked = CellRef(row: row, col: dataCol)
        let flags = NSApp.currentEvent?.modifierFlags ?? []
        if flags.contains(.shift), let anchor = cellAnchor {
            cellSelection = rectangleSelection(from: anchor, to: clicked)
        } else if flags.contains(.command) {
            if cellSelection.contains(clicked) { cellSelection.remove(clicked) } else { cellSelection.insert(clicked) }
            cellAnchor = clicked
        } else {
            cellSelection = [clicked]
            cellAnchor = clicked
        }
        tableView.reloadData()
        updateStatus()
    }

    private func rectangleSelection(from a: CellRef, to b: CellRef) -> Set<CellRef> {
        let visible = visibleDataColumns()
        guard let ia = visible.firstIndex(of: a.col), let ib = visible.firstIndex(of: b.col) else { return [b] }
        let cols = visible[min(ia, ib)...max(ia, ib)]
        var set = Set<CellRef>()
        for r in min(a.row, b.row)...max(a.row, b.row) {
            for c in cols { set.insert(CellRef(row: r, col: c)) }
        }
        return set
    }

    /// Copy selected cells: rows ascending, visible-column order within a row,
    /// tab-separated; newline between rows. One cell → just its raw value.
    private func copySelectedCells() {
        guard let engine = engine, let view = currentView, !cellSelection.isEmpty else { return }
        let rows = Set(cellSelection.map { $0.row }).sorted()
        let visible = visibleDataColumns()
        var lines: [String] = []
        for r in rows {
            let cols = visible.filter { cellSelection.contains(CellRef(row: r, col: $0)) }
            lines.append(cols.map { engine.cell(view: view, row: r, col: $0) }.joined(separator: "\t"))
        }
        setPasteboard(lines.joined(separator: "\n"))
    }

    private func setPasteboard(_ string: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(string, forType: .string)
    }

    private func clickedDataCell() -> (viewRow: Int, dataCol: Int, value: String)? {
        guard let engine = engine, let view = currentView else { return nil }
        let row = tableView.clickedRow
        let displayCol = tableView.clickedColumn
        guard row >= 0, displayCol >= 0, displayCol < tableView.tableColumns.count,
              let dataCol = dataColumnIndex(tableView.tableColumns[displayCol].identifier) else { return nil }
        return (row, dataCol, engine.cell(view: view, row: row, col: dataCol))
    }

    // MARK: Right-click context menu

    /// Build the cell context menu dynamically so the VirusTotal item reflects
    /// the value under the cursor.
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        addItem(menu, "Copy Cell", #selector(ctxCopyCell(_:)))
        addItem(menu, "Copy Column", #selector(ctxCopyColumn(_:)))
        menu.addItem(.separator())
        addItem(menu, "Copy Row(s)", #selector(ctxCopyRows(_:)))
        addItem(menu, "Copy with Headers", #selector(copyWithHeaders(_:)))
        menu.addItem(.separator())
        addItem(menu, "Filter to This Value", #selector(ctxFilterToValue(_:)))

        // Enrichment: a VirusTotal lookup when the clicked cell is a hash or IP.
        if let cell = clickedDataCell(), let ioc = IOCDetector.detect(cell.value),
           let url = IOCDetector.virusTotalURL(type: ioc.type, value: ioc.value) {
            menu.addItem(.separator())
            let item = NSMenuItem(title: "Look up \(ioc.type.label) on VirusTotal",
                                  action: #selector(ctxVirusTotal(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = url
            menu.addItem(item)
        }

        menu.addItem(.separator())
        addItem(menu, "Tag Selected Rows", #selector(ctxTagSelected(_:)))
        addItem(menu, "Untag Selected Rows", #selector(ctxUntagSelected(_:)))
    }

    private func addItem(_ menu: NSMenu, _ title: String, _ action: Selector) {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        menu.addItem(item)
    }

    @objc private func ctxVirusTotal(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        NSWorkspace.shared.open(url)
    }

    @objc private func ctxCopyCell(_ sender: Any?) {
        if !cellSelection.isEmpty { copySelectedCells() } else if let cell = clickedDataCell() {
            setPasteboard(cell.value)
        }
    }

    @objc private func ctxCopyColumn(_ sender: Any?) {
        guard let engine = engine, let view = currentView, let cell = clickedDataCell() else { return }
        var rows = tableView.selectedRowIndexes
        if rows.isEmpty { rows = IndexSet(integer: cell.viewRow) }
        let values = rows.map { engine.cell(view: view, row: $0, col: cell.dataCol) }
        setPasteboard(values.joined(separator: "\n"))
    }

    @objc private func ctxCopyRows(_ sender: Any?) { writeSelection(includeHeaders: false) }

    @objc private func ctxFilterToValue(_ sender: Any?) {
        guard let cell = clickedDataCell() else { return }
        columnFilters[cell.dataCol] = cell.value
        reapply()
    }

    @objc private func ctxTagSelected(_ sender: Any?) { tagSelectedRows(true) }
    @objc private func ctxUntagSelected(_ sender: Any?) { tagSelectedRows(false) }

    private func tagSelectedRows(_ tagged: Bool) {
        guard let engine = engine, let view = currentView else { return }
        var rows = tableView.selectedRowIndexes
        if rows.isEmpty, tableView.clickedRow >= 0 { rows = IndexSet(integer: tableView.clickedRow) }
        for viewRow in rows {
            if let dataRow = engine.dataRowId(view: view, viewRow: viewRow) {
                engine.setTag(dataRow: dataRow, tagged: tagged)
            }
        }
        tableView.reloadData()
        if taggedOnly { reapply() } else { updateStatus() }
    }

    // MARK: Clipboard (RF-09)

    private func writeSelection(includeHeaders: Bool) {
        guard let engine = engine else { return }
        let dataRows = selectedDataRows()
        guard !dataRows.isEmpty else { return }
        let cols = visibleDataColumns()

        var lines: [String] = []
        if includeHeaders {
            lines.append(cols.map { engine.columnName($0) }.joined(separator: "\t"))
        }
        for dataRow in dataRows {
            lines.append(cols.map { engine.cell(dataRow: dataRow, col: $0) }.joined(separator: "\t"))
        }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(lines.joined(separator: "\n"), forType: .string)
    }

    private func selectedDataRows() -> [Int] {
        guard let engine = engine else { return [] }
        if groupColumns.isEmpty {
            guard let view = currentView else { return [] }
            return tableView.selectedRowIndexes.compactMap { engine.dataRowId(view: view, viewRow: $0) }
        }
        guard let tree = groupTree else { return [] }
        return outlineView.selectedRowIndexes.compactMap { row in
            guard let item = outlineView.item(atRow: row) as? NSNumber else { return nil }
            return tree.rowId(item.int64Value)
        }
    }

    // MARK: Smart paste (RF-09)

    @objc func paste(_ sender: Any?) {
        guard engine != nil, let text = NSPasteboard.general.string(forType: .string) else { return }
        var seen = Set<String>()
        let tokens = text
            .components(separatedBy: CharacterSet(charactersIn: "\n\r\t,"))
            .map { $0.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "\"", with: "") }
            .filter { !$0.isEmpty && seen.insert($0).inserted }
        guard !tokens.isEmpty else { return }
        let query = tokens.count == 1 ? tokens[0] : tokens.map { "\"\($0)\"" }.joined(separator: " OR ")
        searchField.stringValue = query
        searchDebounce?.cancel()
        reapply()
    }

    @objc func focusSearch(_ sender: Any?) {
        window?.makeFirstResponder(searchField)
    }

    // MARK: Export (RF-10 — respects hidden columns ②)

    @objc func exportData(_ sender: NSMenuItem) {
        guard let engine = engine, let view = currentView, let window = window else { return }
        let format = UInt32(max(0, min(3, sender.tag)))
        let ext = ["csv", "tsv", "json", "html"][Int(format)]

        let panel = NSSavePanel()
        panel.nameFieldStringValue = "nexusview_export.\(ext)"
        panel.canCreateDirectories = true
        let columns = visibleDataColumns()
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            if !engine.export(view: view, format: format, columns: columns, path: url.path) {
                self?.presentError(Engine.lastError)
            }
        }
    }

    // MARK: Helpers

    private func presentError(_ message: String) {
        let alert = NSAlert()
        alert.messageText = "Operation failed"
        alert.informativeText = message.isEmpty ? "Unknown error." : message
        alert.alertStyle = .warning
        if let window = window {
            alert.beginSheetModal(for: window)
        } else {
            alert.runModal()
        }
    }
}
