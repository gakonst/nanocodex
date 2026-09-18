#if os(macOS)
import AppKit
import SwiftUI

/// Owned by the dashboard so AppKit's weak window delegate outlives the window.
@MainActor
final class RemoteFullscreen: NSObject, ObservableObject, NSWindowDelegate {
    @Published private(set) var isPresented = false
    private var window: NSWindow?
    private weak var viewer: RemoteViewer?

    func open(viewer: RemoteViewer) {
        if let window { window.makeKeyAndOrderFront(nil); return }
        guard !isPresented, viewer.hand != nil else { return }
        // Moving the renderer never implies permission to capture input.
        viewer.releaseControl()
        self.viewer = viewer
        isPresented = true
        // Let SwiftUI remove the embedded renderer before installing this one.
        DispatchQueue.main.async { [weak self, weak viewer] in
            guard let self, self.isPresented, let viewer else { return }
            self.present(viewer: viewer)
        }
    }

    private func present(viewer: RemoteViewer) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1100, height: 700),
                              styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.title = viewer.hand.map { $0.machineName + " · " + $0.name } ?? "Remote screen"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.toolbar = nil
        for button: NSWindow.ButtonType in [.closeButton, .miniaturizeButton, .zoomButton] {
            window.standardWindowButton(button)?.isHidden = true
        }
        window.backgroundColor = .black
        window.collectionBehavior = [.fullScreenPrimary]
        window.delegate = self
        window.contentView = NSHostingView(rootView: RemoteFullscreenContent(viewer: viewer) { [weak self] in self?.close() })
        self.window = window
        window.center()
        window.makeKeyAndOrderFront(nil)
        window.toggleFullScreen(nil)
    }

    func close() {
        viewer?.releaseControl()
        // Remove the detached renderer before making the embedded one visible.
        if let window {
            window.delegate = nil
            window.contentView = nil
            window.close()
        }
        window = nil
        viewer = nil
        isPresented = false
    }

    func windowWillClose(_ notification: Notification) { close() }
    func windowDidResignKey(_ notification: Notification) { viewer?.releaseControl() }
    func windowWillExitFullScreen(_ notification: Notification) { viewer?.releaseControl() }
    func windowDidExitFullScreen(_ notification: Notification) { close() }
    func windowDidFailToEnterFullScreen(_ window: NSWindow) { close() }
}

private struct RemoteFullscreenContent: View {
    @ObservedObject var viewer: RemoteViewer
    let close: () -> Void

    var body: some View {
        RemoteCanvas(viewer: viewer, capturesInput: true)
            .accessibilityIdentifier("remote-fullscreen-canvas")
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(.black)
            .ignoresSafeArea()
            .overlay(alignment: .top) {
                if !viewer.controlling {
                    HStack(spacing: 12) {
                        Button(action: close) { Label("Back", systemImage: "arrow.down.right.and.arrow.up.left") }
                            .accessibilityIdentifier("remote-fullscreen-close")
                        if viewer.connecting {
                            ProgressView().controlSize(.small)
                            Text(viewer.status).lineLimit(1)
                        } else if !viewer.connected {
                            Text("Screen disconnected")
                            Button("Reconnect") { Task { await viewer.reconnect() } }
                                .accessibilityIdentifier("remote-fullscreen-reconnect")
                        } else if viewer.hand?.controllable == true {
                            Button("Take control") { viewer.takeControl() }
                                .buttonStyle(.borderedProminent)
                                .accessibilityIdentifier("remote-fullscreen-take-control")
                            Text("⌘⇧Esc releases control").foregroundStyle(.secondary)
                        } else {
                            Text("View only").foregroundStyle(.secondary)
                        }
                    }
                    .font(.callout)
                    .controlSize(.small)
                    .padding(10)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
                    .padding(12)
                }
            }
    }
}
#endif
