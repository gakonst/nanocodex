#if os(macOS)
import AppKit
import SwiftUI

/// Independent of the controlling canvas: lease release dismantles capture but
/// must not remove the observer needed to recognize foreground reentry.
struct RemoteWindowFocusObserver: NSViewRepresentable {
    let changed: (Bool) -> Void
    func makeNSView(context: Context) -> FocusView { FocusView(changed: changed) }
    func updateNSView(_ view: FocusView, context: Context) { view.changed = changed }
    static func dismantleNSView(_ view: FocusView, coordinator: ()) { view.stop() }

    final class FocusView: NSView {
        var changed: (Bool) -> Void
        private var observers: [NSObjectProtocol] = []
        private var revision = 0
        init(changed: @escaping (Bool) -> Void) {
            self.changed = changed
            super.init(frame: .zero)
            for name in [NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification,
                         NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification] {
                observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                    self?.publish(deferred: false)
                })
            }
        }
        required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
        override func viewDidMoveToWindow() { super.viewDidMoveToWindow(); publish(deferred: true) }
        private func publish(deferred: Bool) {
            revision += 1
            if !deferred {
                // Never coalesce away a brief deactivation: the lease must be
                // released even if another key-window notification follows.
                changed(NSApp.isActive && window?.isKeyWindow == true)
                return
            }
            let current = revision
            // Attaching/removing a SwiftUI NSView must not publish into its
            // graph synchronously. Coalesce notifications, then reread focus.
            DispatchQueue.main.async { [weak self] in
                guard let self, current == self.revision else { return }
                self.changed(NSApp.isActive && self.window?.isKeyWindow == true)
            }
        }
        func stop() {
            revision += 1
            observers.forEach(NotificationCenter.default.removeObserver); observers.removeAll()
        }
        deinit { stop() }
    }
}
#endif
