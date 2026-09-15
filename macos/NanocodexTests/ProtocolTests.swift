import XCTest
import SwiftUI
import NanocodexRemote
@testable import Nanocodex

final class ProtocolTests: XCTestCase {
    @MainActor
    func testKeyboardLookupVisitsEachAncestorOnce() {
        final class CountingView: NSView {
            var reads = 0
            override var subviews: [NSView] {
                get { reads += 1; return super.subviews }
                set { super.subviews = newValue }
            }
        }
        let ancestors = (0..<16).map { _ in CountingView() }
        for index in 0..<(ancestors.count - 1) { ancestors[index].addSubview(ancestors[index + 1]) }
        let keyboard = WorkspaceKeyboardView()
        ancestors.last?.addSubview(keyboard)
        ancestors.forEach { $0.reads = 0 }
        XCTAssertTrue(WorkspaceKeyboardView.find(in: ancestors[0]) === keyboard)
        XCTAssertEqual(ancestors.reduce(0) { $0 + $1.reads }, ancestors.count,
                       "Nested native hosts must not multiply keyboard-focus search work")
    }

    @MainActor
    func testConversationColumnStaysCenteredAcrossWindowSizes() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-column-fixture")
        model.runtime.requestOverride = { _, _ in .null }
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [WorkspaceTab(id: "column", threadId: "column-thread", title: "Desktop layout")]
        model.activeTabID = "column"; model.workspaceFilter = .all
        model.snapshots["column-thread"] = ThreadSnapshot(id: "column-thread", events: [], hasMore: false, connected: true, activeTurns: [], settings: AgentSettings())
        model.messages["column-thread"] = [
            MessageEntry(id: "column-user", turnId: "column-turn", kind: .user, text: "Keep this conversation balanced."),
            MessageEntry(id: "column-reply", turnId: "column-turn", kind: .assistant, text: "The conversation and composer should share one centered column, with balanced margins at every window size.")
        ]
        let host = NSHostingView(rootView: ContentView().environmentObject(model)); host.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1600, height: 900), styleMask: [.titled, .closable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
        window.toolbar = NSToolbar(identifier: "column-fixture-toolbar")
        window.toolbarStyle = .unified
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        func find<T: NSView>(_ type: T.Type, in view: NSView) -> [T] {
            ((view as? T).map { [$0] } ?? []) + view.subviews.flatMap { find(type, in: $0) }
        }
        var retainedEditor: ComposerTextView?
        for (width, position) in [(CGFloat(1600), "top"), (1600, "left"), (1200, "left"), (820, "left"), (1200, "top"), (1600, "top")] {
            model.setTabPosition(position)
            window.setContentSize(NSSize(width: width, height: 900))
            try await Task.sleep(for: .milliseconds(400)); host.layoutSubtreeIfNeeded()
            let input = try XCTUnwrap(find(ComposerTextView.self, in: host).first)
            if let retainedEditor { XCTAssertTrue(input === retainedEditor) } else { retainedEditor = input }
            let marker = try XCTUnwrap(find(TranscriptItemAnchor.MarkerView.self, in: host).first { $0.itemID == "column-reply" })
            let firstTurn = try XCTUnwrap(find(TranscriptItemAnchor.MarkerView.self, in: host).first { $0.itemID == "column-user" })
            XCTAssertLessThanOrEqual(firstTurn.convert(firstTurn.bounds, to: nil).maxY, window.contentLayoutRect.maxY,
                                     "The first user message stays below the native toolbar")
            let row = marker.convert(marker.bounds, to: host)
            let editor = input.convert(input.bounds, to: host)
            if position == "left", let sidebar = find(NSTableView.self, in: host).first, !sidebar.visibleRect.isEmpty {
                let sidebarBounds = sidebar.convert(sidebar.bounds, to: host)
                XCTAssertEqual(row.midX, (sidebarBounds.maxX + host.bounds.maxX) / 2, accuracy: 12,
                               "The transcript is centered beside the native sidebar")
            } else {
                XCTAssertEqual(row.midX, host.bounds.midX, accuracy: 2, "The transcript has balanced outer margins")
            }
            XCTAssertEqual(row.midX, editor.midX, accuracy: 2, "The composer and transcript share a center line")
            XCTAssertLessThanOrEqual(row.width, 820)
            XCTAssertGreaterThanOrEqual(row.minX, 24)
            let scroll = try XCTUnwrap(marker.enclosingScrollView)
            XCTAssertLessThanOrEqual(scroll.documentView?.bounds.height ?? .infinity, scroll.contentView.bounds.height + 2,
                                     "A fully visible first reply must not create an empty screen of scrollable space")
        }
    }

    @MainActor
    func testTabOrientationRetainsEditorSelectionAndLayout() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-tab-orientation")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.workspaceFilter = .all
        model.tabs = [WorkspaceTab(id: "one", title: "First conversation", draft: "Keep this draft"), WorkspaceTab(id: "two", title: "Second conversation")]
        model.activeTabID = "one"
        var saved: JSONValue?
        model.runtime.requestOverride = { method, args in if method == "saveLayout" { saved = args.first }; return .null }
        let host = NSHostingView(rootView: ContentView().environmentObject(model)); host.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        func find<T: NSView>(_ type: T.Type, in view: NSView) -> [T] {
            ((view as? T).map { [$0] } ?? []) + view.subviews.flatMap { find(type, in: $0) }
        }
        try await Task.sleep(for: .milliseconds(250))
        let editor = try XCTUnwrap(find(ComposerTextView.self, in: host).first)
        window.makeFirstResponder(editor); editor.setSelectedRange(NSRange(location: 5, length: 4))
        for position in ["left", "top", "left", "top"] {
            model.setTabPosition(position)
            try await Task.sleep(for: .milliseconds(200)); host.layoutSubtreeIfNeeded()
            XCTAssertTrue(find(ComposerTextView.self, in: host).contains { $0 === editor })
            XCTAssertEqual(editor.selectedRange(), NSRange(location: 5, length: 4))
            XCTAssertEqual(editor.string, "Keep this draft")
            XCTAssertEqual(model.activeTabID, "one")
            XCTAssertEqual(model.tabPosition, position)
        }
        model.setTabPosition("left")
        try await Task.sleep(for: .milliseconds(200))
        let sidebar = try XCTUnwrap(find(NSTableView.self, in: host).first)
        window.makeFirstResponder(sidebar)
        model.selectSidebarWorkspace(PaneNode(id: "two"))
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertTrue(window.firstResponder === sidebar, "Native sidebar selection must retain its arrow-key navigation")
        XCTAssertEqual(model.workspaceFocus, .sidebar)
        model.enterNavigation(in: window)
        XCTAssertTrue(window.firstResponder is WorkspaceKeyboardView, "Escape returns to v/h pane navigation")
        for (code, character) in [(UInt16(9), "v"), (UInt16(4), "h")] {
            let event = try XCTUnwrap(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0,
                                                     windowNumber: window.windowNumber, context: nil, characters: character,
                                                     charactersIgnoringModifiers: character, isARepeat: false, keyCode: code))
            window.firstResponder?.keyDown(with: event)
        }
        XCTAssertEqual(model.activePaneLayout?.leaves.count, 3, "Immediate v/h after leaving the sidebar must both reach pane navigation")
        XCTAssertEqual(model.activePaneLayout?.axis, "horizontal")
        XCTAssertEqual(model.activePaneLayout?.children[1].axis, "vertical")
        try await Task.sleep(for: .milliseconds(250))
        let surface = try XCTUnwrap(find(AgentSplitSurface.self, in: host).first)
        let hosts = surface.hosts
        for position in ["left", "top", "left"] {
            model.setTabPosition(position)
            try await Task.sleep(for: .milliseconds(200)); host.layoutSubtreeIfNeeded()
            XCTAssertTrue(find(AgentSplitSurface.self, in: host).first === surface)
            for (id, pane) in hosts { XCTAssertTrue(surface.hosts[id] === pane) }
        }
        model.setTabPosition("invalid")
        XCTAssertEqual(model.tabPosition, "left")
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(try XCTUnwrap(saved).decode(TabLayout.self).tabPosition, "left")
        let restored = AppModel(runtimeDirectory: "/tmp/nanocodex-tab-orientation-restored")
        restored.runtime.requestOverride = { _, _ in .null }; defer { restored.shutdown() }
        guard case .object(var state) = Self.connectedState else { return XCTFail("Missing fixture") }
        state["layout"] = saved
        var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("state"), "state": .object(state)])])); wire.append(10)
        restored.runtime.receiveForTesting(wire)
        XCTAssertEqual(restored.tabPosition, "left")
        XCTAssertEqual(restored.activePaneLayout, model.activePaneLayout)
    }

    @MainActor
    func testPaneDragPublishesOnlyWhenPreviewChanges() {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-drag-publications")
        defer { model.shutdown() }
        var changes = 0
        let subscription = model.objectWillChange.sink { changes += 1 }
        defer { subscription.cancel() }
        for _ in 0..<100 { model.cancelPaneDrag() }
        XCTAssertEqual(changes, 0, "Ordinary window focus changes cannot invalidate an idle workspace")
        model.draggingPaneID = "one"
        model.updatePaneDrop(target: "two", edge: .left)
        let before = changes
        for _ in 0..<100 { model.updatePaneDrop(target: "two", edge: .left) }
        XCTAssertEqual(changes, before, "Pointer movement within one docking region must not republish the workspace")
        model.updatePaneDrop(target: "two", edge: .right)
        XCTAssertEqual(changes, before + 1)
        XCTAssertEqual(model.paneDropEdge, .right)
        model.cancelPaneDrag()
        XCTAssertNil(model.draggingPaneID); XCTAssertNil(model.paneDropTarget); XCTAssertNil(model.paneDropEdge)
    }

    @MainActor
    func testNativeWindowMovementAndResizeRetainContent() async throws {
        func findEditor(_ view: NSView) -> ComposerTextView? {
            if let editor = view as? ComposerTextView { return editor }
            for child in view.subviews { if let found = findEditor(child) { return found } }
            return nil
        }
        let phase = ProcessInfo.processInfo.environment["NANOCODEX_PERFORMANCE_PHASE"] ?? "after"
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-window-motion")
        model.runtime.requestOverride = { _, _ in .null }
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [WorkspaceTab(id: "motion", threadId: "motion-thread", title: "Window performance", draft: "Retain this draft and selection")]
        model.activeTabID = "motion"; model.workspaceFilter = .all
        model.snapshots["motion-thread"] = ThreadSnapshot(id: "motion-thread", events: [], hasMore: false, connected: true, activeTurns: [], settings: AgentSettings())
        model.messages["motion-thread"] = (0..<24).map { index in
            MessageEntry(id: "motion-\(index)", turnId: "turn-\(index / 2)", kind: index.isMultiple(of: 2) ? .user : .assistant,
                         text: index.isMultiple(of: 2) ? "Improve window movement and resizing." : "Keep the **native conversation** responsive while the window moves. Preserve the editor, selection, and history while the text wraps to fit the available space.\n\nThe same content should stay mounted throughout the gesture.")
        }
        let host = NSHostingView(rootView: ContentView().environmentObject(model)); host.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 100, y: 100, width: 1200, height: 840), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        try await Task.sleep(for: .milliseconds(300))
        let editor = try XCTUnwrap(findEditor(host)); window.makeFirstResponder(editor)
        editor.setSelectedRange(NSRange(location: 7, length: 4))
        var publications = 0
        let subscription = model.objectWillChange.sink { publications += 1 }
        defer { subscription.cancel() }
        var measurements: [String: [Double]] = [:]
        for mode in ["move", "resize", "split-resize"] {
            if mode == "split-resize" {
                model.splitAgent(axis: "horizontal")
                try await Task.sleep(for: .milliseconds(300))
            }
            let retained = try XCTUnwrap(findEditor(host))
            publications = 0
            var samples: [Double] = []
            for step in 0..<40 {
                let started = CFAbsoluteTimeGetCurrent()
                if mode == "move" { window.setFrameOrigin(NSPoint(x: 100 + step * 2, y: 100 + step)) }
                else { window.setContentSize(NSSize(width: 1200 - step * 8, height: 840 - step * 3)) }
                host.layoutSubtreeIfNeeded(); host.displayIfNeeded()
                samples.append((CFAbsoluteTimeGetCurrent() - started) * 1000)
                try await Task.sleep(for: .milliseconds(16))
            }
            measurements[mode + "Ms"] = samples
            measurements[mode + "Publications"] = [Double(publications)]
            XCTAssertTrue(findEditor(host) === retained, "Window geometry must retain the actual text editor")
            if mode != "split-resize" {
                XCTAssertTrue(window.firstResponder === editor)
                XCTAssertEqual(editor.selectedRange(), NSRange(location: 7, length: 4))
            }
        }
        XCTAssertEqual(model.tab("motion")?.draft, "Retain this draft and selection")
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        try JSONSerialization.data(withJSONObject: measurements, options: [.prettyPrinted, .sortedKeys])
            .write(to: evidence.appendingPathComponent("native-window-motion-\(phase).json"))
    }

    @MainActor
    func testManyTabsKeepNavigationAndDraftsResponsive() async throws {
        let phase = ProcessInfo.processInfo.environment["NANOCODEX_PERFORMANCE_PHASE"] ?? "after"
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-many-tabs")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.runtime.requestOverride = { _, _ in .null }; model.workspaceFilter = .all
        model.tabs = (0..<120).map { WorkspaceTab(id: "tab-\($0)", title: "Conversation \($0 + 1)", draft: "Draft \($0)") }
        model.activeTabID = "tab-0"
        let content = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 800))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 800), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        try await Task.sleep(for: .milliseconds(250))
        var switches: [Double] = [], edits: [Double] = []
        for index in [119, 0, 60, 118, 1, 61, 117, 2, 62, 116, 3, 63] {
            let start = CFAbsoluteTimeGetCurrent()
            model.selectWorkspace(PaneNode(id: "tab-\(index)"))
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            switches.append((CFAbsoluteTimeGetCurrent() - start) * 1000)
            try await Task.sleep(for: .milliseconds(60))
            func editor(_ view: NSView) -> ComposerTextView? {
                if let found = view as? ComposerTextView { return found }
                for child in view.subviews { if let found = editor(child) { return found } }
                return nil
            }
            let input = try XCTUnwrap(editor(content))
            XCTAssertEqual(input.workspaceTabID, model.activeTabID)
            XCTAssertEqual(input.string, "Draft \(index)")
            window.makeFirstResponder(input)
            let began = CFAbsoluteTimeGetCurrent()
            input.insertText("!", replacementRange: NSRange(location: input.string.utf16.count, length: 0))
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            edits.append((CFAbsoluteTimeGetCurrent() - began) * 1000)
            XCTAssertEqual(model.activeTab?.draft, "Draft \(index)!")
        }
        let metrics: [String: Any] = ["phase": phase, "tabs": 120,
            "tabSwitchMedianMs": switches.sorted()[6], "tabSwitchP95Ms": switches.max()!,
            "editorUpdateMedianMs": edits.sorted()[6], "editorUpdateP95Ms": edits.max()!,
            "measurement": "Synchronous native selection/edit, layout and drawing; excludes the settling interval"]
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        try JSONSerialization.data(withJSONObject: metrics, options: [.prettyPrinted, .sortedKeys])
            .write(to: evidence.appendingPathComponent("native-many-tabs-\(phase).json"))
    }

    @MainActor
    func testBrowserHistoryPreservesLayoutsDraftsAndReviewState() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-browser-history-fixture")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.runtime.requestOverride = { _, _ in .null }
        model.workspaceFilter = .all
        model.tabs = [WorkspaceTab(id: "a", title: "First", draft: "First draft"),
                      WorkspaceTab(id: "b", title: "Second", draft: "Second draft"),
                      WorkspaceTab(id: "c", title: "Third"), WorkspaceTab(id: "d", title: "Fourth")]
        model.activeTabID = "a"
        defer { model.shutdown() }
        model.openBeside("b")
        let layout = try XCTUnwrap(model.activePaneLayout)
        let agents = model.tabs
        model.selectWorkspace(PaneNode(id: "c"))
        model.selectWorkspace(PaneNode(id: "d"))
        model.navigateHistory(back: true)
        XCTAssertEqual(model.activeTabID, "c")
        model.navigateHistory(back: true)
        XCTAssertEqual(model.activeTabID, "b")
        XCTAssertEqual(model.activePaneLayout, layout, "History restores the group instead of replacing a pane")
        XCTAssertEqual(model.tabs, agents, "History must not mark updates seen or change drafts")
        XCTAssertEqual(model.workspaceFocus, .navigation)
        model.navigateHistory(back: false)
        XCTAssertEqual(model.activeTabID, "c")
        model.closeTab("d")
        XCTAssertFalse(model.canGoForward, "Closed tabs are skipped without reopening them")
        model.navigateHistory(back: true)
        XCTAssertEqual(model.activeTabID, "b")
        model.select("a")
        XCTAssertFalse(model.canGoForward, "A new selection starts a new history branch")
        let count = model.backTabs.count
        model.composerFocused("a"); model.updateDraft("Continue typing", tabID: "a")
        XCTAssertEqual(model.backTabs.count, count, "Typing must not add duplicate navigation entries")
    }

    @MainActor
    func testTabOverviewRendersSplitGroupsAndDrafts() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-overview-fixture")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.runtime.requestOverride = { _, _ in .null }; model.workspaceFilter = .all
        model.tabs = [WorkspaceTab(id: "a", title: "Polish the desktop", draft: "Keep the remote screen open while we work"),
                      WorkspaceTab(id: "b", title: "Review mobile parity"),
                      WorkspaceTab(id: "c", title: "Explore the code", draft: "Check keyboard navigation")]
        model.activeTabID = "a"; model.openBeside("b")
        let content = NSHostingView(rootView: TabOverviewView().environmentObject(model).environment(\.colorScheme, .dark))
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 728, height: 568), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content; window.appearance = NSAppearance(named: .darkAqua); window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(model.browserTabs.count, 2)
        XCTAssertEqual(model.activeTab?.id, "b")
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
        let bitmap = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
        content.cacheDisplay(in: content.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: root.appendingPathComponent("native-tab-overview.png"))
    }

    @MainActor
    func testScreenPaneResizesWithoutReplacingConversation() async throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_SCREEN_FIXTURE"] == "1" else {
            throw XCTSkip("Run apple/NanocodexInboxUITests/fixtures/remote-screen.mjs")
        }
        let service = try RemoteService(origin: URL(string: "http://127.0.0.1:18965")!) { _ in }
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-screen-pane-test", remoteService: service)
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.runtime.requestOverride = { _, _ in .null }
        model.tabs = [WorkspaceTab(title: "Review the workspace"), WorkspaceTab(title: "Check the layout")]
        model.activeTabID = model.tabs[0].id; model.showingScreens = true
        XCTAssertNotNil(model.remoteService)
        XCTAssertFalse(model.showsOnboarding)
        let content = NSHostingView(rootView: ContentView().environmentObject(model)
            .environment(\.scenePhase, .active).environment(\.colorScheme, .dark).frame(width: 1280, height: 800))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1280, height: 800), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content
        window.appearance = NSAppearance(named: .darkAqua)
        window.setContentSize(NSSize(width: 1280, height: 800)); window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        // SwiftUI's accessibility nodes expose Objective-C accessors without
        // necessarily declaring conformance to the complete AppKit protocol.
        func attribute(_ value: NSObject, _ name: String) -> Any? {
            let selector = NSSelectorFromString(name)
            guard value.responds(to: selector) else { return nil }
            return value.perform(selector)?.takeUnretainedValue()
        }
        func find(_ value: Any, _ id: String) -> NSObject? {
            var visited: [ObjectIdentifier: NSObject] = [:]
            func visit(_ value: Any) -> NSObject? {
                guard let element = value as? NSObject else { return nil }
                let identity = ObjectIdentifier(element)
                guard visited[identity] == nil else { return nil }
                visited[identity] = element
                if attribute(element, "accessibilityIdentifier") as? String == id { return element }
                for child in (attribute(element, "accessibilityChildren") as? [Any]) ?? [] {
                    if let found = visit(child) { return found }
                }
                for child in (element as? NSView)?.subviews ?? [] {
                    if let found = visit(child) { return found }
                }
                return nil
            }
            return visit(value)
        }
        func editor(_ view: NSView) -> NSTextView? {
            if let found = view as? ComposerTextView { return found }
            for child in view.subviews { if let found = editor(child) { return found } }
            return nil
        }
        for _ in 0..<50 {
            if find(content, "remote-screen:fixture:desktop") != nil { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        let screen = try XCTUnwrap(find(content, "remote-screen:fixture:desktop"))
        let pressSelector = NSSelectorFromString("accessibilityPerformPress")
        XCTAssertTrue(screen.responds(to: pressSelector))
        typealias Press = @convention(c) (AnyObject, Selector) -> Bool
        let press = unsafeBitCast(screen.method(for: pressSelector), to: Press.self)
        XCTAssertTrue(press(screen, pressSelector))
        func canvas(_ view: NSView) -> MacRemoteCanvas? {
            if let found = view as? MacRemoteCanvas { return found }
            for child in view.subviews { if let found = canvas(child) { return found } }
            return nil
        }
        for _ in 0..<50 {
            if canvas(content)?.subviews.contains(where: { ($0 as? NSImageView)?.image != nil }) == true { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        let preview = try XCTUnwrap(canvas(content))
        XCTAssertTrue(preview.subviews.contains { ($0 as? NSImageView)?.image != nil }, "The fixture must deliver a decoded screen frame")
        let input = try XCTUnwrap(editor(content)); window.makeFirstResponder(input)
        input.insertText("Keep the preview beside this draft", replacementRange: NSRange(location: NSNotFound, length: 0))
        let draft = model.activeTab?.draft
        for position in ["left", "top"] {
            model.setTabPosition(position)
            try await Task.sleep(for: .milliseconds(200))
            XCTAssertTrue(canvas(content) === preview, "Tab orientation must retain the connected screen")
            XCTAssertTrue(editor(content) === input)
        }
        func split(_ view: NSView) -> NSSplitView? {
            if let split = view as? NSSplitView,
               let inputColumn = split.subviews.firstIndex(where: { editor($0) != nil }),
               let screenColumn = split.subviews.firstIndex(where: { canvas($0) != nil }),
               inputColumn != screenColumn { return split }
            for child in view.subviews { if let found = split(child) { return found } }
            return nil
        }
        let divider = try XCTUnwrap(split(content))
        divider.setPosition(560, ofDividerAt: 0)
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertTrue(editor(content) === input, "Resizing must retain the conversation editor")
        XCTAssertEqual(model.activeTab?.draft, draft)
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
        let bitmap = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
        content.cacheDisplay(in: content.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: root.appendingPathComponent("native-screen-pane.png"))
        divider.setPosition(940, ofDividerAt: 0)
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertTrue(canvas(content) === preview, "A narrow screen pane keeps the same viewer")
        XCTAssertTrue(editor(content) === input)
        XCTAssertGreaterThan(preview.bounds.width, 250)
        XCTAssertLessThan(preview.bounds.width, 360)
        let narrow = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
        content.cacheDisplay(in: content.bounds, to: narrow)
        try XCTUnwrap(narrow.representation(using: .png, properties: [:])).write(to: root.appendingPathComponent("native-screen-pane-narrow.png"))
        model.select(model.tabs[1].id)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(canvas(content) === preview, "Switching conversations keeps the screen pane mounted")
        model.showingScreens = false
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertNil(canvas(content))
        model.select(model.tabs[0].id)
        XCTAssertEqual(model.activeTab?.draft, draft)
    }

    func testStreamedAnswerSurvivesInterleavedEventsAndFinalization() throws {
        func output(_ cursor: String, _ type: String, _ text: String, agent: String? = nil) -> ManagedEvent {
            var data: [String: JSONValue] = ["type": .string("event"), "event": .object([
                "type": .string(type), "payload": .object(["text": .string(text), "phase": .string("final_answer"), "item_id": .string("answer")])])]
            if let agent { data["agent_id"] = .string(agent) }
            return .init(cursor: cursor, turnId: "turn", data: .object(data))
        }
        var events = [output("1", "assistant.delta", "Hello")]
        var cache = TimelineProjection()
        let first = try XCTUnwrap(cache.project(events).first)
        XCTAssertEqual(first.text, "Hello")
        XCTAssertTrue(first.streaming)
        events += [output("2", "assistant.delta", "Helper", agent: "helper"), output("3", "assistant.delta", " world")]
        let growing = cache.project(events).filter { $0.agent == nil }
        XCTAssertEqual(growing.map(\.text), ["Hello world"])
        XCTAssertEqual(growing.first?.id, first.id)
        let terminalOnly = events + [.init(cursor: "terminal", turnId: "turn", data: .object(["type": .string("turn_completed"), "final_message": .string("Hello world!")]))]
        XCTAssertEqual(projectTimeline(terminalOnly).filter { $0.agent == nil }.map(\.text), ["Hello world!"])
        events += [output("4", "assistant.delta", " update", agent: "helper"), output("5", "assistant.message", "Hello world!")]
        XCTAssertEqual(cache.project(events).first?.text, "Hello world!")
        XCTAssertFalse(try XCTUnwrap(cache.project(events).first).streaming)
        events += [output("6", "assistant.message", "Helper update", agent: "helper"), .init(cursor: "7", turnId: "turn", data: .object(["type": .string("turn_completed"), "final_message": .string("Hello world!")]))]
        let final = cache.project(events + events).filter { $0.agent == nil }
        XCTAssertEqual(final.map(\.text), ["Hello world!"])
        XCTAssertEqual(final.first?.id, first.id)
    }

    @MainActor
    func testRuntimePipeBackpressureKeepsMainQueueResponsive() async throws {
        let runtime = RuntimeClient(dataDirectory: "/tmp/nanocodex-pipe-backpressure")
        defer { runtime.stop() }
        try runtime.startForTesting(executable: URL(fileURLWithPath: "/usr/bin/python3"), arguments: ["-u", "-c", """
        import json, sys, time
        time.sleep(0.6)
        request = json.loads(sys.stdin.readline())
        print(json.dumps({"id": request["id"], "result": len(request["args"][0])}), flush=True)
        """])
        let started = CFAbsoluteTimeGetCurrent()
        let request = Task { try await runtime.request("large", [.string(String(repeating: "x", count: 2_000_000))]) }
        try await Task.sleep(for: .milliseconds(50))
        XCTAssertLessThan(CFAbsoluteTimeGetCurrent() - started, 0.4, "A full pipe must not block the main actor until the child reads")
        let reply = try await request.value
        XCTAssertEqual(reply, .number(2_000_000))
    }

    @MainActor
    func testPreparedReplyRetainsRevisionAndRebuildsCorrections() async throws {
        let runtime = RuntimeClient(dataDirectory: "/tmp/nanocodex-prepared-replies")
        defer { runtime.stop() }
        var text = "original", cursor = "1"
        runtime.requestOverride = { _, _ in
            .object(["id": .string("prepared"), "events": .array([.object(["cursor": .string("1"), "turnId": .string("turn"), "data": .object(["type": .string("turn_completed"), "final_message": .string(text)])])]), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings()), "cursor": .string(cursor)])
        }
        let first: ThreadSnapshot = try await runtime.call("openThread")
        cursor = "2"
        let replay: ThreadSnapshot = try await runtime.call("older")
        XCTAssertNotNil(first.presentation)
        XCTAssertEqual(first.presentation?.revision, replay.presentation?.revision)
        XCTAssertEqual(replay.cursor, "2", "Metadata updates survive reuse of prepared content")
        text = "corrected"
        let correction: ThreadSnapshot = try await runtime.call("older")
        XCTAssertNotEqual(replay.presentation?.revision, correction.presentation?.revision)
        XCTAssertEqual(correction.presentation?.messages.last?.text, "corrected")
        XCTAssertTrue(correction.presentation?.queue.finished.contains("turn") == true)
    }

    @MainActor
    func testTranscriptPreparationMainQueueLatency() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-main-thread-profile")
        model.runtime.requestOverride = { _, _ in .null }
        defer { model.shutdown() }
        var events: [ManagedEvent] = []
        for index in 0..<400 {
            let payload: JSONValue = .object(["call_id": .string("call-\(index)"), "name": .string("exec_command"), "arguments": .string("{\"cmd\":\"echo benchmark\"}"), "output": .string(String(repeating: "benchmark output ", count: 100))])
            events.append(ManagedEvent(cursor: "\(index * 2)", turnId: "turn-\(index)", data: .object(["type": .string("event"), "event": .object(["type": .string("tool.call"), "payload": payload])])))
            events.append(ManagedEvent(cursor: "\(index * 2 + 1)", turnId: "turn-\(index)", data: .object(["type": .string("event"), "event": .object(["type": .string("tool.result"), "payload": payload])])))
        }
        let snapshot: JSONValue = .object(["id": .string("profile"), "events": try .encoded(events), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings()), "cursor": .string("complete")])
        var frame = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("thread"), "thread": snapshot])]))
        frame.append(10)
        let start = CFAbsoluteTimeGetCurrent()
        var previous = start, largestGap = 0.0
        model.runtime.receiveAsynchronouslyForTesting(frame)
        while model.snapshots["profile"]?.cursor != "complete", CFAbsoluteTimeGetCurrent() - start < 10 {
            try await Task.sleep(for: .milliseconds(5))
            let now = CFAbsoluteTimeGetCurrent(); largestGap = max(largestGap, now - previous); previous = now
        }
        XCTAssertEqual(model.snapshots["profile"]?.cursor, "complete")
        XCTAssertEqual(model.messages["profile"]?.count, 400)
        print("MAIN_QUEUE_PROFILE bytes=\(frame.count) total_ms=\((CFAbsoluteTimeGetCurrent() - start) * 1000) largest_gap_ms=\(largestGap * 1000)")
    }

    @MainActor
    func testWorkspaceKeyboardZoomAndCancelledDocking() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-keyboard-zoom-fixture")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.workspaceFilter = .all
        model.runtime.requestOverride = { _, _ in .null }
        model.tabs = [WorkspaceTab(id: "original", title: "Keyboard fixture", draft: "Keep this draft")]
        model.activeTabID = "original"
        let content = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1440, height: 1000))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1440, height: 1000), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        func key(_ code: UInt16, _ text: String, _ modifiers: NSEvent.ModifierFlags = []) throws {
            let event = try XCTUnwrap(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: modifiers, timestamp: 0, windowNumber: window.windowNumber, context: nil, characters: text, charactersIgnoringModifiers: text, isARepeat: false, keyCode: code))
            window.firstResponder?.keyDown(with: event)
        }
        func editors(_ view: NSView) -> [ComposerTextView] { (view as? ComposerTextView).map { [$0] } ?? view.subviews.flatMap(editors) }
        func surface(_ view: NSView) -> AgentSplitSurface? {
            if let found = view as? AgentSplitSurface { return found }
            for child in view.subviews { if let found = surface(child) { return found } }
            return nil
        }
        try await Task.sleep(for: .milliseconds(250))
        model.focusComposer()
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(window.firstResponder is ComposerTextView)
        try key(53, "\u{1b}"); try key(9, "v"); try key(4, "h")
        XCTAssertEqual(model.tabs.count, 3, "Rapid Escape/v/h opens both split directions without typing into a draft")
        XCTAssertEqual(model.workspaceFocus, .navigation)
        let right = model.tabs[1].id, below = model.tabs[2].id
        XCTAssertEqual(model.activePaneLayout?.children[1].axis, "vertical")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertTrue(window.firstResponder is WorkspaceKeyboardView)
        try key(126, ""); XCTAssertEqual(model.activeTabID, right)
        try key(4, "h", .control); XCTAssertEqual(model.activeTabID, "original")
        try key(37, "l", .control); XCTAssertEqual(model.activeTabID, right)
        try key(38, "j", .control); XCTAssertEqual(model.activeTabID, below)
        let initial = try XCTUnwrap(model.activePaneLayout?.nearestSplit(to: below, axis: "vertical"))
        try key(40, "K", .shift)
        XCTAssertEqual(model.activePaneLayout?.nearestSplit(to: below, axis: "vertical")?.fraction, initial.fraction - 0.05)
        try key(6, "z"); XCTAssertFalse(model.isTiled)
        try key(6, "z"); XCTAssertTrue(model.isTiled)
        try await Task.sleep(for: .milliseconds(300))
        try key(36, "\r"); try key(9, "v"); try key(4, "h"); try key(7, "x")
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(model.tab(below)?.draft, "vhx", "Writing keeps unmodified pane shortcut letters")
        XCTAssertEqual(model.tab("original")?.draft, "Keep this draft")
        let split = try XCTUnwrap(surface(content)), hosts = split.hosts
        for value in [3, -6, 20, -20] {
            model.changeZoom(value)
            try await Task.sleep(for: .milliseconds(120)); content.layoutSubtreeIfNeeded()
            XCTAssertTrue((0.75...1.5).contains(model.workspaceZoom))
            XCTAssertTrue(split === surface(content))
            for (id, host) in hosts { XCTAssertTrue(host === split.hosts[id]) }
            XCTAssertEqual(editors(content).first(where: { $0.workspaceTabID == below })?.string, "vhx")
        }
        model.resetZoom(); XCTAssertEqual(model.workspaceZoom, 1)
        let keyboard = try XCTUnwrap(WorkspaceKeyboardView.find(in: content))
        for (text, flags, expected) in [("=", NSEvent.ModifierFlags.command, 1.1), ("+", [.command, .shift], 1.2), ("-", .command, 1.1), ("0", .command, 1.0)] {
            let event = try XCTUnwrap(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: window.windowNumber, context: nil, characters: text, charactersIgnoringModifiers: text, isARepeat: false, keyCode: 24))
            XCTAssertTrue(keyboard.handleZoomCommand(event))
            XCTAssertEqual(model.workspaceZoom, expected, accuracy: 0.001)
        }
        model.draggingPaneID = right; model.paneDropTarget = below; model.paneDropEdge = .top
        XCTAssertEqual(model.paneDropPreview(for: below), .top)
        XCTAssertNil(model.paneDropPreview(for: right))
        model.cancelPaneDrag(); XCTAssertNil(model.paneDropPreview(for: below))
        model.draggingPaneID = right; XCTAssertNil(model.paneDropPreview(for: below), "A fresh drag cannot resurrect the previous target")
        model.paneDropTarget = below; model.paneDropEdge = .bottom
        NotificationCenter.default.post(name: NSApplication.didResignActiveNotification, object: NSApp)
        try await Task.sleep(for: .milliseconds(50))
        XCTAssertNil(model.draggingPaneID)
        XCTAssertNil(model.paneDropPreview(for: below))
        try key(53, "\u{1b}"); try key(7, "x")
        XCTAssertNil(model.tab(below))
        XCTAssertEqual(model.tabs.count, 2)
        XCTAssertEqual(model.workspaceFocus, .navigation)
        XCTAssertEqual(model.tab("original")?.draft, "Keep this draft")
        model.reopenTab()
        XCTAssertEqual(model.tab(below)?.draft, "vhx", "Closing a pane keeps its draft recoverable")
    }

    @MainActor
    func testBrowserSplitLayoutsPersistReopenAndRetainEditors() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-browser-fixture")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.workspaceFilter = .all
        var saved: JSONValue?
        model.runtime.requestOverride = { method, args in if method == "saveLayout" { saved = args.first }; return .null }
        model.tabs = [WorkspaceTab(id: "one", title: "Plan the release", draft: "First draft"), WorkspaceTab(id: "two", title: "Review the changes", draft: "Second draft"), WorkspaceTab(id: "three", title: "Check the result", draft: "Third draft")]
        model.activeTabID = "one"; model.openBeside("two")
        model.splitAxis = "vertical"; model.openBeside("three")
        let tree = try XCTUnwrap(model.activePaneLayout)
        XCTAssertEqual(tree.leaves, ["one", "two", "three"])
        XCTAssertEqual(tree.children[1].axis, "vertical")
        XCTAssertEqual(model.browserTabs.count, 1)
        let content = NSHostingView(rootView: ContentView().environmentObject(model).preferredColorScheme(.dark).frame(width: 1440, height: 1000))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1440, height: 1000), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        try await Task.sleep(for: .milliseconds(200)); content.layoutSubtreeIfNeeded()
        func find(_ view: NSView) -> AgentSplitSurface? {
            if let found = view as? AgentSplitSurface { return found }
            for child in view.subviews { if let found = find(child) { return found } }
            return nil
        }
        let surface = try XCTUnwrap(find(content))
        let hosts = surface.hosts
        let initialWidth = try XCTUnwrap(hosts["one"]).frame.width
        var timings: [Double] = []
        for index in 0..<30 {
            let began = CFAbsoluteTimeGetCurrent()
            surface.dragSplit(tree.id, fraction: 0.4 + Double(index) / 200, finished: index == 29)
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            timings.append((CFAbsoluteTimeGetCurrent() - began) * 1000)
        }
        XCTAssertNotEqual(hosts["one"]?.frame.width, initialWidth)
        for id in tree.leaves { XCTAssertTrue(hosts[id] === surface.hosts[id], "Resizing retains each native editor and viewport") }
        XCTAssertGreaterThan(try XCTUnwrap(hosts["three"]).frame.minY, try XCTUnwrap(hosts["two"]).frame.minY)
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        let bitmap = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
        content.cacheDisplay(in: content.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-browser-splits.png"))
        content.rootView = ContentView().environmentObject(model).preferredColorScheme(.dark).frame(width: 820, height: 600)
        window.setContentSize(NSSize(width: 820, height: 600))
        try await Task.sleep(for: .milliseconds(100)); content.layoutSubtreeIfNeeded()
        let narrow = try XCTUnwrap(find(content))
        XCTAssertGreaterThanOrEqual(narrow.frame.width, 728)
        XCTAssertGreaterThanOrEqual(narrow.frame.height, 608)
        XCTAssertTrue(narrow.hosts.values.allSatisfy { $0.frame.width >= 360 && $0.frame.height >= 300 })
        XCTAssertTrue(narrow.visibleRect.contains(try XCTUnwrap(narrow.hosts["three"]).frame), "Shrinking the window keeps the active pane visible")
        let narrowBitmap = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
        content.cacheDisplay(in: content.bounds, to: narrowBitmap)
        try XCTUnwrap(narrowBitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-browser-splits-narrow.png"))
        model.newTab(); let independent = model.activeTabID
        XCTAssertFalse(model.isTiled); XCTAssertEqual(model.browserTabs.count, 2)
        model.selectWorkspace(try XCTUnwrap(model.browserTabs.first { $0.id == tree.id }))
        XCTAssertEqual(model.activePaneLayout?.leaves, tree.leaves)
        XCTAssertEqual(model.activeTabID, "three", "Returning to a layout restores its last focused pane")
        XCTAssertEqual(model.tab("one")?.draft, "First draft")
        model.moveTab(independent, before: "one")
        XCTAssertEqual(model.browserTabs.first?.id, independent)
        model.moveTab("one", before: independent)
        XCTAssertEqual(model.browserTabs.first?.id, tree.id)
        model.closeWorkspace(tree); XCTAssertNil(model.tab("one")); XCTAssertNotNil(model.tab(independent))
        model.reopenTab(); XCTAssertEqual(model.activePaneLayout?.leaves, tree.leaves)
        XCTAssertEqual(model.tab("two")?.draft, "Second draft")
        model.resizeSplit(tree.id, fraction: 0.63)
        try await Task.sleep(for: .milliseconds(450))
        let layout = try XCTUnwrap(saved).decode(TabLayout.self)
        XCTAssertEqual(layout.paneLayouts?.first?.fraction, 0.63)
        let restored = AppModel(runtimeDirectory: "/tmp/nanocodex-browser-restored")
        restored.runtime.requestOverride = { _, _ in .null }; defer { restored.shutdown() }
        guard case .object(var state) = Self.connectedState else { return XCTFail("Missing fixture") }
        state["layout"] = saved
        var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("state"), "state": .object(state)])])); wire.append(10)
        restored.runtime.receiveForTesting(wire)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(restored.activePaneLayout, model.activePaneLayout)
        XCTAssertEqual(restored.tab("three")?.draft, "Third draft")
        let status = HandStatusItem(model: model, openMainWindow: {})
        XCTAssertTrue(status.statusItemVisible)
        XCTAssertEqual(status.statusItemTitle, "")
        XCTAssertLessThanOrEqual(status.statusItemSize.width, 32)
        let metrics: [String: Any] = ["timestamp": ISO8601DateFormatter().string(from: Date()), "dividerResizeMedianMs": timings.sorted()[15], "dividerResizeP95Ms": timings.sorted()[28], "menuBarItemWidthPt": status.statusItemSize.width, "network": "none; native window, editors, split layout and persistence"]
        try JSONSerialization.data(withJSONObject: metrics, options: [.prettyPrinted, .sortedKeys]).write(to: evidence.appendingPathComponent("native-browser-layout-metrics.json"))
    }

    @MainActor
    func testPaneDockingPreservesIdentitiesDraftsAndDeepLayouts() throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-dock-fixture")
        model.runtime.requestOverride = { _, _ in .null }; defer { model.shutdown() }
        model.state = try Self.connectedState.decode(DesktopState.self)
        model.workspaceFilter = .all
        model.tabs = (0..<80).map { WorkspaceTab(id: "pane-\($0)", title: "Agent \($0)", draft: "Draft \($0)") }
        model.activeTabID = "pane-0"
        model.pending = [PendingMessage(id: "queued", tabID: "pane-79", agentID: "durable-agent", text: "Preserve this follow-up", predecessor: "running-turn", phase: .queued)]
        let pendingBefore = model.pending
        for index in 1..<80 {
            model.splitAxis = index.isMultiple(of: 2) ? "vertical" : "horizontal"
            model.openBeside("pane-\(index)")
        }
        let original = try XCTUnwrap(model.activePaneLayout)
        XCTAssertEqual(original.leaves.count, 80)
        let decoded = try JSONDecoder().decode(PaneNode.self, from: JSONEncoder().encode(original))
        XCTAssertEqual(decoded, original)
        for edge in [PaneDock.left, .top, .right, .bottom, .center] {
            model.dockPane("pane-79", at: "pane-0", edge: edge)
            XCTAssertEqual(Set(try XCTUnwrap(model.activePaneLayout).leaves), Set(model.tabs.map(\.id)))
            XCTAssertEqual(model.paneLayouts.flatMap(\.leaves).count, 80)
            XCTAssertEqual(model.activeTabID, "pane-79")
            for index in 0..<80 { XCTAssertEqual(model.tab("pane-\(index)")?.draft, "Draft \(index)") }
        }
        model.separatePane("pane-79")
        XCTAssertFalse(model.isTiled)
        XCTAssertEqual(model.browserTabs.count, 2)
        model.dockPane("pane-79", at: "pane-0", edge: .top)
        XCTAssertTrue(model.isTiled); XCTAssertEqual(model.browserTabs.count, 1)
        XCTAssertEqual(model.pending, pendingBefore)
        let beforeInvalid = model.paneLayouts
        model.dockPane("pane-0", at: "pane-0", edge: .left)
        model.dockPane("missing", at: "pane-0", edge: .right)
        XCTAssertEqual(model.paneLayouts, beforeInvalid)
        XCTAssertEqual(PaneDock.destination(at: CGPoint(x: 2, y: 150), size: CGSize(width: 400, height: 300)), .left)
        XCTAssertEqual(PaneDock.destination(at: CGPoint(x: 200, y: 2), size: CGSize(width: 400, height: 300)), .top)
        XCTAssertEqual(PaneDock.destination(at: CGPoint(x: 398, y: 150), size: CGSize(width: 400, height: 300)), .right)
        XCTAssertEqual(PaneDock.destination(at: CGPoint(x: 200, y: 298), size: CGSize(width: 400, height: 300)), .bottom)
        XCTAssertEqual(PaneDock.destination(at: CGPoint(x: 200, y: 150), size: CGSize(width: 400, height: 300)), .center)
    }

    @MainActor
    func testPaneMotionCommitsSizeOnceAndLiveResizeCancelsMotion() throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-pane-motion-fixture")
        defer { model.shutdown() }
        let surface = AgentSplitSurface(model: model)
        let host = NSHostingView(rootView: AnyView(Text("A stable live editor")))
        host.sizingOptions = []; host.wantsLayer = true
        surface.addSubview(host)
        surface.placeHost(host, in: NSRect(x: 0, y: 0, width: 800, height: 600), animated: false)
        let final = NSRect(x: 410, y: 0, width: 390, height: 600)
        surface.placeHost(host, in: final, animated: true)
        XCTAssertEqual(host.frame, final, "Content adopts its final size before the transition starts")
        XCTAssertEqual(host.bounds.size, final.size)
        let motion = try XCTUnwrap(host.layer?.animation(forKey: "pane-position") as? CABasicAnimation)
        XCTAssertEqual(motion.keyPath, "position")
        XCTAssertNil(host.layer?.animation(forKey: "bounds"), "Animation must not reflow live text at every frame")
        surface.placeHost(host, in: final, animated: false)
        XCTAssertNotNil(host.layer?.animation(forKey: "pane-position"), "Redundant layout must not snap an active transition to its endpoint")
        let resized = NSRect(x: 380, y: 0, width: 420, height: 600)
        surface.placeHost(host, in: resized, animated: false)
        XCTAssertEqual(host.frame, resized)
        XCTAssertNil(host.layer?.animation(forKey: "pane-position"), "A real divider drag immediately takes ownership of geometry")
    }

    @MainActor
    func testNativeDockingRetainsEditorsAndRendersGlass() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-glass-fixture")
        model.runtime.requestOverride = { _, _ in .null }
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.workspaceFilter = .all
        model.tabs = [WorkspaceTab(id: "plan", title: "Plan the release", draft: "Review the rollout plan"),
                      WorkspaceTab(id: "build", title: "Build the workspace", draft: "Keep the editor state"),
                      WorkspaceTab(id: "review", title: "Review the changes", draft: "Inspect the diff"),
                      WorkspaceTab(id: "verify", title: "Verify on devices", draft: "Check the native interactions")]
        model.activeTabID = "plan"
        model.openBeside("build")
        model.dockPane("review", at: "plan", edge: .bottom)
        model.dockPane("verify", at: "build", edge: .bottom)
        let content = NSHostingView(rootView: ContentView().environmentObject(model).preferredColorScheme(.light).frame(width: 1440, height: 1000))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1440, height: 1000), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        try await Task.sleep(for: .milliseconds(300)); content.layoutSubtreeIfNeeded()
        func find(_ view: NSView) -> AgentSplitSurface? {
            if let found = view as? AgentSplitSurface { return found }
            for child in view.subviews { if let found = find(child) { return found } }
            return nil
        }
        let surface = try XCTUnwrap(find(content)), hosts = surface.hosts
        model.dockPane("verify", at: "plan", edge: .center)
        try await Task.sleep(for: .milliseconds(350)); content.layoutSubtreeIfNeeded()
        for (id, host) in hosts { XCTAssertTrue(host === surface.hosts[id], "Swapping keeps live editor identity") }
        model.dockPane("verify", at: "build", edge: .top)
        try await Task.sleep(for: .milliseconds(350)); content.layoutSubtreeIfNeeded()
        for (id, host) in hosts { XCTAssertTrue(host === surface.hosts[id], "Redocking keeps live editor identity") }
        XCTAssertLessThan(try XCTUnwrap(surface.hosts["verify"]).frame.minY, try XCTUnwrap(surface.hosts["build"]).frame.minY)
        let root = try XCTUnwrap(model.activePaneLayout), divider = try XCTUnwrap(surface.dividers[root.id])
        XCTAssertTrue(divider.accessibilityPerformIncrement())
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(try XCTUnwrap(model.activePaneLayout).fraction, 0.55, accuracy: 0.001)
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        for (theme, name) in [(ColorScheme.light, "light"), (.dark, "dark")] {
            window.appearance = NSAppearance(named: theme == .dark ? .darkAqua : .aqua)
            content.rootView = ContentView().environmentObject(model).preferredColorScheme(theme).frame(width: 1440, height: 1000)
            try await Task.sleep(for: .milliseconds(250)); content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            let bitmap = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
            content.cacheDisplay(in: content.bounds, to: bitmap)
            try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-glass-splits-\(name).png"))
        }
    }

    @MainActor
    func testAppearanceChangesRetainNativeComposerAndDraft() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-accessible-glass-fixture")
        model.runtime.requestOverride = { _, _ in .null }
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [WorkspaceTab(id: "accessible", title: "Accessible workspace", draft: "Keep this draft")]
        model.activeTabID = "accessible"; model.workspaceFilter = .all
        func page(_ theme: ColorScheme) -> some View {
            WorkspacePane(tab: model.tabs[0]).environmentObject(model).environment(\.workspaceTabID, "accessible")
                .environment(\.colorScheme, theme)
                .frame(width: 820, height: 600)
        }
        let host = NSHostingView(rootView: page(.light)); host.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 820, height: 600), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        try await Task.sleep(for: .milliseconds(150))
        func findEditor(_ view: NSView) -> ComposerTextView? {
            if let found = view as? ComposerTextView { return found }
            for child in view.subviews { if let found = findEditor(child) { return found } }
            return nil
        }
        let editor = try XCTUnwrap(findEditor(host))
        window.makeFirstResponder(editor)
        editor.setSelectedRange(NSRange(location: 5, length: 4))
        for (theme, name) in [(ColorScheme.dark, "dark"), (.light, "light")] {
            window.appearance = NSAppearance(named: theme == .dark ? .darkAqua : .aqua)
            host.rootView = page(theme)
            try await Task.sleep(for: .milliseconds(120)); host.layoutSubtreeIfNeeded()
            XCTAssertTrue(findEditor(host) === editor, "Appearance changes preserve AppKit editor identity")
            XCTAssertTrue(window.firstResponder === editor)
            XCTAssertEqual(editor.selectedRange(), NSRange(location: 5, length: 4))
            XCTAssertEqual(editor.string, "Keep this draft")
            let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
            try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
            let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
            host.cacheDisplay(in: host.bounds, to: bitmap)
            try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-appearance-\(name).png"))
        }
        editor.insertText("that", replacementRange: editor.selectedRange())
        XCTAssertEqual(model.activeTab?.draft, "Keep that draft")
    }

    @MainActor
    func testNativeMenuBarAfterWindowClose() async throws {
        guard ProcessInfo.processInfo.environment["NANOCODEX_DESKTOP_MENU_BAR_LIVE"] == "1" else {
            throw XCTSkip("Opt-in visible menu-bar check requires an unlocked macOS desktop")
        }
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-menu-bar-fixture")
        model.runtime.requestOverride = { _, _ in .null }
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        defer { model.shutdown() }
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = NSHostingView(rootView: ContentView().environmentObject(model))
        window.makeKeyAndOrderFront(nil)
        let status = HandStatusItem(model: model, openMainWindow: { window.makeKeyAndOrderFront(nil) })
        defer { status.dismiss(); window.close() }
        try await Task.sleep(for: .milliseconds(250))
        window.close()
        let started = CFAbsoluteTimeGetCurrent()
        status.show()
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertTrue(status.controlPanelIsShown, "The control panel stays available with the workspace window closed")
        print("BACKGROUND_PANEL visible=\(status.controlPanelIsShown) observed_ms=\((CFAbsoluteTimeGetCurrent() - started) * 1000) width_pt=\(status.statusItemSize.width)")
        status.dismiss(); XCTAssertFalse(status.controlPanelIsShown)
    }

    func testActivityPreservesAnswerPhasesAndStableTurnIdentity() {
        func event(_ cursor: String, _ phase: String, _ text: String) -> ManagedEvent {
            .init(cursor: cursor, turnId: "turn", data: .object(["type": .string("event"), "event": .object([
                "type": .string("assistant.delta"), "payload": .object(["text": .string(text), "phase": .string(phase), "item_id": .string(phase)])])]))
        }
        let rows = projectTimeline([event("1", "commentary", "Checking files."), event("2", "final_answer", "The answer.")])
        XCTAssertEqual(rows.map(\.text), ["Checking files.", "The answer."])
        let grouped = NativeConversationItem.group(rows, working: true)
        XCTAssertEqual(grouped.count, 2)
        XCTAssertEqual(grouped.first?.activity.map(\.text), ["Checking files."])
        XCTAssertEqual(grouped.last?.message?.text, "The answer.")
        XCTAssertFalse(grouped.first?.isRunning ?? true)
        let child = MessageEntry(id: "helper", turnId: "turn", kind: .assistant, text: "Helper update", agent: "helper")
        let reasoning = MessageEntry(id: "thinking", turnId: "turn", kind: .reasoning, text: "Reviewing evidence")
        let live = NativeConversationItem.group([rows[0], child, reasoning], working: true)
        XCTAssertEqual(live.count, 1)
        XCTAssertEqual(live[0].id, grouped[0].id)
        XCTAssertEqual(live[0].activity.count, 3)
        XCTAssertTrue(live[0].isRunning)
        let legacy = MessageEntry(id: "legacy", turnId: "old", kind: .assistant, text: "Older answer")
        XCTAssertEqual(NativeConversationItem.group([legacy], working: false).first?.message?.text, "Older answer")
    }

    @MainActor
    func testNativeActivityDisclosureKeepsStreamingCompact() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-activity-" + UUID().uuidString)
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.workspaceFilter = .all
        model.runtime.requestOverride = { _, _ in .null }
        let tab = WorkspaceTab(id: "activity", threadId: "activity-thread", title: "Review the reconnect flow")
        model.tabs = [tab]; model.activeTabID = tab.id
        model.snapshots["activity-thread"] = ThreadSnapshot(id: "activity-thread", events: [], hasMore: false, connected: true, activeTurns: ["turn"], settings: AgentSettings())
        model.messages["activity-thread"] = [.init(id: "user", turnId: "turn", kind: .user, text: "Review the reconnect flow and check the result."),
                                             .init(id: "note", turnId: "turn", kind: .reasoning, text: "Checking the request lifecycle.", streaming: true)]
        let host = NSHostingView(rootView: ContentView().environmentObject(model).preferredColorScheme(.light).frame(width: 1100, height: 820))
        host.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1100, height: 820), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        func scrolls(_ view: NSView) -> [NSScrollView] { ((view as? NSScrollView).map { [$0] } ?? []) + view.subviews.flatMap(scrolls) }
        try await Task.sleep(for: .milliseconds(250))
        let transcript = try XCTUnwrap(scrolls(host).first { $0.bounds.width > 500 && $0.bounds.height > 300 })
        let initialHeight = transcript.documentView?.bounds.height ?? 0
        for index in 0..<12 {
            model.messages["activity-thread"]?.append(.init(id: "tool-\(index)", turnId: "turn", kind: .tool, text: "{\"cmd\":\"swift test --package-path apple/InboxCore\"}", name: "exec_command", output: "Reconnect checks passed.", status: "completed"))
            model.messages["activity-thread"]?.append(.init(id: "reason-\(index)", turnId: "turn", kind: .reasoning, text: "Compared the boundary and retained the request identity.", streaming: true))
        }
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(transcript.documentView?.bounds.height ?? 0, initialHeight, accuracy: 2, "Closed activity cannot grow the transcript as steps arrive")
        model.messages["activity-thread"]?.append(.init(id: "answer", turnId: "turn", kind: .assistant, text: "The reconnect flow keeps the original request identity. **All checks passed.**", phase: "final_answer"))
        model.snapshots["activity-thread"]?.activeTurns = []
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        func capture(_ name: String) throws {
            window.setContentSize(NSSize(width: 1100, height: 820))
            host.setFrameSize(NSSize(width: 1100, height: 820))
            host.layoutSubtreeIfNeeded(); host.displayIfNeeded()
            let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds)); host.cacheDisplay(in: host.bounds, to: bitmap)
            try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent(name))
        }
        try await Task.sleep(for: .milliseconds(200)); try capture("native-activity-collapsed.png")
        let collapsedHeight = transcript.documentView?.bounds.height ?? 0
        model.expandedMessages[tab.id] = ["activity-turn"]
        try await Task.sleep(for: .milliseconds(200)); try capture("native-activity-timeline.png")
        model.expandedMessages[tab.id]?.insert("tool-0")
        try await Task.sleep(for: .milliseconds(200)); try capture("native-activity-details.png")
        model.expandedMessages[tab.id] = []
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(transcript.documentView?.bounds.height ?? 0, collapsedHeight, accuracy: 2, "Collapsing activity restores the height including the completed answer")
    }

    @MainActor
    func testNativeTaskStatusClearsReviewedFailureAndShowsCurrentWork() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/native-task-status-" + UUID().uuidString)
        model.state = try Self.connectedState.decode(DesktopState.self)
        model.workspaceFilter = .all
        model.runtime.requestOverride = { _, _ in .null }
        model.tabs = [WorkspaceTab(id: "task", threadId: "thread", title: "Task status lifecycle")]
        model.activeTabID = "task"
        model.snapshots["thread"] = ThreadSnapshot(id: "thread", events: [
            ManagedEvent(cursor: "10", turnId: "old", data: .object(["type": .string("turn_failed")]))
        ], hasMore: false, connected: true, activeTurns: [], settings: AgentSettings())
        let panel = HandControlPanel(model: model, openMainWindow: {})
        let host = NSHostingView(rootView: panel.frame(width: 720, height: 560))
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 720, height: 560), styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        func capture(_ name: String) async throws {
            try await Task.sleep(for: .milliseconds(100))
            host.layoutSubtreeIfNeeded(); host.displayIfNeeded()
            let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds)); host.cacheDisplay(in: host.bounds, to: bitmap)
            try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent(name))
        }
        XCTAssertEqual(panel.agentStatus(model.tabs[0]), "Needs attention")
        XCTAssertEqual(model.attentionCount, 1)
        model.review("task", seen: true)
        try await capture("native-task-reviewed.png")
        XCTAssertEqual(model.attentionCount, 0)
        XCTAssertFalse(model.hasAttentionError(model.tabs[0]))
        XCTAssertEqual(panel.agentStatus(model.tabs[0]), "Idle", "Reviewed historical failure must not keep a task flagged")
        model.snapshots["thread"]?.activeTurns = ["new"]
        try await capture("native-task-running.png")
        XCTAssertFalse(model.hasAttentionError(model.tabs[0]))
        XCTAssertEqual(panel.agentStatus(model.tabs[0]), "Running", "An old failure must not override a new active turn")
        model.snapshots["thread"]?.activeTurns = []
        model.snapshots["thread"]?.events.append(ManagedEvent(cursor: "12", turnId: "new", data: .object(["type": .string("turn_failed")])) )
        XCTAssertEqual(panel.agentStatus(model.tabs[0]), "Needs attention", "A new failure still needs review")
        XCTAssertEqual(model.attentionCount, 1)
    }

    func testInboxReviewUsesExactCursorsAndOnlyNewUpdatesReturn() {
        var tab = WorkspaceTab(seenCursor: "9007199254740992")
        let ready = WorkspaceUpdate(cursor: "9007199254740993", running: false, checked: true, failed: false, completed: true)
        XCTAssertTrue(ready.needsAttention(tab))
        XCTAssertTrue(ready.isInInbox(tab))
        tab.deferredCursor = ready.cursor
        XCTAssertFalse(ready.isInInbox(tab))
        XCTAssertTrue(ready.needsAttention(tab), "Later must not mark an update seen")
        tab.seenCursor = ready.cursor
        XCTAssertFalse(ready.needsAttention(tab))
        let newer = WorkspaceUpdate(cursor: "9007199254740994", running: true, checked: true, failed: false, completed: false)
        XCTAssertTrue(newer.isInInbox(tab))
        XCTAssertTrue(cursorIsNewer("1000000000000000000000000000000000000000", than: "999999999999999999999999999999999999999"))
        XCTAssertFalse(cursorIsNewer("00012", than: "12"))
    }

    @MainActor
    func testUnsentTabKeepsSelectedSettingsAcrossRelaunch() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/native-draft-settings-" + UUID().uuidString)
        model.tabs = [WorkspaceTab(id: "draft"), WorkspaceTab(id: "other")]
        model.activeTabID = "draft"
        var saved: JSONValue = .null
        model.runtime.requestOverride = { method, args in
            if method == "saveLayout" { saved = args[0] }
            return .null
        }
        model.updateDraft("Keep this unsent draft")
        model.changeSettings(tabID: "draft") {
            $0.selectModel("gpt-5.6-luna"); $0.thinking = "low"; $0.fast_mode = true
        }
        await model.prepareToQuit()
        let restored = AppModel(runtimeDirectory: "/tmp/native-draft-settings-restored-" + UUID().uuidString)
        restored.runtime.requestOverride = { _, _ in .null }
        defer { restored.shutdown() }
        guard case .object(var state) = Self.connectedState else { return XCTFail("Missing state fixture") }
        state["layout"] = saved
        var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("state"), "state": .object(state)])])); wire.append(10)
        restored.runtime.receiveForTesting(wire)
        for _ in 0..<30 where restored.activeTabID != "draft" { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertEqual(restored.activeTab?.draft, "Keep this unsent draft")
        XCTAssertEqual(restored.settingsForTab("draft"), AgentSettings(model: "gpt-5.6-luna", thinking: "low", fast_mode: true))
        XCTAssertEqual(restored.settingsForTab("other"), AgentSettings(), "A draft's selection must not alter another tab's defaults")
        restored.closeTab("draft"); restored.reopenTab()
        XCTAssertEqual(restored.settingsForTab("draft").model, "gpt-5.6-luna")
    }

    @MainActor
    func testBrowserRestoresReviewedConversationWithoutHiddenFiltering() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-browser-default-filter")
        defer { model.shutdown() }
        let tab = WorkspaceTab(id: "read-tab", threadId: "read-thread", seenCursor: "1")
        guard case .object(var state) = Self.connectedState else { return XCTFail("Missing state fixture") }
        state["layout"] = try .encoded(TabLayout(tabs: [tab], activeTabId: tab.id))
        model.runtime.requestOverride = { method, args in
            if method == "openThread" {
                return .object(["id": args[0], "events": .array([.object(["cursor": .string("1"), "turnId": .string("turn"), "data": .object(["type": .string("turn_completed")])])]), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())])
            }
            return .null
        }
        var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("state"), "state": .object(state)])])); wire.append(10)
        model.runtime.receiveForTesting(wire)
        for _ in 0..<30 where model.snapshots.isEmpty { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertEqual(model.workspaceFilter, .all)
        XCTAssertEqual(model.activeTabID, tab.id)
        XCTAssertEqual(model.canvasTabs.map(\.id), [tab.id])
        XCTAssertFalse(model.update(for: tab).needsAttention(tab))
        model.setFilter(.inbox)
        XCTAssertTrue(model.canvasTabs.isEmpty, "Inbox filtering remains an explicit action")
    }

    @MainActor
    func testRestoredTiledSelectionIgnoresInboxFiltering() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-restored-selection")
        model.workspaceFilter = .inbox
        let tabs = [WorkspaceTab(id: "outside"), WorkspaceTab(id: "one", threadId: "thread-one", deferredCursor: "1"), WorkspaceTab(id: "two", threadId: "thread-two", seenCursor: "1", deferredCursor: "1")]
        guard case .object(var state) = Self.connectedState else { return XCTFail("Missing state fixture") }
        state["layout"] = try .encoded(TabLayout(tabs: tabs, activeTabId: "one", workspaceMode: "tiles", tiledTabIDs: ["one", "two"]))
        var opened = Set<String>()
        model.runtime.requestOverride = { method, args in
            if method == "openThread" {
                opened.insert(args[0].string)
                return .object(["id": args[0], "events": .array([.object(["cursor": .string("1"), "turnId": .string("turn"), "data": .object(["type": .string("turn_completed")])])]), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())])
            }
            return .null
        }
        defer { model.shutdown() }
        var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("state"), "state": .object(state)])])); wire.append(10)
        model.runtime.receiveForTesting(wire)
        for _ in 0..<30 where model.snapshots.count < 2 { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertEqual(opened.count, 2)
        XCTAssertEqual(model.visibleTabs.map(\.id), ["outside"], "Inbox excludes the seen and deferred agents")
        XCTAssertEqual(model.canvasTabs.map(\.id), ["one", "two"])
        XCTAssertEqual(model.activeTabID, "one", "The restored tile must stay active even when it is outside Inbox")
    }

    @MainActor
    func testNativeTiledInboxKeepsEditorsActionsAndReviewIndependent() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-tiles")
        model.workspaceFilter = .inbox
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [
            WorkspaceTab(id: "design", threadId: "thread-design", title: "Refine the Inbox", draft: "Design draft"),
            WorkspaceTab(id: "research", threadId: "thread-research", title: "Research for the next release", draft: "Research draft"),
            WorkspaceTab(id: "release", threadId: "thread-release", title: "Prepare the release")
        ]
        model.activeTabID = "design"
        for tab in model.tabs {
            let id = try XCTUnwrap(tab.threadId)
            model.snapshots[id] = ThreadSnapshot(id: id, events: [ManagedEvent(cursor: "100", turnId: "turn-" + tab.id, data: .object(["type": .string("turn_completed")]))], hasMore: false, connected: true, activeTurns: [], settings: AgentSettings())
            model.messages[id] = [
                MessageEntry(id: id + "-user", turnId: "turn", kind: .user, text: "Keep agents easy to review and conversations easy to pick up."),
                MessageEntry(id: id + "-reply", turnId: "turn", kind: .assistant, text: "Each agent has its own place in the workspace. Review the latest update, send a follow-up, and move to the next pane.\n\n**Ready for review**\n\nThe composer stays with this conversation while other agents continue working.")
            ]
        }
        var requests: [(String, [JSONValue])] = []
        model.runtime.requestOverride = { method, args in
            requests.append((method, args))
            if method == "queuePrompt" { return .object(["turn_id": args[0]["requestId"], "state": .string("queued")]) }
            if method == "openThread" || method == "older" {
                let id = args[0].string
                return .object(["id": .string(id), "events": try .encoded(model.snapshots[id]!.events), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())])
            }
            return .null
        }
        let content = NSHostingView(rootView: AnyView(ContentView().environmentObject(model).preferredColorScheme(.light).frame(width: 1440, height: 900)))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1440, height: 900), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        func editors(_ view: NSView) -> [ComposerTextView] { (view as? ComposerTextView).map { [$0] } ?? view.subviews.flatMap(editors) }
        func scrolls(_ view: NSView) -> [NSScrollView] { ((view as? NSScrollView).map { [$0] } ?? []) + view.subviews.flatMap(scrolls) }
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        func capture(_ name: String) throws {
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            let rep = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
            content.cacheDisplay(in: content.bounds, to: rep)
            try XCTUnwrap(rep.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent(name))
        }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertFalse(model.isTiled)
        XCTAssertEqual(editors(content).count, 1, "One spacious conversation is the default")
        try capture("native-inbox-default.png")
        func key(_ code: UInt16, characters: String, modifiers: NSEvent.ModifierFlags = []) throws {
            let event = try XCTUnwrap(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: modifiers, timestamp: 0, windowNumber: window.windowNumber, context: nil, characters: characters, charactersIgnoringModifiers: characters, isARepeat: false, keyCode: code))
            window.firstResponder?.keyDown(with: event)
        }
        model.focusComposer()
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(window.firstResponder is ComposerTextView)
        try key(53, characters: "\u{1b}")
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(window.firstResponder is WorkspaceKeyboardView)
        XCTAssertEqual(model.workspaceFocus, .navigation)
        try key(53, characters: "\u{1b}")
        try key(53, characters: "\u{1b}")
        try key(48, characters: "\t")
        try key(36, characters: "\r")
        try key(0, characters: "a")
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(model.activeTabID, "research")
        XCTAssertEqual(model.tab("research")?.draft.count, "Research draft".count + 1, "Typing immediately after Enter survives a mounting page")
        try key(51, characters: "\u{7f}")
        try key(53, characters: "\u{1b}")
        XCTAssertTrue(window.firstResponder is WorkspaceKeyboardView, "Tab navigates without entering the next composer")
        try key(48, characters: "\t", modifiers: .shift)
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(model.activeTabID, "design")
        try key(125, characters: "\u{f701}")
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(model.activeTabID, "research")
        try key(123, characters: "\u{f702}")
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(model.activeTabID, "design")
        try key(36, characters: "\r")
        try await Task.sleep(for: .milliseconds(100))
        let writingEditor = try XCTUnwrap(window.firstResponder as? ComposerTextView)
        XCTAssertEqual(model.workspaceFocus, .writing)
        writingEditor.setSelectedRange(NSRange(location: writingEditor.string.utf16.count, length: 0))
        try key(123, characters: "\u{f702}")
        XCTAssertEqual(model.activeTabID, "design", "Arrows edit text in writing mode")
        XCTAssertEqual(writingEditor.selectedRange().location, writingEditor.string.utf16.count - 1)
        XCTAssertFalse(requests.contains { $0.0 == "queuePrompt" }, "Enter from navigation only focuses the composer")
        XCTAssertEqual(model.tab("design")?.draft, "Design draft")
        try capture("native-inbox-writing.png")
        model.enterNavigation()
        try await Task.sleep(for: .milliseconds(100))
        func pageController(_ view: NSView) -> InboxPageController? {
            if let controller = view.nextResponder as? InboxPageController { return controller }
            for child in view.subviews { if let found = pageController(child) { return found } }
            return nil
        }
        let pager = try XCTUnwrap(pageController(content))
        let swipeStart = CFAbsoluteTimeGetCurrent()
        pager.navigateForward(nil)
        try await Task.sleep(for: .milliseconds(450))
        XCTAssertEqual(model.activeTabID, "research")
        XCTAssertEqual(model.tab("design")?.draft, "Design draft")
        XCTAssertNil(model.tab("design")?.seenCursor, "Swiping never marks an agent seen")
        pager.navigateBack(nil)
        try await Task.sleep(for: .milliseconds(450))
        XCTAssertEqual(model.activeTabID, "design")
        XCTAssertTrue(editors(content).first === writingEditor, "Returning to a warm thread keeps its native editor")
        XCTAssertEqual(writingEditor.selectedRange().location, writingEditor.string.utf16.count - 1, "Switching threads retains the caret position")
        let navigationMs = (CFAbsoluteTimeGetCurrent() - swipeStart) * 1000
        XCTAssertLessThan(navigationMs, 1500)
        for id in ["release", "design", "research", "release", "research", "design"] {
            model.select(id)
            try await Task.sleep(for: .milliseconds(20))
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            XCTAssertEqual(pager.arrangedObjects[pager.selectedIndex] as? String, id)
            XCTAssertEqual(editors(content).first?.workspaceTabID, id, "Rapid sidebar selection shows the requested thread on the next frame")
            XCTAssertFalse(pager.selectedViewController?.view.isHiddenOrHasHiddenAncestor ?? true, "The selected conversation is visible, without an intermediate blank transition")
        }
        try capture("native-thread-switch.png")
        model.openBeside("research"); model.openBeside("release"); model.select("design")
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(model.canvasTabs.map(\.id), ["design", "research", "release"])
        XCTAssertGreaterThanOrEqual(editors(content).count, 2)
        let first = try XCTUnwrap(editors(content).first { $0.string == "Design draft" })
        let second = try XCTUnwrap(editors(content).first { $0.string == "Research draft" })
        second.insertText(" stays here", replacementRange: NSRange(location: second.string.utf16.count, length: 0))
        XCTAssertEqual(model.tab("research")?.draft, "Research draft stays here")
        XCTAssertEqual(model.tab("design")?.draft, "Design draft")
        XCTAssertEqual(first.string, "Design draft")
        XCTAssertEqual(model.activeTabID, "design", "Binding must target its own pane even before focus changes")
        second.submit?()
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(requests.last(where: { $0.0 == "queuePrompt" })?.1[0]["agentId"].string, "thread-research")
        XCTAssertEqual(requests.last(where: { $0.0 == "queuePrompt" })?.1[0]["input"].string, "Research draft stays here")
        XCTAssertEqual(model.tab("design")?.draft, "Design draft")
        model.pending = []
        try capture("native-inbox-tiles-light.png")
        let workspace = try XCTUnwrap(scrolls(content).first { $0.documentView is AgentSplitSurface })
        let surface = try XCTUnwrap(workspace.documentView as? AgentSplitSurface)
        XCTAssertEqual(surface.hosts.count, 3)
        XCTAssertTrue(surface.hosts.values.allSatisfy { $0.frame.width >= 360 })
        window.makeFirstResponder(first)
        try await Task.sleep(for: .milliseconds(100))
        model.cyclePane(1)
        try await Task.sleep(for: .milliseconds(350))
        XCTAssertEqual(model.activeTabID, "research")
        XCTAssertTrue(window.firstResponder === second, "Keyboard pane navigation transfers typing to that pane")
        try key(53, characters: "\u{1b}")
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(window.firstResponder is WorkspaceKeyboardView)
        model.cyclePane(1)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(workspace.documentVisibleRect.intersects(try XCTUnwrap(surface.hosts["release"]).frame), "Keyboard focus reveals the selected pane")
        XCTAssertTrue(window.firstResponder is WorkspaceKeyboardView, "Tiled navigation stays outside composers")
        model.select("research")
        let order = model.visibleTabs.map(\.id)
        model.snapshots["thread-design"]?.events.append(ManagedEvent(cursor: "101", data: .object(["type": .string("turn_failed")])))
        XCTAssertEqual(model.activeTabID, "research")
        XCTAssertEqual(model.visibleTabs.map(\.id), order, "Live updates must not shuffle tiles")
        model.review("research", seen: false)
        XCTAssertNil(model.tab("research")?.seenCursor)
        XCTAssertFalse(model.visibleTabs.contains { $0.id == "research" })
        model.review("design", seen: true)
        model.review("release", seen: true)
        XCTAssertTrue(model.visibleTabs.isEmpty)
        XCTAssertFalse(model.isTiled)
        let restored = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-restored-tiles")
        restored.workspaceFilter = .inbox
        restored.tabs = model.tabs.map { tab in var copy = tab; copy.seenCursor = "101"; copy.deferredCursor = "101"; return copy }
        restored.activeTabID = "design"; restored.snapshots = model.snapshots
        XCTAssertTrue(restored.visibleTabs.isEmpty, "Restoration must not pin an already reviewed agent back into Inbox")
        try await Task.sleep(for: .milliseconds(100)); try capture("native-inbox-zero.png")
        model.setFilter(.all)
        XCTAssertEqual(model.visibleTabs.count, 3)
        XCTAssertEqual(model.tab("design")?.draft, "Design draft")
        model.select("design"); model.movePane(1)
        XCTAssertEqual(model.tabs.map(\.id), ["research", "design", "release"])
        model.focusOnly("design")
        try await Task.sleep(for: .milliseconds(150)); try capture("native-inbox-focus.png")
        model.openBeside("research"); model.resizePanes(420)
        content.rootView = AnyView(ContentView().environmentObject(model).preferredColorScheme(.dark).frame(width: 820, height: 700))
        window.setContentSize(NSSize(width: 820, height: 700))
        try await Task.sleep(for: .milliseconds(250)); try capture("native-inbox-tiles-narrow.png")
        model.removePane("research")
        XCTAssertFalse(model.isTiled)
        XCTAssertEqual(model.activeTabID, "design")
        XCTAssertEqual(model.tab("design")?.draft, "Design draft")
        XCTAssertNotNil(model.tab("research"), "Removing a pane leaves its conversation in the sidebar")
        model.newTab()
        XCTAssertFalse(model.isTiled, "New tabs do not opt into tiling")
        XCTAssertFalse(requests.contains { ["cancel", "steer", "deleteThread"].contains($0.0) }, "Navigation and review cannot control an agent")
    }

    @MainActor
    func testTypedRuntimeEventsHandleFragmentedFramesAndIgnoreUnknownEvents() throws {
        let runtime = RuntimeClient(dataDirectory: "/tmp/nanocodex-isolated-wire")
        var connected = false
        var threads: [String] = []
        runtime.onEvent = { event in
            switch event { case .state(let state): connected = state.connected; case .thread(let thread): threads.append(thread.id); case .ignored: break }
        }
        let thread: JSONValue = .object(["id": .string("durable"), "events": .array([]), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())])
        let frames: [JSONValue] = [
            .object(["event": .object(["type": .string("future-event")])]),
            .object(["event": .object(["type": .string("state"), "state": Self.connectedState])]),
            .object(["event": .object(["type": .string("thread"), "thread": thread])])
        ]
        var bytes = Data()
        for frame in frames { bytes.append(try JSONEncoder().encode(frame)); bytes.append(10) }
        runtime.receiveForTesting(bytes.prefix(17))
        XCTAssertFalse(connected); XCTAssertTrue(threads.isEmpty)
        runtime.receiveForTesting(bytes.dropFirst(17))
        XCTAssertTrue(connected); XCTAssertEqual(threads, ["durable"])
    }
    @MainActor
    func testAsyncRuntimeDecodePreservesOrderWithoutBlockingMainQueue() async throws {
        let runtime = RuntimeClient(dataDirectory: "/tmp/nanocodex-isolated-async-wire")
        defer { runtime.stop() }
        let delivered = expectation(description: "Ordered runtime frames")
        var ids: [String] = []
        runtime.onEvent = { event in
            XCTAssertTrue(Thread.isMainThread)
            if case .thread(let thread) = event {
                ids.append(thread.id)
                if ids.count == 2 { delivered.fulfill() }
            }
        }
        let events = (0..<1800).map { ManagedEvent(cursor: "\($0)", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("History payload")])])])) }
        func frame(_ id: String) throws -> Data {
            let thread: JSONValue = .object(["id": .string(id), "events": try .encoded(events), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())])
            var data = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("thread"), "thread": thread])]))
            data.append(10); return data
        }
        let first = try frame("first"), second = try frame("second")
        runtime.receiveAsynchronouslyForTesting(first.prefix(37))
        runtime.receiveAsynchronouslyForTesting(first.dropFirst(37) + second)
        let responsive = expectation(description: "Main queue remains available during decode")
        DispatchQueue.main.async { XCTAssertTrue(ids.isEmpty); responsive.fulfill() }
        await fulfillment(of: [responsive, delivered], timeout: 5)
        XCTAssertEqual(ids, ["first", "second"])
    }
    @MainActor
    func testRuntimeDrainsFinalResponseBeforeImmediateChildExit() async throws {
        let runtime = RuntimeClient(dataDirectory: "/tmp/nanocodex-isolated-exit-drain")
        defer { runtime.stop() }
        let exited = expectation(description: "Runtime exit after stdout drain")
        runtime.onFailure = { _ in exited.fulfill() }
        try runtime.startForTesting(executable: URL(fileURLWithPath: "/usr/bin/python3"), arguments: ["-u", "-c", """
        import json, sys
        request = json.loads(sys.stdin.readline())
        sys.stdout.write(json.dumps({"id": request["id"], "result": "final-response-" * 30000}) + "\\n")
        sys.stdout.flush()
        """])
        let reply = try await runtime.request("finish")
        XCTAssertEqual(reply.string, String(repeating: "final-response-", count: 30000))
        await fulfillment(of: [exited], timeout: 5)
    }
    @MainActor
    func testRuntimeRetainsCursorOnlySnapshotUpdates() throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-cursor-update")
        defer { model.shutdown() }
        for cursor in ["1", "2"] {
            let thread: JSONValue = .object(["id": .string("thread"), "events": .array([]), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings()), "cursor": .string(cursor)])
            var frame = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("thread"), "thread": thread])]))
            frame.append(10); model.runtime.receiveForTesting(frame)
        }
        XCTAssertEqual(model.snapshots["thread"]?.cursor, "2")
    }
    @MainActor
    func testIdenticalRuntimeStateDoesNotPublishAgain() throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-state-dedup")
        model.runtime.requestOverride = { _, _ in .null }
        defer { model.shutdown() }
        var updates = 0
        let observation = model.$state.dropFirst().sink { _ in updates += 1 }
        defer { observation.cancel() }
        var data = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("state"), "state": Self.connectedState])]))
        data.append(10)
        model.runtime.receiveForTesting(data); model.runtime.receiveForTesting(data)
        XCTAssertEqual(updates, 1)
    }
    func testCachedTimelinePreservesInterleavingReplayHistoryAndCorrections() {
        func delta(_ cursor: String, _ turn: String, _ text: String) -> ManagedEvent {
            .init(cursor: cursor, turnId: turn, data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string(text)])])]))
        }
        var cache = TimelineProjection()
        var events = [delta("2", "first", "Hello"), delta("3", "second", "A follow-up")]
        XCTAssertEqual(cache.project(events), projectTimeline(events))
        events.append(delta("4", "first", " world"))
        XCTAssertEqual(cache.project(events), projectTimeline(events))
        XCTAssertEqual(cache.project(events + events), projectTimeline(events))
        events.insert(.init(cursor: "1", turnId: "older", data: .object(["type": .string("turn_completed"), "final_message": .string("Older answer")])), at: 0)
        XCTAssertEqual(cache.project(events), projectTimeline(events))
        events[1] = delta("2", "first", "Corrected")
        XCTAssertEqual(cache.project(events), projectTimeline(events))
        events.append(.init(cursor: "5", turnId: "first", data: .object(["type": .string("turn_completed"), "final_message": .string("Final answer")])))
        XCTAssertEqual(cache.project(events), projectTimeline(events))
        XCTAssertEqual(cache.project([]), [])
        XCTAssertEqual(cache.project([delta("2", "other-account", "Other account")]).first?.text, "Other account")
    }
    @MainActor
    func testNativeHistoryLoadsOnScrollAndRetainsVisibleMessage() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-history-" + UUID().uuidString)
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        let tab = WorkspaceTab(id: "history", threadId: "history-thread", title: "Automatic history")
        model.tabs = [tab]; model.activeTabID = tab.id; model.workspaceFilter = .all
        func turns(_ range: Range<Int>) -> [ManagedEvent] {
            range.flatMap { index in [
                ManagedEvent(cursor: "\(index)-accepted", turnId: "turn-\(index)", data: .object(["type": .string("turn_accepted"), "input": .string("Review change \(index)")])),
                ManagedEvent(cursor: "\(index)-reply", turnId: "turn-\(index)", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string(Array(repeating: "This paragraph describes the change, its context, and the result of the native review.", count: 6).joined(separator: "\n\n"))])])]))
            ] }
        }
        var events = turns(20..<28)
        func snapshot(hasMore: Bool = true) throws -> JSONValue {
            .object(["id": .string("history-thread"), "events": try .encoded(events), "hasMore": .bool(hasMore), "connected": .bool(true), "activeTurns": .array([.string("turn-27")]), "settings": try .encoded(AgentSettings())])
        }
        func publish() throws {
            var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("thread"), "thread": try snapshot()])]))
            wire.append(10); model.runtime.receiveForTesting(wire)
        }
        try publish()
        var requests = 0
        var response: CheckedContinuation<JSONValue, Error>?
        model.runtime.requestOverride = { method, args in
            guard method == "older" else { return .null }
            XCTAssertEqual(args.first?.string, "history-thread")
            requests += 1
            return try await withCheckedThrowingContinuation { response = $0 }
        }
        let host = NSHostingView(rootView: ContentView().environmentObject(model).preferredColorScheme(.light).frame(width: 1200, height: 840))
        host.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { response?.resume(throwing: CancellationError()); model.shutdown(); window.close() }
        func scrolls(_ view: NSView) -> [NSScrollView] { ((view as? NSScrollView).map { [$0] } ?? []) + view.subviews.flatMap(scrolls) }
        func markers(_ view: NSView) -> [TranscriptItemAnchor.MarkerView] { ((view as? TranscriptItemAnchor.MarkerView).map { [$0] } ?? []) + view.subviews.flatMap(markers) }
        try await Task.sleep(for: .milliseconds(300))
        let scroll = try XCTUnwrap(scrolls(host).first { ($0.documentView?.bounds.height ?? 0) > 2000 })
        let document = try XCTUnwrap(scroll.documentView)
        XCTAssertEqual(requests, 0, "Opening and initial layout cannot drain history")
        func userScroll(to y: CGFloat) async throws {
            scroll.contentView.scroll(to: NSPoint(x: 0, y: y)); scroll.reflectScrolledClipView(scroll.contentView)
            NotificationCenter.default.post(name: NSScrollView.didLiveScrollNotification, object: scroll)
            try await Task.sleep(for: .milliseconds(60))
        }
        try await userScroll(to: 180)
        XCTAssertEqual(requests, 1, "Approaching the top automatically fetches a page")
        for _ in 0..<3 { try await userScroll(to: 210) }
        XCTAssertEqual(requests, 1, "Repeated scroll notifications share one in-flight request")
        let visible = scroll.documentVisibleRect
        let retained = try XCTUnwrap(markers(document).filter { $0.convert($0.bounds, to: document).maxY > visible.minY }.min { $0.convert($0.bounds, to: document).minY < $1.convert($1.bounds, to: document).minY })
        let retainedID = retained.itemID
        func anchorY() throws -> CGFloat {
            let marker = try XCTUnwrap(markers(document).first { $0.itemID == retainedID })
            return marker.convert(marker.bounds, to: document).minY - scroll.documentVisibleRect.minY
        }
        let before = try anchorY()
        let oldHeight = document.bounds.height
        events.append(.init(cursor: "live-more", turnId: "turn-27", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string(Array(repeating: "\n\nLive output continues below the retained message.", count: 12).joined())])])])))
        try publish(); try await Task.sleep(for: .milliseconds(100))
        XCTAssertGreaterThan(document.bounds.height, oldHeight, "The fixture grows the live response during pagination")
        XCTAssertEqual(try anchorY(), before, accuracy: 2)
        // A page can project to very little text (for example mostly tool
        // events), so continuing upward must work even inside the near-top zone.
        events.insert(.init(cursor: "19-reply", turnId: "turn-19", data: .object(["type": .string("turn_completed"), "final_message": .string("Earlier context.")])), at: 0)
        let firstResponse = try XCTUnwrap(response); response = nil; firstResponse.resume(returning: try snapshot())
        var samples: [CGFloat] = []
        for _ in 0..<12 {
            try await Task.sleep(for: .milliseconds(16)); host.layoutSubtreeIfNeeded(); host.displayIfNeeded()
            samples.append(try anchorY())
        }
        XCTAssertTrue(samples.allSatisfy { abs($0 - before) < 2 }, "Prepending retains the actual visible message in every sampled frame: \(samples)")
        XCTAssertEqual(requests, 1, "Prepend layout cannot automatically cascade into another page")
        XCTAssertGreaterThan(scroll.documentVisibleRect.minY, 210)
        XCTAssertLessThan(scroll.documentVisibleRect.minY, 400, "A short page must not trap pagination inside the rearm threshold")
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds)); host.cacheDisplay(in: host.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-automatic-history.png"))
        try await userScroll(to: scroll.documentVisibleRect.minY - 20)
        try await userScroll(to: 180)
        XCTAssertEqual(requests, 2, "Scrolling back through the boundary fetches the next page")
        let failedResponse = try XCTUnwrap(response); response = nil; failedResponse.resume(throwing: RuntimeFailure(message: "Temporary history failure"))
        try await Task.sleep(for: .milliseconds(80))
        for _ in 0..<3 { try await userScroll(to: 180) }
        XCTAssertEqual(requests, 2, "A failed page cannot spin while the boundary remains visible")
        try await userScroll(to: 600); try await userScroll(to: 180)
        XCTAssertEqual(requests, 3, "Leaving and re-entering the boundary rearms a failed page")
        events = turns(0..<19) + events
        let finalResponse = try XCTUnwrap(response); response = nil; finalResponse.resume(returning: try snapshot(hasMore: false))
        try await Task.sleep(for: .milliseconds(200))
        try await userScroll(to: 600); try await userScroll(to: 180)
        XCTAssertEqual(requests, 3, "An exhausted transcript stops pagination")
    }

    @MainActor
    func testNativeWorkspaceRenderingAndInteractionLatency() async throws {
        let phase = ProcessInfo.processInfo.environment["NANOCODEX_PERFORMANCE_PHASE"] ?? "after"
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-visual")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.runtime.requestOverride = { _, _ in .null }
        model.tabs = [WorkspaceTab(title: "Build something"), WorkspaceTab(title: "Explore my code")]
        model.activeTabID = model.tabs[0].id
        model.state.hands = [
            Hand(id: "studio", name: "Studio workspace", kind: "local", workspace: "/Users/you/Code/Studio", status: "connected", calls: 12, activeCalls: 0),
            Hand(id: "vm", name: "Private Linux VM", kind: "vm", workspace: "/workspace", status: "stopped", calls: 3, activeCalls: 0)
        ]
        let content = NSHostingView(rootView: AnyView(ContentView().environmentObject(model).preferredColorScheme(.dark).frame(width: 1200, height: 840)))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let began = CFAbsoluteTimeGetCurrent()
        window.contentView = content; window.setContentSize(NSSize(width: 1200, height: 840)); window.makeKeyAndOrderFront(nil)
        content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
        let firstLayoutMs = (CFAbsoluteTimeGetCurrent() - began) * 1000
        defer { model.shutdown(); window.close() }
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        let evidence = root.appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        func capture(_ name: String) throws {
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            let rep = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
            content.cacheDisplay(in: content.bounds, to: rep)
            try XCTUnwrap(rep.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-\(name)-\(phase).png"))
        }
        func editor(in view: NSView) -> NSTextView? {
            if let found = view as? ComposerTextView { return found }
            for child in view.subviews { if let found = editor(in: child) { return found } }
            return nil
        }
        try await Task.sleep(for: .milliseconds(100))
        try capture("chat")
        let input = try XCTUnwrap(editor(in: content)); window.makeFirstResponder(input)
        let typing = CFAbsoluteTimeGetCurrent()
        input.insertText("Build a beautiful native app", replacementRange: NSRange(location: NSNotFound, length: 0))
        content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
        let inputMs = (CFAbsoluteTimeGetCurrent() - typing) * 1000
        XCTAssertEqual(model.activeTab?.draft, "Build a beautiful native app")
        var tabMs: [Double] = []
        for index in 0..<20 {
            let started = CFAbsoluteTimeGetCurrent()
            model.select(model.tabs[index % 2].id); content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            tabMs.append((CFAbsoluteTimeGetCurrent() - started) * 1000)
        }
        model.activeTabID = model.tabs[0].id; model.tabs[0].threadId = "preview"
        try await Task.sleep(for: .milliseconds(80))
        try capture("thread-loading")
        model.snapshots["preview"] = ThreadSnapshot(id: "preview", events: [], hasMore: false, connected: true, activeTurns: ["turn"], settings: AgentSettings())
        model.messages["preview"] = (0..<18).map { index in MessageEntry(id: "row-\(index)", turnId: "turn", kind: index.isMultiple(of: 2) ? .user : .assistant, text: index.isMultiple(of: 2) ? "Refine the native tabs and keyboard shortcuts." : "The sidebar now keeps every open thread within reach. You can switch tabs with **⌘⇧[** and **⌘⇧]**, and reopen a closed tab with **⌘⇧T**.\n\nDrafts stay with their tabs, so you can pick up exactly where you left off.", streaming: index == 17) }
        try await Task.sleep(for: .milliseconds(120))
        func scrollViews(in view: NSView) -> [NSScrollView] { ((view as? NSScrollView).map { [$0] } ?? []) + view.subviews.flatMap { scrollViews(in: $0) } }
        var transcript = try XCTUnwrap(scrollViews(in: content).first { ($0.documentView?.bounds.height ?? 0) > 1000 })
        transcript.contentView.scroll(to: .zero); transcript.reflectScrolledClipView(transcript.contentView)
        NotificationCenter.default.post(name: NSScrollView.didLiveScrollNotification, object: transcript)
        try await Task.sleep(for: .milliseconds(60))
        let readingOffset = transcript.contentView.bounds.origin.y
        model.messages["preview"]?[17].text += "\n\nThis additional streamed paragraph should not interrupt reading earlier messages."
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(transcript.contentView.bounds.origin.y, readingOffset, accuracy: 2, "Streaming must preserve manual scroll position")
        let readingTab = model.activeTabID
        let savedOffset = model.readingPositions[readingTab]?.offset
        XCTAssertNotNil(savedOffset)
        let retainedTranscript = transcript
        for _ in 0..<4 {
            model.newTab()
            try await Task.sleep(for: .milliseconds(35))
        }
        try await Task.sleep(for: .milliseconds(220))
        model.select(readingTab)
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(model.readingPositions[readingTab]?.followsOutput, false, "Returning to a conversation preserves reading mode")
        XCTAssertEqual(model.readingPositions[readingTab]?.offset, savedOffset, "Returning to a conversation preserves its scroll offset")
        transcript = try XCTUnwrap(scrollViews(in: content).first { ($0.documentView?.bounds.height ?? 0) > 1000 })
        XCTAssertTrue(transcript === retainedTranscript, "A distant warm conversation keeps its actual viewport")
        XCTAssertEqual(transcript.contentView.bounds.minY, savedOffset ?? -1, accuracy: 2, "The native viewport returns to its reading position")
        // Explicit scrolling reaches the bottom; later output still cannot move it.
        for _ in 0..<3 {
            let bottom = max(0, (transcript.documentView?.bounds.height ?? 0) - transcript.contentView.bounds.height)
            transcript.contentView.scroll(to: NSPoint(x: 0, y: bottom)); transcript.reflectScrolledClipView(transcript.contentView)
            NotificationCenter.default.post(name: NSScrollView.didLiveScrollNotification, object: transcript)
            try await Task.sleep(for: .milliseconds(40))
        }
        let bottomReadingOffset = transcript.contentView.bounds.minY
        for _ in 0..<6 {
            model.messages["preview"]?[17].text += "\n\nThe latest response continues here."
            try await Task.sleep(for: .milliseconds(25))
        }
        try await Task.sleep(for: .milliseconds(80))
        let remaining = (transcript.documentView?.bounds.maxY ?? 0) - transcript.documentVisibleRect.maxY
        XCTAssertGreaterThan(remaining, 64, "New output grows below the fixed reading position")
        XCTAssertEqual(transcript.contentView.bounds.minY, bottomReadingOffset, accuracy: 2, "Streaming must never jump to the bottom")
        model.messages["preview"]?.append(.init(id: "next-user", turnId: "next-turn", kind: .user, text: "Start this follow-up at the top"))
        var arrivalOffsets: [CGFloat] = []
        for _ in 0..<8 {
            try await Task.sleep(for: .milliseconds(16))
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            arrivalOffsets.append(transcript.contentView.bounds.minY)
        }
        let followUpTop = transcript.contentView.bounds.minY
        XCTAssertTrue(arrivalOffsets.allSatisfy { abs($0 - followUpTop) < 2 }, "Follow-up insertion must not flash through an estimated bottom position")
        XCTAssertGreaterThan(followUpTop, bottomReadingOffset, "A new prompt moves its turn to the top")
        model.messages["preview"]?.append(.init(id: "next-reply", turnId: "next-turn", kind: .assistant, text: "The answer begins here.", streaming: true))
        var offsets: [CGFloat] = []
        for _ in 0..<20 {
            model.messages["preview"]?[19].text += "\n\nThe response grows down from the prompt without moving it."
            try await Task.sleep(for: .milliseconds(20))
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            offsets.append(transcript.contentView.bounds.minY)
        }
        XCTAssertTrue(offsets.allSatisfy { abs($0 - followUpTop) < 2 }, "Every sampled streaming frame must retain the prompt's top position")
        try capture("transcript")
        model.screen = .hands
        try await Task.sleep(for: .milliseconds(100)); try capture("hands")
        content.rootView = AnyView(ContentView().environmentObject(model).preferredColorScheme(.dark).frame(width: 820, height: 700))
        window.setContentSize(NSSize(width: 820, height: 700))
        try await Task.sleep(for: .milliseconds(100)); try capture("hands-narrow")
        content.rootView = AnyView(SettingsView().environmentObject(model).preferredColorScheme(.light))
        window.setContentSize(NSSize(width: 600, height: 580))
        try await Task.sleep(for: .milliseconds(100)); try capture("settings")
        content.rootView = AnyView(SettingsView(section: .appearance).environmentObject(model).preferredColorScheme(.light).id("appearance"))
        window.setContentSize(NSSize(width: 600, height: 580))
        try await Task.sleep(for: .milliseconds(100)); try capture("settings-appearance")
        content.rootView = AnyView(SettingsView(section: .shortcuts).environmentObject(model).preferredColorScheme(.light).id("shortcuts"))
        window.setContentSize(NSSize(width: 600, height: 580))
        try await Task.sleep(for: .milliseconds(100)); try capture("settings-shortcuts")
        let history: [ManagedEvent] = (0..<800).map { index in
            .init(cursor: "\(index)", turnId: "benchmark", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("A streamed response with durable history. ")])])]))
        }
        let snapshot: JSONValue = .object(["id": .string("benchmark"), "events": try .encoded(history), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())])
        let event: JSONValue = .object(["event": .object(["type": .string("thread"), "thread": snapshot])])
        var wire = try JSONEncoder().encode(event); wire.append(10)
        model.runtime.receiveForTesting(wire)
        let repeated = CFAbsoluteTimeGetCurrent()
        for _ in 0..<40 { model.runtime.receiveForTesting(wire) }
        let snapshotMs = (CFAbsoluteTimeGetCurrent() - repeated) * 1000 / 40
        var longHistory: [ManagedEvent] = []
        for turn in 0..<80 {
            longHistory.append(.init(cursor: "\(turn)-accepted", turnId: "long-\(turn)", data: .object(["type": .string("turn_accepted"), "input": .string("Explain change \(turn)")])))
            for chunk in 0..<20 {
                longHistory.append(.init(cursor: "\(turn)-\(chunk)", turnId: "long-\(turn)", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("A paragraph explaining this change and its implications for the native workspace. ")])])])))
            }
            longHistory.append(.init(cursor: "\(turn)-done", turnId: "long-\(turn)", data: .object(["type": .string("turn_completed")])))
        }
        var streamingMs: [Double] = []
        for chunk in 0..<20 {
            longHistory.append(.init(cursor: "live-\(chunk)", turnId: "live", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("The current response grows while prior turns stay unchanged. ")])])])) )
            let thread: JSONValue = .object(["id": .string("long-stream"), "events": try .encoded(longHistory), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([.string("live")]), "settings": try .encoded(AgentSettings())])
            let frame: JSONValue = .object(["event": .object(["type": .string("thread"), "thread": thread])])
            var packet = try JSONEncoder().encode(frame); packet.append(10)
            let started = CFAbsoluteTimeGetCurrent()
            model.runtime.receiveForTesting(packet)
            streamingMs.append((CFAbsoluteTimeGetCurrent() - started) * 1000)
        }
        XCTAssertEqual(model.messages["long-stream"]?.count, 161)
        let metrics: [String: Any] = ["phase": phase, "firstNativeViewLayoutMs": firstLayoutMs, "nativeEditorInputMs": inputMs, "tabSwitchMedianMs": tabMs.sorted()[10], "tabSwitchP95Ms": tabMs.sorted()[18], "unchanged800EventSnapshotMs": snapshotMs, "longStreamingSnapshotMedianMs": streamingMs.sorted()[10], "longStreamingSnapshotP95Ms": streamingMs.sorted()[18], "network": "none; isolated native view and protocol evidence"]
        try JSONSerialization.data(withJSONObject: metrics, options: [.prettyPrinted, .sortedKeys]).write(to: evidence.appendingPathComponent("native-performance-\(phase).json"))
    }
    @MainActor
    func testNativePhoneAndCodeScreensRenderWithoutNetwork() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-protocol")
        model.isStarting = false
        model.runtime.requestOverride = { method, _ in
            guard method == "startSignIn" else { throw RuntimeFailure(message: "Unexpected network request") }
            let now = Date().timeIntervalSince1970 * 1000
            return .object(["phone": .string("+15555550100"), "resendAt": .number(now + 30_000), "expiresAt": .number(now + 300_000)])
        }
        let content = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 840))
        content.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = content; window.setContentSize(NSSize(width: 1200, height: 840)); window.orderFront(nil)
        defer { window.close() }
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        let evidence = root.appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        func fields(_ view: NSView) -> [NSTextField] { (view as? NSTextField).map { [$0] } ?? view.subviews.flatMap(fields) }
        func capture(_ name: String) throws {
            content.layoutSubtreeIfNeeded(); content.displayIfNeeded()
            let rep = try XCTUnwrap(content.bitmapImageRepForCachingDisplay(in: content.bounds))
            content.cacheDisplay(in: content.bounds, to: rep)
            try XCTUnwrap(rep.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent(name))
        }
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertTrue(fields(content).contains { $0.placeholderString == "+1 415 555 0123" || $0.placeholderString == "Phone number" })
        try capture("native-sign-in-phone.png")
        _ = try await model.startPhoneSignIn(phone: "+15555550100", baseUrl: "https://example.invalid")
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertTrue(fields(content).contains { $0.placeholderString == "6-digit code" })
        try capture("native-sign-in-code.png")
    }
    func testSignInChallengeUsesMillisecondsAndAcceptsPastedCode() throws {
        let challenge = try JSONValue.object([
            "phone": .string("+15555550100"), "resendAt": .number(1_780_000_030_000), "expiresAt": .number(1_780_000_300_000)
        ]).decode(SignInChallenge.self)
        let now = Date(timeIntervalSince1970: 1_780_000_000)
        XCTAssertEqual(challenge.resendSeconds(at: now), 30)
        XCTAssertEqual(challenge.resendSeconds(at: now.addingTimeInterval(31)), 0)
        XCTAssertFalse(challenge.isExpired(at: now.addingTimeInterval(299)))
        XCTAssertTrue(challenge.isExpired(at: now.addingTimeInterval(300)))
        XCTAssertEqual(SignInChallenge.normalizedCode("123 456\n"), "123456")
        XCTAssertEqual(SignInChallenge.normalizedCode("１２３abc1234567"), "123456")
    }
    func testAcceptedTurnLocksModelBeforeCompletionAndDecodesOlderSnapshots() throws {
        var value: [String: JSONValue] = [
            "id": .string("thread"), "events": .array([]), "hasMore": .bool(false), "connected": .bool(true),
            "activeTurns": .array([]), "settings": try .encoded(AgentSettings())
        ]
        XCTAssertFalse(try JSONValue.object(value).decode(ThreadSnapshot.self).hasAcceptedTurn)
        value["acceptedTurns"] = .number(1)
        XCTAssertTrue(try JSONValue.object(value).decode(ThreadSnapshot.self).hasAcceptedTurn)
        value.removeValue(forKey: "acceptedTurns")
        value["activeTurns"] = .array([.string("first-turn")])
        XCTAssertTrue(try JSONValue.object(value).decode(ThreadSnapshot.self).hasAcceptedTurn)
    }
    @MainActor
    func testPhoneOnboardingWaitsForCommitAndRetriesWithoutConsumingCodeAgain() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-protocol")
        model.isStarting = false
        var calls: [String] = []
        var completionAttempts = 0
        model.runtime.requestOverride = { method, _ in
            calls.append(method)
            switch method {
            case "startSignIn":
                return .object(["phone": .string("+15555550100"), "resendAt": .number(30_000), "expiresAt": .number(300_000)])
            case "verifySignIn":
                return .object(["baseUrl": .string("https://example.invalid"), "apiKey": .string("isolated-private-credential")])
            case "connect":
                XCTAssertTrue(model.showsOnboarding)
                return Self.connectedState
            case "completeSignIn":
                XCTAssertTrue(model.state.connected)
                XCTAssertTrue(model.showsOnboarding)
                completionAttempts += 1
                if completionAttempts == 1 { throw RuntimeFailure(message: "Temporary connection interruption") }
                return .null
            default: throw RuntimeFailure(message: "Unexpected protocol method: \(method)")
            }
        }
        _ = try await model.startPhoneSignIn(phone: "+15555550100", baseUrl: "https://example.invalid")
        do { try await model.finishPhoneSignIn(code: "123456"); XCTFail("Expected the interrupted completion") }
        catch { XCTAssertTrue(model.phoneSignInActive); XCTAssertTrue(model.showsOnboarding) }
        try await model.finishPhoneSignIn(code: "123456")
        XCTAssertFalse(model.showsOnboarding)
        XCTAssertFalse(model.phoneSignInActive)
        XCTAssertEqual(calls, ["startSignIn", "verifySignIn", "connect", "completeSignIn", "completeSignIn"])
        try await model.cancelPhoneSignIn()
        XCTAssertEqual(calls.count, 5)
    }
    @MainActor
    func testCancellingPhoneSwitchPreservesExistingAccountAndTabs() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-protocol")
        model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [WorkspaceTab(title: "Existing work", draft: "Keep this draft")]
        let tabs = model.tabs
        var calls: [String] = []
        model.runtime.requestOverride = { method, _ in
            calls.append(method)
            if method == "startSignIn" { return .object(["phone": .string("+15555550100"), "resendAt": .number(30_000), "expiresAt": .number(300_000)]) }
            guard method == "cancelSignIn" else { throw RuntimeFailure(message: "Existing account was changed") }
            return .null
        }
        _ = try await model.startPhoneSignIn(phone: "+15555550100", baseUrl: "https://example.invalid")
        XCTAssertFalse(model.showsOnboarding)
        try await model.cancelPhoneSignIn()
        XCTAssertTrue(model.state.connected)
        XCTAssertEqual(model.tabs, tabs)
        XCTAssertEqual(calls, ["startSignIn", "cancelSignIn"])
    }
    private static var connectedState: JSONValue { .object([
        "connected": .bool(true), "baseUrl": .string("https://example.invalid"), "threads": .array([]), "hands": .array([]),
        "defaults": .object([:]), "platform": .string("darwin"), "version": .string("0.1.0")
    ]) }
    func testAstraSelectionNormalizesUnsupportedSettings() throws {
        var settings = AgentSettings(model: "gpt-5.6-sol", thinking: "none", reasoning_mode: "pro", fast_mode: true)
        settings.selectModel("gpt-6-astra")

        XCTAssertEqual(settings.modelName, "Astra")
        XCTAssertEqual(settings.thinking, "high")
        XCTAssertEqual(settings.reasoning_mode, "standard")
        XCTAssertFalse(settings.supportsNoReasoning)
        XCTAssertFalse(settings.supportsProReasoning)
        XCTAssertEqual(try JSONValue.encoded(settings), .object([
            "model": .string("gpt-6-astra"), "thinking": .string("high"),
            "reasoning_mode": .string("standard"), "fast_mode": .bool(true),
        ]))
    }
    func testAstraRetainsSupportedEffortsAndExistingDefaults() throws {
        XCTAssertEqual(AgentSettings(), AgentSettings(model: "gpt-5.6-sol", thinking: "high", reasoning_mode: "standard", fast_mode: false))
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            var settings = AgentSettings(model: "gpt-5.6-terra", thinking: effort, reasoning_mode: "pro", fast_mode: false)
            settings.selectModel("gpt-6-astra")
            XCTAssertEqual(settings.thinking, effort)
            XCTAssertEqual(settings.reasoning_mode, "standard")
            XCTAssertFalse(settings.fast_mode)
            let retained = try JSONValue.encoded(settings).decode(AgentSettings.self)
            XCTAssertEqual(retained, settings)
        }
        var existing = AgentSettings(model: "gpt-5.6-sol", thinking: "none", reasoning_mode: "pro", fast_mode: true)
        existing.selectModel("gpt-5.6-sol")
        XCTAssertEqual(existing.thinking, "none")
        XCTAssertEqual(existing.reasoning_mode, "pro")
        XCTAssertTrue(existing.supportsNoReasoning)
        XCTAssertTrue(existing.supportsProReasoning)
    }
    func testDurableReplayDoesNotDuplicateOutput() throws {
        let events: [ManagedEvent] = [
            .init(cursor: "1", turnId: "turn", data: .object(["type": .string("turn_accepted"), "id": .string("turn"), "input": .string("hello")])),
            .init(cursor: "2", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("Hello")])])])),
            .init(cursor: "3", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.message"), "payload": .object(["text": .string("Hello there")])])])),
            .init(cursor: "4", turnId: "turn", data: .object(["type": .string("turn_completed"), "id": .string("turn"), "final_message": .string("Hello there")]))
        ]
        let projected = projectTimeline(events + events)
        XCTAssertEqual(projected.count, 2)
        XCTAssertEqual(projected.last?.text, "Hello there")
        XCTAssertFalse(projected.last?.streaming ?? true)
    }
    @MainActor
    func testAcceptedAndFinalMessagesKeepTheirIdentity() throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-continuity")
        model.tabs = [WorkspaceTab(id: "tab", threadId: "thread")]; model.activeTabID = "tab"
        model.pending = [.init(id: "turn", tabID: "tab", agentID: "thread", text: "Keep this conversation steady")]
        let before = try XCTUnwrap(model.displayedTranscript("tab").first)
        let title = model.title(model.tabs[0])
        var events = [ManagedEvent(cursor: "1", turnId: "turn", data: .object(["type": .string("turn_accepted"), "input": .string(before.text)]))]
        let frame: JSONValue = .object(["event": .object(["type": .string("thread"), "thread": .object(["id": .string("thread"), "events": try .encoded(events), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([.string("turn")]), "settings": try .encoded(AgentSettings())])])])
        var wire = try JSONEncoder().encode(frame); wire.append(10); model.runtime.receiveForTesting(wire)
        XCTAssertEqual(model.pending.first?.phase, .queued)
        var accepted = before; accepted.cursor = "1"
        XCTAssertEqual(model.displayedTranscript("tab"), [accepted], "Server acceptance updates the existing optimistic row with its admission cursor")
        XCTAssertEqual(model.title(model.tabs[0]), title, "Acceptance must not flash the title back to New thread")
        events.append(.init(cursor: "2", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("A partial response")])])])))
        let streamed = try XCTUnwrap(projectTimeline(events).last)
        events.append(.init(cursor: "3", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.message"), "payload": .object(["text": .string("")])])])))
        XCTAssertEqual(projectTimeline(events).last?.text, streamed.text, "An empty completion payload cannot erase streamed text")
        events.append(.init(cursor: "4", turnId: "turn", data: .object(["type": .string("turn_completed"), "final_message": .string("A completed response.")])))
        let completed = projectTimeline(events)
        XCTAssertEqual(completed.count, 2)
        XCTAssertEqual(completed.last?.id, streamed.id)
        XCTAssertEqual(completed.last?.cursor, "2", "Later deltas and completion retain the row's original admission cursor")
        XCTAssertEqual(completed.last?.text, "A completed response.")
        XCTAssertFalse(completed.last?.streaming ?? true)
    }
    @MainActor
    func testFailedThreadLoadIsLocalAndRetryRestoresTheConversation() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-thread-retry")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [WorkspaceTab(id: "missing", threadId: "missing-thread", draft: "Keep this draft"), WorkspaceTab(id: "healthy", threadId: "healthy-thread")]
        model.activeTabID = "missing"
        var fail = true
        var opens = 0
        model.runtime.requestOverride = { method, args in
            if method == "openThread" {
                opens += 1
                let id = args[0].string
                if id == "missing-thread" && fail { throw RuntimeFailure(message: "managed request failed (404)") }
                return .object(["id": .string(id), "events": .array([]), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())])
            }
            if method == "refresh" { return Self.connectedState }
            return .null
        }
        defer { model.shutdown() }
        XCTAssertFalse(model.canSend("missing"), "A restored thread must load before it accepts another message")
        await model.observe("missing-thread"); await model.observe("healthy-thread")
        XCTAssertNotNil(model.threadError("missing"))
        XCTAssertNil(model.error, "One stale thread must not cover the whole workspace with an error")
        XCTAssertTrue(model.loading.isEmpty, "A failed load must stop spinning")
        XCTAssertFalse(model.canSend("missing")); XCTAssertTrue(model.canSend("healthy"))
        XCTAssertEqual(model.tab("missing")?.draft, "Keep this draft")
        let host = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 840))
        host.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { window.orderOut(nil) }
        try await Task.sleep(for: .milliseconds(100)); host.layoutSubtreeIfNeeded(); host.displayIfNeeded()
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-missing-conversation.png"))
        fail = false
        await model.refresh()
        XCTAssertNil(model.threadError("missing"))
        XCTAssertTrue(model.canSend("missing"))
        XCTAssertEqual(model.tab("missing")?.draft, "Keep this draft")
        XCTAssertEqual(opens, 3, "Refresh retries the failed active thread, not healthy observers")
    }

    @MainActor
    func testCancelledQueueDoesNotStealTheReplyOrClearInboxAttention() throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-cancelled-queue")
        defer { model.shutdown() }
        model.tabs = [WorkspaceTab(id: "tab", threadId: "thread")]; model.activeTabID = "tab"
        var events: [ManagedEvent] = [
            .init(cursor: "1", turnId: "running", data: .object(["type": .string("turn_accepted"), "input": .string("Real request")])),
            .init(cursor: "2", turnId: "running", data: .object(["type": .string("event"), "event": .object(["type": .string("run.started")])])),
            .init(cursor: "3", turnId: "running", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("The useful answer")])])])),
            .init(cursor: "4", turnId: "queued", data: .object(["type": .string("turn_accepted"), "input": .string("Never execute this")])),
            .init(cursor: "5", turnId: "queued", data: .object(["type": .string("turn_cancelling")]))
        ]
        func deliver(_ active: [String], cursor: String) throws {
            let snapshot: JSONValue = .object(["id": .string("thread"), "events": try .encoded(events), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array(active.map(JSONValue.string)), "settings": try .encoded(AgentSettings()), "cursor": .string(cursor)])
            var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("thread"), "thread": snapshot])])); wire.append(10)
            model.runtime.receiveForTesting(wire)
        }
        model.pending = [.init(id: "queued", tabID: "tab", agentID: "thread", text: "Never execute this", predecessor: "running", phase: .cancelling, acceptedCursor: "4")]
        let before = projectTimeline(events)
        XCTAssertEqual(before.map(\.turnId), ["running", "running"])
        events += [
            .init(cursor: "6", turnId: "running", data: .object(["type": .string("turn_completed"), "final_message": .string("The useful answer")])),
            .init(cursor: "7", turnId: "queued", data: .object(["type": .string("event"), "event": .object(["type": .string("run.started")])])),
            .init(cursor: "8", turnId: "queued", data: .object(["type": .string("event"), "event": .object(["type": .string("run.error"), "payload": .object(["message": .string("the turn was cancelled")])])]))
        ]
        try deliver(["queued"], cursor: "8")
        XCTAssertEqual(model.pending.first?.phase, .cancelling, "Draining cancellation must not pretend the queued message started work")
        XCTAssertEqual(model.displayedTranscript().map(\.id), before.map(\.id))
        events.append(.init(cursor: "9", turnId: "queued", data: .object(["type": .string("turn_cancelled")])))
        try deliver([], cursor: "9")
        XCTAssertTrue(model.pending.isEmpty)
        XCTAssertEqual(model.displayedTranscript().last?.text, "The useful answer")
        XCTAssertEqual(model.update(for: model.tabs[0]).cursor, "6")
        XCTAssertTrue(model.update(for: model.tabs[0]).needsAttention(model.tabs[0]), "Cancelling a queued item must not clear a completed answer's review state")
        model.tabs[0].seenCursor = "6"
        XCTAssertFalse(model.update(for: model.tabs[0]).needsAttention(model.tabs[0]), "Cancellation metadata must not create a new unread update")
        let cancellationOfStartedWork = [events[0], events[1], ManagedEvent(cursor: "10", turnId: "running", data: .object(["type": .string("turn_cancelling")])), ManagedEvent(cursor: "11", turnId: "running", data: .object(["type": .string("turn_cancelled")]))]
        XCTAssertEqual(projectTimeline(cancellationOfStartedWork).last?.text, "Stopped by you.", "Stopping actual running work remains visible")
    }

    func testQueuedPolicyKeepsExactCursorsAndRestoresUnconfirmedDelivery() throws {
        var message = PendingMessage(id: "follow-up", tabID: "one", agentID: "thread", text: "Change direction", predecessor: "original")
        XCTAssertNil(message.interruption)
        message.restore()
        XCTAssertEqual(message.phase, .failed)
        XCTAssertEqual(try JSONDecoder().decode(PendingMessage.self, from: JSONEncoder().encode(message)), message)
        message.phase = .queued; message.acceptedCursor = "9007199254740993"
        XCTAssertEqual(message.interruption, "original")
        var snapshot = ThreadSnapshot(id: "thread", events: [], hasMore: false, connected: true, activeTurns: [], settings: AgentSettings(), cursor: "9007199254740992")
        XCTAssertFalse(message.hasFinished(in: snapshot), "A stale state read cannot erase a newly accepted message")
        snapshot.cursor = "9007199254740993"
        XCTAssertTrue(message.hasFinished(in: snapshot))
        snapshot.activeTurns = [message.id]
        XCTAssertFalse(message.hasFinished(in: snapshot))
        message.phase = .starting; XCTAssertNil(message.interruption)
        message.restore(); XCTAssertEqual(message.phase, .queued)
        XCTAssertFalse(message.hasStarted(in: [.init(cursor: "1", turnId: "original", data: .object(["type": .string("turn_cancelled")]))]))
        XCTAssertFalse(message.hasStarted(in: [.init(cursor: "2", turnId: message.id, data: .object(["type": .string("turn_accepted")]))]))
        XCTAssertTrue(message.hasStarted(in: [.init(cursor: "3", turnId: message.id, data: .object(["type": .string("event"), "event": .object(["type": .string("run.started")])]))]))
    }

    @MainActor
    func testFailedHandPreparationStopsWorkingAndCanBeCancelledLocally() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-hand-failure")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [WorkspaceTab(id: "tab", threadId: "thread", folder: "/tmp/workspace")]; model.activeTabID = "tab"
        var requests: [String] = []
        model.runtime.requestOverride = { method, _ in
            requests.append(method)
            if method == "openThread" { return .object(["id": .string("thread"), "events": .array([]), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())]) }
            if method == "prepareFolderHand" { throw RuntimeFailure(message: "Unexpected server response: 503") }
            return .null
        }
        defer { model.shutdown() }
        await model.observe("thread")
        await model.send("Read the selected workspace")
        let pending = try XCTUnwrap(model.pendingMessages().first)
        XCTAssertEqual(pending.phase, .failed)
        XCTAssertTrue(pending.error?.contains("Hand couldn’t connect") == true)
        XCTAssertFalse(model.working())
        XCTAssertFalse(model.update(for: model.tabs[0]).running)
        XCTAssertTrue(model.update(for: model.tabs[0]).failed)
        XCTAssertFalse(requests.contains("queuePrompt"))
        await model.cancelPending(pending.id)
        XCTAssertTrue(model.pendingMessages().isEmpty)
        XCTAssertFalse(requests.contains("cancel"), "A message that never reached submission can be removed locally")
    }

    func testFollowUpAcceptanceDoesNotSplitOrReplaceStreamingResponse() throws {
        func accepted(_ cursor: String, _ turn: String, _ text: String) -> ManagedEvent { .init(cursor: cursor, turnId: turn, data: .object(["type": .string("turn_accepted"), "input": .string(text)])) }
        func delta(_ cursor: String, _ text: String) -> ManagedEvent { .init(cursor: cursor, turnId: "original", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string(text)])])])) }
        var events = [accepted("1", "original", "First prompt"), delta("2", "Still")]
        let original = try XCTUnwrap(projectTimeline(events).last)
        events += [accepted("3", "queued", "Change direction"), delta("4", " working")]
        let during = projectTimeline(events)
        XCTAssertEqual(during.filter { $0.kind == .assistant }.count, 1)
        XCTAssertEqual(during[1].id, original.id)
        XCTAssertEqual(during[1].text, "Still working")
        events.append(.init(cursor: "5", turnId: "original", data: .object(["type": .string("turn_completed"), "final_message": .string("Finished working")])) )
        let completed = projectTimeline(events)
        XCTAssertEqual(completed[1].id, original.id)
        XCTAssertEqual(completed[1].text, "Finished working")
        XCTAssertEqual(completed.map(\.kind), [.user, .assistant, .user])
    }

    @MainActor
    func testNativeQueueSteeringRetryAndPaneIsolation() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-steering")
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.tabs = [WorkspaceTab(id: "one", threadId: "thread-one", draft: "Change direction"), WorkspaceTab(id: "two", threadId: "thread-two", draft: "Keep this draft")]
        model.activeTabID = "one"
        var events = [ManagedEvent(cursor: "1", turnId: "original", data: .object(["type": .string("turn_accepted"), "input": .string("Keep working on the first request")]))]
        func frame(_ active: [String], cursor: String = "1") throws -> JSONValue {
            .object(["id": .string("thread-one"), "events": try .encoded(events), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array(active.map(JSONValue.string)), "settings": try .encoded(AgentSettings()), "cursor": .string(cursor)])
        }
        func deliver(_ active: [String], cursor: String = "1") throws {
            var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("thread"), "thread": try frame(active, cursor: cursor)])])); wire.append(10)
            model.runtime.receiveForTesting(wire)
        }
        try deliver(["original"])
        var requests: [(String, [JSONValue])] = []
        var saved: JSONValue = .null
        var fail = true
        model.runtime.requestOverride = { method, args in
            requests.append((method, args))
            if method == "saveLayout" { saved = args[0] }
            if method == "openThread" { return try frame(["original"]) }
            if method == "queuePrompt" {
                if fail { fail = false; throw RuntimeFailure(message: "Lost acknowledgement") }
                return .object(["turn_id": args[0]["requestId"], "cursor": .string("2"), "state": .string("queued")])
            }
            if method == "cancel" {
                try await Task.sleep(for: .milliseconds(70))
                return .object(["turn_id": args[0]["turnId"], "state": .string("cancelled")])
            }
            return .null
        }
        let host = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 840))
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        host.sizingOptions = []; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { window.orderOut(nil); model.shutdown() }
        await model.send(tabID: "one")
        let first = try XCTUnwrap(model.pending.first)
        XCTAssertEqual(first.phase, .failed)
        XCTAssertFalse(model.canSend("one"))
        XCTAssertEqual(model.tab("one")?.draft, "")
        await model.retryPending(first.id)
        let submissions = requests.filter { $0.0 == "queuePrompt" }
        XCTAssertEqual(submissions.count, 2)
        XCTAssertEqual(submissions[0].1, submissions[1].1, "Retry sends the captured payload with the exact same ID")
        XCTAssertEqual(model.pending.first?.phase, .queued)
        XCTAssertEqual(model.controllableTurns("one"), ["original"])
        await model.send("Then update the tests", tabID: "one")
        let second = try XCTUnwrap(model.pending.last)
        XCTAssertEqual(second.predecessor, first.id)
        await model.send("Finally summarize", tabID: "one")
        let third = try XCTUnwrap(model.pending.last)
        await model.cancelPending(second.id)
        XCTAssertEqual(model.pending.last?.id, third.id)
        XCTAssertEqual(model.pending.last?.predecessor, first.id, "Cancelling a queued message reconnects its successor")
        model.persistLayout(); try await Task.sleep(for: .milliseconds(400))
        let layout = try saved.decode(TabLayout.self)
        XCTAssertEqual(layout.pendingMessages, model.pending)
        let restored = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-steering-restored")
        defer { restored.shutdown() }
        guard case .object(var state) = Self.connectedState else { return XCTFail("Missing state fixture") }
        state["layout"] = try .encoded(layout)
        var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("state"), "state": .object(state)])])); wire.append(10)
        restored.runtime.receiveForTesting(wire)
        XCTAssertEqual(restored.pending.first?.id, first.id)
        XCTAssertEqual(restored.pending.first?.interruption, "original")
        model.select("two")
        let steer = Task { await model.steerNow(first.id) }
        try await Task.sleep(for: .milliseconds(10))
        await model.steerNow(first.id)
        await steer.value
        XCTAssertEqual(requests.filter { $0.0 == "cancel" && $0.1[0]["turnId"].string == "original" }.count, 1, "Rapid repeat steering cannot send a second command")
        XCTAssertFalse(requests.contains { $0.0 == "steer" })
        XCTAssertEqual(model.pending.first?.phase, .starting, "A stop receipt cannot remove a message that has not started")
        XCTAssertEqual(model.tab("two")?.draft, "Keep this draft")
        XCTAssertEqual(model.pending.first?.agentID, "thread-one")
        model.select("one")
        try await Task.sleep(for: .milliseconds(100)); host.layoutSubtreeIfNeeded()
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-steering-queue.png"))
        events.append(.init(cursor: "2", turnId: first.id, data: .object(["type": .string("turn_accepted"), "input": .string(first.text)])))
        try deliver(["original", first.id], cursor: "2")
        XCTAssertFalse(model.displayedTranscript("one").contains { $0.turnId == first.id }, "Waiting messages stay in the composer")
        events.append(.init(cursor: "3", turnId: "original", data: .object(["type": .string("turn_cancelled")])))
        try deliver([first.id], cursor: "3")
        XCTAssertEqual(model.pending.first?.id, first.id, "Only the queued turn's own start removes its row")
        events.append(.init(cursor: "4", turnId: first.id, data: .object(["type": .string("event"), "event": .object(["type": .string("run.started")])])))
        try deliver([first.id], cursor: "4")
        XCTAssertFalse(model.pending.contains { $0.id == first.id })
        XCTAssertEqual(model.displayedTranscript("one").filter { $0.id == MessageEntry.userID(first.id) }.count, 1)
        XCTAssertEqual(requests.filter { $0.0 == "queuePrompt" }.count, 4, "Steer now never resubmits the queued message")
    }

    func testToolResultUpdatesCallAndPreservesExactOutput() {
        let events: [ManagedEvent] = [
            .init(cursor: "90071992547409930", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("tool.call"), "payload": .object(["call_id": .string("c"), "tool": .string("exec_command"), "arguments": .object(["cmd": .string("pwd")])])])])),
            .init(cursor: "90071992547409931", turnId: "turn", data: .object(["type": .string("event"), "event": .object(["type": .string("tool.result"), "payload": .object(["call_id": .string("c"), "status": .string("completed"), "result": .string("/workspace")])])]))
        ]
        let projected = projectTimeline(events)
        XCTAssertEqual(projected.count, 1)
        XCTAssertEqual(projected.first?.status, "completed")
        XCTAssertEqual(projected.first?.output, "/workspace")
    }

    func testGeneratedCodeOutputsPreserveBothResultsAndDeduplicateNestedMedia() throws {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZQmcAAAAASUVORK5CYII="
        let events = try Self.generatedOutputEvents(png: png)
        var cache = TimelineProjection()
        let rows = cache.project(events)
        XCTAssertEqual(rows, projectTimeline(events))
        let items = NativeConversationItem.group(rows, working: false)
        let outputs = items.flatMap(\.generatedOutputs)
        XCTAssertEqual(outputs.filter { $0.kind == .image }.count, 1, "The inner MCP image and outer exec input_image must render only once")
        XCTAssertEqual(outputs.filter { $0.kind == .file }.count, 1, "The structured result's file survives alongside the raw result's media")
        XCTAssertFalse(outputs.contains { $0.kind == .text }, "Tool text stays in Activity instead of becoming assistant prose")
        XCTAssertTrue(items.contains { !$0.activity.isEmpty }, "Tool diagnostics remain available in Activity")
        XCTAssertTrue(items.filter { !$0.generatedOutputs.isEmpty }.allSatisfy { $0.activity.isEmpty }, "Emitted content must remain visible outside the collapsed Activity group")
        XCTAssertTrue(rows.filter { $0.kind == .tool }.allSatisfy { !$0.output.contains(png) }, "Binary media must never be printed as diagnostics")
        XCTAssertTrue(rows.contains { $0.output.contains("exit_code") }, "Structured diagnostics remain readable")
        let delta = ManagedEvent(cursor: "7", turnId: "generated", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("The preview is ready.")])])]))
        XCTAssertEqual(cache.project(events + [delta]), projectTimeline(events + [delta]), "Live output retains the already parsed media")
        var corrected = events
        corrected[4].data = .object(["type": .string("event"), "event": .object(["type": .string("tool.result"), "payload": .object(["call_id": .string("exec"), "result": .string("Corrected generated text"), "structured_result": .object(["exit_code": .number(1)])])])])
        XCTAssertEqual(cache.project(corrected), projectTimeline(corrected), "Corrections invalidate the exact cached result")
        XCTAssertEqual(cache.project(corrected).last(where: { $0.kind == .tool })?.status, "failed")
        let trimmed = [events[0], delta]
        XCTAssertEqual(cache.project(trimmed), projectTimeline(trimmed), "Trimming history releases obsolete tool outputs")
        XCTAssertTrue(cache.project(trimmed).allSatisfy { $0.generatedOutputs.isEmpty })
    }

    func testUserMediaAndInspectedFramesStayOutOfGeneratedReplies() {
        let source = "data:image/png;base64,aGVsbG8="
        let input: JSONValue = .array([
            .object(["type": .string("input_text"), "text": .string("Please inspect this")]),
            .object(["type": .string("input_image"), "image_url": .string(source)])
        ])
        let events: [ManagedEvent] = [
            .init(cursor: "1", turnId: "media", data: .object(["type": .string("turn_accepted"), "input": input])),
            .init(cursor: "2", turnId: "media", data: .object(["type": .string("event"), "event": .object(["type": .string("tool.call"), "payload": .object(["call_id": .string("inspect"), "tool": .string("view_image"), "arguments": .object(["path": .string("frame.png")])])])])),
            .init(cursor: "3", turnId: "media", data: .object(["type": .string("event"), "event": .object(["type": .string("tool.result"), "payload": .object(["call_id": .string("inspect"), "result": .object(["type": .string("image"), "image_url": .string(source)])])])]))
        ]
        let rows = projectTimeline(events)
        XCTAssertEqual(rows.first?.text, "Please inspect this")
        XCTAssertEqual(rows.first?.attachments?.images, [source])
        XCTAssertTrue(rows.flatMap(\.generatedOutputs).isEmpty)
        XCTAssertEqual(rows.filter { $0.kind == .tool }.count, 1)
    }

    private static func generatedOutputEvents(png: String) throws -> [ManagedEvent] {
        func event(_ cursor: String, _ type: String, _ payload: [String: JSONValue]) -> ManagedEvent {
            .init(cursor: cursor, turnId: "generated", data: .object(["type": .string("event"), "event": .object(["type": .string(type), "payload": .object(payload)])]))
        }
        let emitted: JSONValue = .array([
            .object(["type": .string("input_text"), "text": .string("**Generated preview** — emitted from code mode.")]),
            .object(["type": .string("input_image"), "image_url": .string("data:image/png;base64," + png)])
        ])
        let raw = String(data: try JSONEncoder().encode(emitted), encoding: .utf8)!
        return [
            .init(cursor: "1", turnId: "generated", data: .object(["type": .string("turn_accepted"), "input": .string("Generate a preview and export the report.")])),
            event("2", "tool.call", ["call_id": .string("image"), "tool": .string("imagegen"), "arguments": .object(["prompt": .string("A native generated-output preview")])]),
            event("3", "tool.result", ["call_id": .string("image"), "result": .object(["content": .array([.object(["type": .string("image"), "mimeType": .string("image/png"), "data": .string(png)])])])]),
            event("4", "tool.call", ["call_id": .string("exec"), "tool": .string("functions.exec"), "arguments": .object(["code": .string("text('Generated preview'); image(result.content[0]);")])]),
            event("5", "tool.result", ["call_id": .string("exec"), "result": .string(raw), "structured_result": .object(["exit_code": .number(0), "content": .array([.object(["type": .string("resource_link"), "name": .string("report.csv"), "uri": .string("https://example.invalid/generated/report.csv"), "mimeType": .string("text/csv")])])])]),
            .init(cursor: "6", turnId: "generated", data: .object(["type": .string("turn_completed")]))
        ]
    }

    @MainActor
    func testNativeGeneratedCodeOutputsRenderWithoutExpandingActivity() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-isolated-generated-output-" + UUID().uuidString)
        model.isStarting = false; model.state = try Self.connectedState.decode(DesktopState.self)
        model.runtime.requestOverride = { _, _ in .null }
        let tab = WorkspaceTab(id: "generated", threadId: "generated-thread", title: "Generated preview")
        model.tabs = [tab]; model.activeTabID = tab.id; model.workspaceFilter = .all
        let bitmap = try XCTUnwrap(NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 480, pixelsHigh: 180, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
        let context = try XCTUnwrap(NSGraphicsContext(bitmapImageRep: bitmap))
        NSGraphicsContext.saveGraphicsState(); NSGraphicsContext.current = context
        NSColor(calibratedRed: 0.08, green: 0.35, blue: 0.76, alpha: 1).setFill(); NSRect(x: 0, y: 0, width: 480, height: 180).fill()
        NSColor(calibratedRed: 0.28, green: 0.84, blue: 0.72, alpha: 1).setFill(); NSBezierPath(roundedRect: NSRect(x: 28, y: 28, width: 110, height: 124), xRadius: 24, yRadius: 24).fill()
        ("Generated image" as NSString).draw(at: NSPoint(x: 164, y: 95), withAttributes: [.font: NSFont.systemFont(ofSize: 26, weight: .semibold), .foregroundColor: NSColor.white])
        ("Visible directly in the chat" as NSString).draw(at: NSPoint(x: 164, y: 65), withAttributes: [.font: NSFont.systemFont(ofSize: 17), .foregroundColor: NSColor.white])
        NSGraphicsContext.restoreGraphicsState()
        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).base64EncodedString()
        let events = try Self.generatedOutputEvents(png: png)
        let snapshot: JSONValue = .object(["id": .string("generated-thread"), "events": try .encoded(events), "hasMore": .bool(false), "connected": .bool(true), "activeTurns": .array([]), "settings": try .encoded(AgentSettings())])
        var wire = try JSONEncoder().encode(JSONValue.object(["event": .object(["type": .string("thread"), "thread": snapshot])]))
        wire.append(10); model.runtime.receiveForTesting(wire)
        let host = NSHostingView(rootView: ContentView().environmentObject(model).preferredColorScheme(.light).frame(width: 1200, height: 900))
        host.sizingOptions = []
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 900), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { model.shutdown(); window.close() }
        try await Task.sleep(for: .milliseconds(700))
        host.layoutSubtreeIfNeeded(); host.displayIfNeeded()
        XCTAssertTrue(model.expandedMessages[tab.id]?.isEmpty ?? true, "No activity disclosure was opened")
        func markers(_ view: NSView) -> [TranscriptItemAnchor.MarkerView] { ((view as? TranscriptItemAnchor.MarkerView).map { [$0] } ?? []) + view.subviews.flatMap(markers) }
        let mediaRows = markers(host).filter { $0.itemID.hasPrefix("output-") }
        XCTAssertEqual(mediaRows.count, 2, "Nested media and outer text/file output have inline native rows")
        XCTAssertGreaterThan(mediaRows.map { $0.bounds.height }.max() ?? 0, 100, "The image contributes real rendered height outside Activity")
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        let image = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds)); host.cacheDisplay(in: host.bounds, to: image)
        let url = evidence.appendingPathComponent("native-generated-code-output.png")
        try XCTUnwrap(image.representation(using: .png, properties: [:])).write(to: url)
        let attachment = XCTAttachment(contentsOfFile: url); attachment.lifetime = .keepAlways; add(attachment)

        var live = Array(events.dropLast()), projection = TimelineProjection()
        _ = projection.project(live)
        var retainedMs: [Double] = [], rebuiltMs: [Double] = []
        for index in 0..<24 {
            live.append(.init(cursor: "stream-\(index)", turnId: "generated", data: .object(["type": .string("event"), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string("The generated output remains visible while this response streams. ")])])])))
            var start = CFAbsoluteTimeGetCurrent()
            let retained = projection.project(live)
            retainedMs.append((CFAbsoluteTimeGetCurrent() - start) * 1000)
            start = CFAbsoluteTimeGetCurrent()
            let rebuilt = projectTimeline(live)
            rebuiltMs.append((CFAbsoluteTimeGetCurrent() - start) * 1000)
            XCTAssertEqual(retained, rebuilt)
        }
        let metrics: [String: Any] = ["retainedToolOutputsMedianMs": retainedMs.sorted()[12], "reparsedToolOutputsMedianMs": rebuiltMs.sorted()[12], "pngBytes": png.utf8.count, "snapshots": 24, "network": "none; actual code-mode wire shape, hosted native window"]
        try JSONSerialization.data(withJSONObject: metrics, options: [.prettyPrinted, .sortedKeys]).write(to: evidence.appendingPathComponent("native-generated-output-performance.json"))
    }
    func testSubagentStreamsRemainSeparate() {
        func delta(_ cursor: String, _ agent: String, _ text: String) -> ManagedEvent {
            .init(cursor: cursor, turnId: "turn", data: .object(["type": .string("event"), "agent_id": .string(agent), "event": .object(["type": .string("assistant.delta"), "payload": .object(["text": .string(text)])])]))
        }
        let result = projectTimeline([delta("1", "researcher", "Research"), delta("2", "coder", "Code"), delta("3", "coder", " change")])
        XCTAssertEqual(result.map(\.text), ["Research", "Code change"])
        XCTAssertNotEqual(result[0].agent, result[1].agent)
    }
    func testSharedStateAndLayoutContractDecodes() throws {
        let fixture = #"{"connected":true,"hasCredentials":true,"baseUrl":"https://example.test","threads":[],"hands":[{"id":"mac-1","name":"This Mac","kind":"local","workspace":"/tmp/work","status":"connected","calls":0,"activeCalls":0,"logs":[]}],"defaults":{"workspace":"/tmp/default"},"platform":"darwin","version":"0.1.0","layout":{"tabs":[{"id":"tab-1","draft":"unfinished","target":"mac-1","folder":""}],"activeTabId":"tab-1","tabPosition":"top","theme":"system"}}"#
        let state = try JSONDecoder().decode(DesktopState.self, from: Data(fixture.utf8))
        XCTAssertEqual(state.hands.first?.status, "connected")
        XCTAssertEqual(state.layout?.tabs.first?.draft, "unfinished")
        XCTAssertEqual(state.layout?.tabPosition, "top")
        XCTAssertEqual(state.defaults["workspace"].string, "/tmp/default")
    }
}

import AppKit
import SwiftUI

final class BackgroundHandTests: XCTestCase {
    @MainActor
    func testNativeControlPanelRendering() async throws {
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-panel-" + UUID().uuidString)
        defer { model.shutdown() }
        model.isStarting = false; model.state.connected = true
        model.state.hands = [
            Hand(id: "mac", name: "This Mac", kind: "local", workspace: "/tmp", status: "connected", activeCalls: 2),
            Hand(id: "vm", name: "Build sandbox", kind: "vm", workspace: "/workspace", status: "stopped")
        ]
        model.state.accountHands = [
            AccountHand(id: "ios-phone", name: "iPhone", workspace: "/ios-phone", capabilities: ["native", "background_limited"], status: "connected"),
            AccountHand(id: "ios-tablet", name: "iPad", workspace: "/ios-tablet", capabilities: ["native", "background_limited"], status: "offline")
        ]
        XCTAssertEqual(model.connectedHandCount, 2)
        model.newTab()
        model.updateTarget("ios-phone")
        XCTAssertEqual(model.activeTab?.target, "ios-phone")
        XCTAssertEqual(model.activeTab?.folder, "")
        model.updateTarget("ios-tablet")
        XCTAssertEqual(model.activeTab?.target, "ios-phone", "An offline device is visible but cannot be selected")
        model.tabs = [WorkspaceTab(id: "running", threadId: "running", title: "Ship the new agent workspace"),
                      WorkspaceTab(id: "review", threadId: "review", title: "Review the reconnect flow")]
        model.activeTabID = "running"
        model.snapshots["running"] = ThreadSnapshot(id: "running", events: [], hasMore: false, connected: true, activeTurns: ["turn"], settings: AgentSettings())
        model.snapshots["review"] = ThreadSnapshot(id: "review", events: [.init(cursor: "1", turnId: "done", data: .object(["type": .string("turn_completed")]))], hasMore: false, connected: true, activeTurns: [], settings: AgentSettings())
        model.messages["running"] = [.init(id: "tool", turnId: "turn", kind: .tool, text: "Running the browser checks", name: "exec_command", status: "running")]
        let host = NSHostingView(rootView: HandControlPanel(model: model, openMainWindow: {}))
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 720, height: 560), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = host; window.makeKeyAndOrderFront(nil)
        defer { window.close() }
        let evidence = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        for theme in ["light", "dark"] {
            model.theme = theme
            try await Task.sleep(for: .milliseconds(200))
            host.layoutSubtreeIfNeeded(); host.displayIfNeeded()
            XCTAssertEqual(host.fittingSize.width, 720, accuracy: 1)
            let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
            host.cacheDisplay(in: host.bounds, to: bitmap)
            try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: evidence.appendingPathComponent("native-control-panel-\(theme).png"))
        }
    }

    @MainActor
    func testSleepPolicyOnlyPreventsIdleSleepWhenEnabledAndRunning() throws {
        XCTAssertNil(HandBackgroundActivity.options(running: false, keepAwake: false))
        XCTAssertNil(HandBackgroundActivity.options(running: false, keepAwake: true))
        let normal = try XCTUnwrap(HandBackgroundActivity.options(running: true, keepAwake: false))
        XCTAssertFalse(normal.contains(.idleSystemSleepDisabled))
        XCTAssertTrue(normal.contains(.automaticTerminationDisabled))
        let awake = try XCTUnwrap(HandBackgroundActivity.options(running: true, keepAwake: true))
        XCTAssertTrue(awake.contains(.idleSystemSleepDisabled))
        XCTAssertFalse(awake.contains(.idleDisplaySleepDisabled))
    }

    @MainActor
    func testPreferenceAndActivityFollowHandLifecycle() throws {
        let suite = "nanocodex-background-test-" + UUID().uuidString
        let preferences = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { preferences.removePersistentDomain(forName: suite) }
        let model = AppModel(runtimeDirectory: "/tmp/" + suite, backgroundPreferences: preferences)
        defer { model.shutdown() }
        XCTAssertTrue(model.keepMacAwake, "New installations keep enabled Hands available through idle sleep")
        model.isStarting = false; model.state.connected = true
        XCTAssertEqual(model.backgroundHandStatus, "No Hands running")
        model.keepMacAwake = true
        XCTAssertNil(model.backgroundActivity.options, "An idle app must not hold the Mac awake")
        model.state.hands = [Hand(id: "hand", name: "This Mac", kind: "local", workspace: "/tmp", status: "connecting")]
        XCTAssertEqual(model.backgroundActivity.options, .userInitiated)
        model.state.hands[0].status = "connected"
        XCTAssertEqual(model.backgroundHandStatus, "1 Hand connected")
        model.keepMacAwake = false
        XCTAssertEqual(model.backgroundActivity.options, .userInitiatedAllowingIdleSystemSleep)
        model.keepMacAwake = true
        model.state.hands[0].status = "stopped"
        XCTAssertNil(model.backgroundActivity.options)
        model.state.hands[0].status = "connected"
        model.state.connected = false
        XCTAssertNil(model.backgroundActivity.options, "Sign-out releases the activity even with stale Hand state")
        model.state.connected = true
        model.runtime.onFailure?("Runtime stopped")
        XCTAssertNil(model.backgroundActivity.options)
        XCTAssertTrue(model.backgroundHandStatus.contains("Runtime stopped"))

        let reopened = AppModel(runtimeDirectory: "/tmp/" + suite, backgroundPreferences: preferences)
        defer { reopened.shutdown() }
        XCTAssertTrue(reopened.keepMacAwake, "The checkbox survives relaunch")
        reopened.state.connected = true; reopened.state.hands = model.state.hands
        XCTAssertEqual(reopened.backgroundActivity.options, .userInitiated)
        reopened.shutdown()
        XCTAssertNil(reopened.backgroundActivity.options)
        reopened.state.hands[0].status = "connecting"
        XCTAssertNil(reopened.backgroundActivity.options, "Late runtime events cannot reacquire activity during quit")
        preferences.set(false, forKey: "keepMacAwakeWhileHandsRunning")
        let optedOut = AppModel(runtimeDirectory: "/tmp/" + suite, backgroundPreferences: preferences)
        XCTAssertFalse(optedOut.keepMacAwake, "The default must preserve an existing explicit opt-out")
        optedOut.shutdown()
    }
}

/// Runs the native AppKit editor and rendered SwiftUI window against the real
/// managed service. This does not require macOS's separate UI automation mode.
final class NativeServiceTests: XCTestCase {
    @MainActor
    func testNativeWindowHandAndTwoDurableTurns() async throws {
        let repository = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let envURL = repository.appendingPathComponent(".env")
        guard FileManager.default.fileExists(atPath: envURL.path) else { throw XCTSkip("A development .env account is required for the real managed-service journey.") }
        let env = try String(contentsOf: envURL, encoding: .utf8)
        guard let line = env.components(separatedBy: .newlines).first(where: { $0.hasPrefix("NC_API_KEY=") }), let key = line.split(separator: "=", maxSplits: 1).last.map(String.init) else { throw XCTSkip("NC_API_KEY is required for live native evidence.") }
        let apiKey = key.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines.union(CharacterSet(charactersIn: "\"'")))
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("nanocodex-native-service-\(UUID().uuidString)")
        let workspace = directory.appendingPathComponent("workspace")
        let statePath = directory.appendingPathComponent("state")
        try FileManager.default.createDirectory(at: workspace, withIntermediateDirectories: true)
        let evidence = repository.appendingPathComponent("macos/build/evidence")
        try FileManager.default.createDirectory(at: evidence, withIntermediateDirectories: true)
        var createdID: String?
        var model = AppModel(runtimeDirectory: statePath.path)
        model.workspaceMode = "single"
        let window = EvidenceWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 840), styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
        window.title = "Nanocodex"; window.center(); window.isReleasedWhenClosed = false
        window.setContentSize(NSSize(width: 1200, height: 840))
        var host = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 840))
        host.sizingOptions = []
        window.contentView = host; window.makeKeyAndOrderFront(nil)
        window.setContentSize(NSSize(width: 1200, height: 840))
        let start = Date()
        do {
            await model.start()
            model.workspaceMode = "single"
            print("Native journey: connected")
            XCTAssertTrue(model.state.connected, model.error ?? model.state.error ?? "Account did not connect")
            try await waitUntil(timeout: 25) { model.state.hands.contains { $0.kind == "local" && $0.agentId == nil && $0.status == "connected" } }
            let automaticHandID = try XCTUnwrap(model.state.hands.first { $0.kind == "local" && $0.agentId == nil }?.id)
            model.keepMacAwake = true
            XCTAssertEqual(model.backgroundActivity.options, .userInitiated)
            let connectedAt = Date().timeIntervalSince(start)
            try await Task.sleep(for: .milliseconds(250))
            try capture(host, to: evidence.appendingPathComponent("native-new-thread.png"))
            var editor = try XCTUnwrap(findEditor(host))
            window.makeFirstResponder(editor)
            editor.insertText("A native draft", replacementRange: editor.selectedRange())
            XCTAssertEqual(model.activeTab?.draft, "A native draft")
            let newline = try XCTUnwrap(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: .shift, timestamp: 0, windowNumber: window.windowNumber, context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36))
            editor.keyDown(with: newline)
            XCTAssertTrue(model.activeTab?.draft.contains("\n") == true, "Shift Return must insert a newline in the native editor")
            let firstTab = model.activeTabID
            model.newTab(); try await Task.sleep(for: .milliseconds(220))
            editor = try XCTUnwrap(findEditor(host))
            XCTAssertEqual(editor.string, "")
            editor.insertText("Second native draft", replacementRange: editor.selectedRange())
            model.closeTab(model.activeTabID); try await Task.sleep(for: .milliseconds(220))
            editor = try XCTUnwrap(findEditor(host))
            XCTAssertEqual(model.activeTabID, firstTab)
            XCTAssertTrue(editor.string.hasPrefix("A native draft"))
            model.reopenTab(); try await Task.sleep(for: .milliseconds(220))
            editor = try XCTUnwrap(findEditor(host))
            XCTAssertEqual(editor.string, "Second native draft")
            model.updateDraft("")
            model.updateTab { $0.folder = workspace.path }
            model.tabPosition = "top"; model.persistLayout()
            try await Task.sleep(for: .milliseconds(100))
            let prompt = "Native Nanocodex integration test. Write exactly native-hand-roundtrip-ok to native-evidence.txt in the selected Hand workspace using exec_command, then read the file using exec_command. Reply NATIVE_HAND_READY when both operations succeeded."
            editor.insertText(prompt, replacementRange: editor.selectedRange())
            let enter = try XCTUnwrap(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber, context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36))
            editor.keyDown(with: enter)
            try await waitUntil(timeout: 25) { model.activeTab?.threadId != nil }
            createdID = model.activeTab?.threadId
            window.close()
            XCTAssertFalse(window.isVisible)
            XCTAssertFalse(AppDelegate().applicationShouldTerminateAfterLastWindowClosed(NSApp))
            try await waitUntil(timeout: 110) { model.activeMessages.contains { $0.kind == .assistant && $0.text.contains("NATIVE_HAND_READY") } && !model.isRunning }
            XCTAssertFalse(window.isVisible, "The real Hand must finish with its window closed")
            XCTAssertEqual(model.backgroundActivity.options, .userInitiated)
            window.makeKeyAndOrderFront(nil)
            XCTAssertEqual(try String(contentsOf: workspace.appendingPathComponent("native-evidence.txt"), encoding: .utf8).trimmingCharacters(in: .newlines), "native-hand-roundtrip-ok")
            XCTAssertEqual(model.state.hands.count, 2)
            let folderHand = try XCTUnwrap(model.state.hands.first { $0.agentId == createdID })
            XCTAssertEqual(folderHand.status, "connected", "Selected folders still create a Hand scoped to their thread")
            XCTAssertNil(model.error)
            print("Native journey: first real file roundtrip completed")
            let firstTurnAt = Date().timeIntervalSince(start)
            try capture(host, to: evidence.appendingPathComponent("native-first-turn.png"))
            model.screen = .hands; try await Task.sleep(for: .milliseconds(150))
            try capture(host, to: evidence.appendingPathComponent("native-connected-hand.png"))
            model.screen = .chat
            await model.prepareToQuit()
            print("Native journey: previous helper closed")
            XCTAssertNil(model.backgroundActivity.options)
            try await Task.sleep(for: .milliseconds(500))
            model = AppModel(runtimeDirectory: statePath.path)
            host = NSHostingView(rootView: ContentView().environmentObject(model).frame(width: 1200, height: 840))
            host.sizingOptions = []
            window.contentView = host
            window.setContentSize(NSSize(width: 1200, height: 840))
            await model.start()
            print("Native journey: restarted helper connected")
            XCTAssertEqual(model.tabPosition, "top")
            XCTAssertEqual(model.activeTab?.threadId, createdID)
            try await waitUntil(timeout: 20) { model.activeMessages.contains { $0.kind == .assistant && $0.text.contains("NATIVE_HAND_READY") } }
            try await waitUntil(timeout: 25) { model.state.hands.contains { $0.id == automaticHandID && $0.status == "connected" } }
            let restartedHand = try XCTUnwrap(model.state.hands.first { $0.id == folderHand.id })
            XCTAssertEqual(restartedHand.status, "stopped", "Only the default device Hand restarts automatically")
            await model.startHand(restartedHand.id)
            print("Native journey: Hand reconnected")
            XCTAssertEqual(model.state.hands.first { $0.id == restartedHand.id }?.status, "connected")
            await model.send("Read native-evidence.txt again from the same selected Hand. Reply with its contents followed by NATIVE_SECOND_TURN_READY.")
            try await waitUntil(timeout: 60) { model.activeMessages.filter { $0.kind == .user }.count >= 2 && !model.isRunning && model.pendingMessages().isEmpty }
            print("Native journey: second turn completed")
            guard model.activeMessages.contains(where: { $0.kind == .assistant && $0.text.contains("native-hand-roundtrip-ok") && $0.text.contains("NATIVE_SECOND_TURN_READY") }) else { throw RuntimeFailure(message: "The reconnected Hand did not complete the second file read: " + (model.activeMessages.last(where: { $0.kind == .assistant })?.text ?? "No assistant reply")) }
            try await Task.sleep(for: .milliseconds(150))
            try capture(host, to: evidence.appendingPathComponent("native-durable-thread.png"))
            await model.send("Call exec_command on the selected Hand with command sleep 25 and yield_time_ms 30000. Wait for it to finish before replying SLOW_TURN_FINISHED.")
            try await waitUntil(timeout: 30) { model.activeMessages.contains { $0.kind == .tool && $0.status == "running" && $0.text.contains("sleep 25") } }
            let predecessor = try XCTUnwrap(model.controllableTurns().first)
            await model.send("The priority changed. Reply exactly NATIVE_STEERING_READY without using tools.")
            let queued = try XCTUnwrap(model.pendingMessages().first)
            XCTAssertEqual(queued.phase, .queued)
            XCTAssertEqual(queued.predecessor, predecessor)
            try capture(host, to: evidence.appendingPathComponent("native-live-steer-ready.png"))
            await model.steerNow(queued.id)
            try await waitUntil(timeout: 65) { model.pendingMessages().isEmpty && !model.isRunning && model.activeMessages.contains { $0.turnId == queued.id && $0.kind == .assistant && $0.text.contains("NATIVE_STEERING_READY") } }
            XCTAssertEqual(model.activeSnapshot?.events.filter { $0.data["type"].string == "turn_accepted" && $0.turnId == queued.id }.count, 1, "The live follow-up was accepted exactly once")
            XCTAssertTrue(model.activeSnapshot?.events.contains { $0.data["type"].string == "turn_cancelled" && $0.turnId == predecessor } == true)
            XCTAssertEqual(model.activeMessages.filter { $0.kind == .user && $0.turnId == queued.id }.count, 1)
            try capture(host, to: evidence.appendingPathComponent("native-live-steered.png"))
            print("Native journey: queued follow-up steered once and completed")
            await model.stopHand(restartedHand.id)
            XCTAssertEqual(model.state.hands.first { $0.id == restartedHand.id }?.status, "stopped")
            let report: [String: Any] = ["connectedSeconds": connectedAt, "firstTurnSeconds": firstTurnAt, "totalSeconds": Date().timeIntervalSince(start), "agentId": createdID ?? "", "nativeEditor": true, "shiftReturn": true, "returnSubmit": true, "tabs": true, "persistedTopTabs": true, "folderHand": true, "realFileRoundtrip": true, "twoDurableTurns": true, "steerNow": true, "queuedAcceptedOnce": true, "stoppedHand": true]
            try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]).write(to: evidence.appendingPathComponent("native-journey.json"))
            await model.prepareToQuit()
            window.orderOut(nil)
            if let createdID { try await removeTestThread(createdID, origin: model.state.baseUrl, key: apiKey) }
            try FileManager.default.removeItem(at: directory)
        } catch {
            try? capture(host, to: evidence.appendingPathComponent("native-failure.png"))
            try? JSONEncoder().encode(model.activeSnapshot?.events ?? []).write(to: evidence.appendingPathComponent("native-failure-events.json"))
            try? JSONEncoder().encode(model.state.hands).write(to: evidence.appendingPathComponent("native-failure-hands.json"))
            let id = createdID ?? model.activeTab?.threadId
            await model.prepareToQuit(); window.orderOut(nil)
            if let id { try? await removeTestThread(id, origin: model.state.baseUrl, key: apiKey) }
            throw error
        }
    }
    @MainActor
    private func waitUntil(timeout: TimeInterval, condition: () -> Bool) async throws {
        let end = Date().addingTimeInterval(timeout)
        while !condition() {
            guard Date() < end else { throw RuntimeFailure(message: "The live native journey did not reach its expected state within \(Int(timeout)) seconds.") }
            try await Task.sleep(for: .milliseconds(100))
        }
    }
    @MainActor
    private func findEditor(_ view: NSView) -> ComposerTextView? {
        if let editor = view as? ComposerTextView { return editor }
        for child in view.subviews { if let found = findEditor(child) { return found } }
        return nil
    }
    @MainActor
    private func capture(_ view: NSView, to url: URL) throws {
        view.layoutSubtreeIfNeeded(); view.displayIfNeeded()
        let bitmap = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: url)
        let attachment = XCTAttachment(contentsOfFile: url); attachment.lifetime = .keepAlways; add(attachment)
    }
    private func removeTestThread(_ id: String, origin: String, key: String) async throws {
        var request = URLRequest(url: try XCTUnwrap(URL(string: origin + "/v1/agents/" + id)))
        request.httpMethod = "DELETE"; request.setValue("Bearer " + key, forHTTPHeaderField: "Authorization")
        let (_, response) = try await URLSession.shared.data(for: request)
        XCTAssertTrue([200, 204, 404].contains((response as? HTTPURLResponse)?.statusCode ?? 0), "Remove only the thread created by this test")
    }
}

private final class EvidenceWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}
