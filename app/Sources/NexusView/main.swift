import AppKit

// Programmatic entry point (no storyboard / nib). A regular activation policy
// gives the process a Dock icon, menu bar, and focus when launched directly.
let application = NSApplication.shared
let delegate = AppDelegate()
application.delegate = delegate
application.setActivationPolicy(.regular)
application.run()
