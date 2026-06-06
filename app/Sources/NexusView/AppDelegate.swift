import AppKit

/// Application lifecycle, native multi-tab management (RF-07), sessions, and the
/// main menu. Each open file is an isolated `MainWindowController` (its own
/// engine and memory); macOS groups them as native tabs.
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// Strong references to every open tab's controller.
    private var controllers: [MainWindowController] = []

    func applicationWillFinishLaunching(_ notification: Notification) {
        // Set the menu before `application(_:open:)` can fire on a Finder open.
        NSApp.mainMenu = buildMenu()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.activate(ignoringOtherApps: true)

        // A file passed on the command line (terminal launch).
        if let path = CommandLine.arguments.dropFirst().first(where: { !$0.hasPrefix("-") }),
           FileManager.default.fileExists(atPath: path) {
            openFile(url: URL(fileURLWithPath: path))
        }

        // Only show an empty tab if nothing else opened a window. A Finder
        // "Open With" routes through `application(_:open:)`, which can run before
        // OR after this method — gating on `controllers.isEmpty` guarantees a
        // single tab either way (fixes the double-tab on open).
        if controllers.isEmpty {
            let controller = makeController()
            controller.showWindow(nil)
            controller.window?.makeKeyAndOrderFront(nil)
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    /// Files opened via Finder / Launch Services.
    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls { openFile(url: url) }
    }

    // MARK: Tabs (RF-07)

    private func makeController() -> MainWindowController {
        let controller = MainWindowController()
        controllers.append(controller)
        if let window = controller.window {
            NotificationCenter.default.addObserver(
                self, selector: #selector(windowWillClose(_:)),
                name: NSWindow.willCloseNotification, object: window
            )
        }
        return controller
    }

    @objc private func windowWillClose(_ note: Notification) {
        guard let window = note.object as? NSWindow else { return }
        controllers.removeAll { $0.window === window }
    }

    /// The controller of the frontmost tab.
    private var keyController: MainWindowController? {
        (NSApp.keyWindow?.windowController as? MainWindowController) ?? controllers.last
    }

    /// Open `url` into the current empty tab, or a new tab if it already has a file.
    private func openFile(url: URL, session: SessionState? = nil) {
        if let key = keyController, !key.hasFile {
            key.load(url: url, session: session)
            return
        }
        let controller = makeController()
        guard let newWindow = controller.window else { return }
        if let host = NSApp.keyWindow {
            host.addTabbedWindow(newWindow, ordered: .above)
        }
        controller.showWindow(nil)
        newWindow.makeKeyAndOrderFront(nil)
        controller.load(url: url, session: session)
    }

    @objc func openDocument(_ sender: Any?) {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.message = "Choose a CSV, TSV, PSV, bodyfile, or other delimited log"
        panel.begin { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            self?.openFile(url: url)
        }
    }

    @objc func newTab(_ sender: Any?) {
        let controller = makeController()
        guard let newWindow = controller.window else { return }
        if let host = NSApp.keyWindow {
            host.addTabbedWindow(newWindow, ordered: .above)
        }
        controller.showWindow(nil)
        newWindow.makeKeyAndOrderFront(nil)
    }

    /// Enables the tab bar's "+" button.
    @objc func newWindowForTab(_ sender: Any?) { newTab(sender) }

    // MARK: Sessions

    @objc func saveSession(_ sender: Any?) {
        guard let controller = keyController,
              let state = controller.sessionState(),
              let window = controller.window else {
            NSSound.beep()
            return
        }
        let panel = NSSavePanel()
        let base = (state.filePath as NSString).lastPathComponent
        panel.nameFieldStringValue = "\(base).\(SessionState.fileExtension)"
        panel.canCreateDirectories = true
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            do {
                try state.encoded().write(to: url)
            } catch {
                self?.presentError("Could not write session: \(error.localizedDescription)", in: window)
            }
        }
    }

    @objc func openSession(_ sender: Any?) {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.message = "Choose a NexusView session"
        panel.begin { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            do {
                let state = try SessionState.decoded(from: Data(contentsOf: url))
                let fileURL = URL(fileURLWithPath: state.filePath)
                guard FileManager.default.fileExists(atPath: fileURL.path) else {
                    self?.presentError("The session's file no longer exists:\n\(state.filePath)", in: NSApp.keyWindow)
                    return
                }
                self?.openFile(url: fileURL, session: state)
            } catch {
                self?.presentError("Could not read session: \(error.localizedDescription)", in: NSApp.keyWindow)
            }
        }
    }

    private func presentError(_ message: String, in window: NSWindow?) {
        let alert = NSAlert()
        alert.messageText = "Session error"
        alert.informativeText = message
        alert.alertStyle = .warning
        if let window = window { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }

    // MARK: Menu

    private func buildMenu() -> NSMenu {
        let mainMenu = NSMenu()

        // Application menu.
        let appItem = NSMenuItem()
        mainMenu.addItem(appItem)
        let appMenu = NSMenu()
        appItem.submenu = appMenu
        appMenu.addItem(withTitle: "About NexusView",
                        action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Quit NexusView",
                        action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")

        // File menu.
        let fileItem = NSMenuItem()
        mainMenu.addItem(fileItem)
        let fileMenu = NSMenu(title: "File")
        fileItem.submenu = fileMenu

        let newTab = NSMenuItem(title: "New Tab", action: #selector(newTab(_:)), keyEquivalent: "t")
        newTab.target = self
        fileMenu.addItem(newTab)
        let open = NSMenuItem(title: "Open…", action: #selector(openDocument(_:)), keyEquivalent: "o")
        open.target = self
        fileMenu.addItem(open)
        fileMenu.addItem(withTitle: "Close Tab", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")

        fileMenu.addItem(.separator())
        let openSession = NSMenuItem(title: "Open Session…", action: #selector(openSession(_:)), keyEquivalent: "O")
        openSession.keyEquivalentModifierMask = [.command, .shift]
        openSession.target = self
        fileMenu.addItem(openSession)
        let saveSession = NSMenuItem(title: "Save Session…", action: #selector(saveSession(_:)), keyEquivalent: "s")
        saveSession.target = self
        fileMenu.addItem(saveSession)

        fileMenu.addItem(.separator())
        let exportItem = NSMenuItem(title: "Export", action: nil, keyEquivalent: "")
        let exportMenu = NSMenu(title: "Export")
        exportItem.submenu = exportMenu
        for (title, tag) in [("CSV (current view)", 0), ("TSV (current view)", 1), ("JSON", 2), ("HTML table", 3)] {
            let item = NSMenuItem(title: title, action: #selector(MainWindowController.exportData(_:)), keyEquivalent: "")
            item.tag = tag
            exportMenu.addItem(item)
        }
        fileMenu.addItem(exportItem)

        // Edit menu — standard selectors + responder chain (see notes below).
        let editItem = NSMenuItem()
        mainMenu.addItem(editItem)
        let editMenu = NSMenu(title: "Edit")
        editItem.submenu = editMenu
        editMenu.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: "Copy", action: #selector(MainWindowController.copy(_:)), keyEquivalent: "c")
        let copyCell = NSMenuItem(title: "Copy Cell(s)", action: #selector(MainWindowController.copyCell(_:)), keyEquivalent: "c")
        copyCell.keyEquivalentModifierMask = [.command, .option]
        editMenu.addItem(copyCell)
        let copyHeaders = NSMenuItem(title: "Copy with Headers", action: #selector(MainWindowController.copyWithHeaders(_:)), keyEquivalent: "c")
        copyHeaders.keyEquivalentModifierMask = [.command, .shift]
        editMenu.addItem(copyHeaders)
        editMenu.addItem(withTitle: "Paste", action: #selector(MainWindowController.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Find…", action: #selector(MainWindowController.focusSearch(_:)), keyEquivalent: "f")

        // Window menu — gives native tab cycling, "Merge All Windows", etc.
        let windowItem = NSMenuItem()
        mainMenu.addItem(windowItem)
        let windowMenu = NSMenu(title: "Window")
        windowItem.submenu = windowMenu
        windowMenu.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        windowMenu.addItem(withTitle: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")
        NSApp.windowsMenu = windowMenu

        return mainMenu
    }
}
