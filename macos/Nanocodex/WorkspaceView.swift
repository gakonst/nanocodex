import AppKit
import QuartzCore
import SwiftUI
import NanocodexRemote
import UniformTypeIdentifiers

struct WorkspacePane: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var docking: PaneDock? { model.paneDropPreview(for: tab.id) }
    @State private var dropGeometry = PaneDropGeometry()
    let tab: WorkspaceTab
    var update: WorkspaceUpdate { model.update(for: tab) }
    var selected: Bool { model.activeTabID == tab.id }
    var status: String {
        if model.threadError(tab.id) != nil { return "Couldn’t load" }
        if model.pendingMessages(tab.id).contains(where: { $0.phase == .failed }) { return "Message needs retry" }
        if update.running { return "Working" }
        if !update.checked { return tab.threadId == nil ? "New agent" : "Connecting" }
        if update.needsAttention(tab) { return update.failed ? "Needs attention" : "Ready to review" }
        return "Up to date"
    }
    var body: some View {
        VStack(spacing: 0) {
            if model.isTiled || (model.tiledTabIDs.count > 1 && model.tiledTabIDs.contains(tab.id)) {
            HStack(spacing: 9) {
                PaneDragHandle(model: model, id: tab.id).frame(width: 18, height: 26)
                    .help("Drag to an edge to split, or to the center to swap panes")
                Circle().fill(model.hasAttentionError(tab) ? Color.orange : update.running ? Color.green : update.needsAttention(tab) ? Color.accentColor : Color.secondary.opacity(0.35)).frame(width: 6, height: 6).help(status)
                Text(model.title(tab)).font(.system(size: 12, weight: .medium)).lineLimit(1)
                Spacer(minLength: 4)
                if update.running { Text("Working").font(.system(size: 10)).foregroundStyle(.secondary) }
                Button { model.review(tab.id, seen: true) } label: { Image(systemName: "checkmark") }
                    .help("Mark update seen (⌘D)").accessibilityIdentifier("pane-seen-\(tab.id)")
                Menu {
                    Button("Split Right") { model.select(tab.id); model.splitAgent(axis: "horizontal") }
                    Button("Split Below") { model.select(tab.id); model.splitAgent(axis: "vertical") }
                    Divider()
                    Button("Rename…") { model.renameTab(tab) }
                    Button("Move left") { model.select(tab.id); model.movePane(-1) }
                    Button("Move right") { model.select(tab.id); model.movePane(1) }
                    Button("Focus this agent") { model.focusOnly(tab.id) }
                    Button("Revisit Later") { model.review(tab.id, seen: false) }
                    if model.isTiled { Button("Move to Separate Tab") { model.removePane(tab.id); model.selectWorkspace(PaneNode(id: tab.id)) } }
                    Divider()
                    Button("Close Agent") { model.closeTab(tab.id) }
                } label: { Image(systemName: "ellipsis") }.menuStyle(.borderlessButton).fixedSize().help("Pane actions")
                if model.isTiled {
                    Button { model.removePane(tab.id) } label: { Image(systemName: "xmark").font(.system(size: 10)) }
                        .help("Remove pane · keep agent in its own tab").accessibilityIdentifier("remove-pane-" + tab.id)
                }
            }.buttonStyle(.plain).foregroundStyle(.secondary).padding(.horizontal, 14).frame(height: 38)
                .background(Color(nsColor: .windowBackgroundColor).opacity(0.5))
                .contentShape(Rectangle())
                .onTapGesture { model.select(tab.id); model.enterNavigation() }
            Divider().opacity(0.45)
            }
            ChatView()
        }
        .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 16))
        .clipShape(RoundedRectangle(cornerRadius: 16))
        .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(model.isTiled ? (selected ? Color.accentColor.opacity(0.45) : Color.primary.opacity(0.09)) : .clear, lineWidth: 1))
        .overlay {
            GeometryReader { geometry in
                if let docking {
                    PaneDockPreview(edge: docking, size: geometry.size)
                        .transition(.opacity)
                }
            }.allowsHitTesting(false)
        }
        .onGeometryChange(for: CGSize.self) { $0.size } action: { dropGeometry.size = $0 }
        .onDrop(of: [WorkspaceDrag.paneType], delegate: PaneDockDelegate(model: model, target: tab.id, geometry: dropGeometry))
        .animation(reduceMotion ? nil : .smooth(duration: 0.18), value: docking)
        .background(PaneActivation { model.select(tab.id) })
        .accessibilityElement(children: .contain).accessibilityIdentifier("workspace-pane-\(tab.id)")
    }
}



/// Activate before AppKit dispatches a click, including clicks inside native editors.
private struct PaneActivation: NSViewRepresentable {
    var activate: () -> Void
    func makeNSView(context: Context) -> ActivationView { let view = ActivationView(); view.activate = activate; return view }
    func updateNSView(_ view: ActivationView, context: Context) { view.activate = activate }
    final class ActivationView: NSView {
        var activate: (() -> Void)?
        private var monitor: Any?
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let monitor { NSEvent.removeMonitor(monitor); self.monitor = nil }
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
                if let self, event.window === self.window, self.visibleRect.contains(self.convert(event.locationInWindow, from: nil)) { self.activate?() }
                return event
            }
        }
        deinit { if let monitor { NSEvent.removeMonitor(monitor) } }
    }
}

struct TiledWorkspaceView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var navigationTabs: [WorkspaceTab] { model.isTiled ? model.canvasTabs : model.visibleTabs }
    private var position: Int { navigationTabs.firstIndex { $0.id == model.activeTabID } ?? 0 }
    var body: some View {
        VStack(spacing: 0) {
            if model.canvasTabs.isEmpty {
                ContentUnavailableView {
                    Label(model.workspaceFilter == .inbox ? "Inbox Zero" : "Nothing running", systemImage: "tray")
                } description: {
                    Text("Your agents and drafts are waiting in All.")
                } actions: {
                    Button("View all agents") { model.setFilter(.all) }
                    Button("New agent") { model.newTab() }
                }.accessibilityIdentifier("workspace-empty")
            } else if model.isTiled {
                SplitCanvas(model: model).padding(.horizontal, 8).padding(.bottom, 6)
                    .accessibilityIdentifier("tiled-workspace")
            } else {
                InboxPager(model: model, reduceMotion: reduceMotion)
                    .padding(.horizontal, 14).padding(.bottom, 2)
                    .accessibilityIdentifier("inbox-pager")
            }
            HStack(spacing: 12) {
                Button { model.cyclePane(-1) } label: { Image(systemName: "chevron.left").frame(width: 24, height: 28) }
                    .disabled(position == 0).help("Previous agent (⌘⌥←)").accessibilityIdentifier("previous-agent")
                Text("\(navigationTabs.isEmpty ? 0 : position + 1) / \(navigationTabs.count)").font(.system(size: 10)).monospacedDigit()
                Button { model.cyclePane(1) } label: { Image(systemName: "chevron.right").frame(width: 24, height: 28) }
                    .disabled(position >= navigationTabs.count - 1).help("Next agent (⌘⌥→)").accessibilityIdentifier("next-agent")
                Divider().frame(height: 22).padding(.horizontal, 4)
                Button {
                    if model.workspaceFocus == .writing { model.enterNavigation() } else { model.focusComposer() }
                } label: {
                    Label(model.workspaceFocus == .writing ? "Writing" : "Navigate", systemImage: model.workspaceFocus == .writing ? "pencil" : "arrow.left.arrow.right")
                        .font(.system(size: 11, weight: .medium)).foregroundStyle(Color.accentColor)
                }.accessibilityIdentifier("workspace-keyboard-mode")
                    .help(model.workspaceFocus == .writing ? "Esc to navigate" : "v / h split · arrows move · Enter write · ? help")
                Text(model.workspaceFocus == .writing ? "Esc" : "Enter to write")
                    .font(.system(size: 10)).foregroundStyle(.secondary)
            }.buttonStyle(.plain).foregroundStyle(.secondary).padding(.vertical, 5)
        }.background(Color(nsColor: .windowBackgroundColor).opacity(0.45))
            .background(WorkspaceKeyboard(model: model))
            .onExitCommand { model.enterNavigation() }
            .onReceive(NotificationCenter.default.publisher(for: NSApplication.didResignActiveNotification)) { _ in model.cancelPaneDrag() }
            .onReceive(NotificationCenter.default.publisher(for: NSWindow.didResignKeyNotification)) { _ in model.cancelPaneDrag() }
            .sheet(isPresented: $model.showingKeyboardHelp) { WorkspaceKeyboardHelp() }
    }
}

/// Keep arrangement actions in the native toolbar, without a second control
/// strip competing with tabs or taking space away from the conversation.
struct WorkspaceLayoutMenu: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        Menu {
            Button("Split Right") { model.splitAgent(axis: "horizontal") }
            Button("Split Below") { model.splitAgent(axis: "vertical") }
            Button("Open Existing Agent Beside…") { model.showingPanePicker = true }
            if model.isTiled {
                Button("Focus This Agent") { model.focusOnly(model.activeTabID) }
            } else if model.tiledTabIDs.contains(model.activeTabID), model.tiledTabIDs.count > 1 {
                Button("Resume Layout") { model.toggleFocusMode() }
            }
            Divider()
            if let tab = model.activeTab {
                Button("Rename Agent…") { model.renameTab(tab) }
                Button("Mark Update Seen") { model.review(tab.id, seen: true) }
                Button("Revisit Later") { model.review(tab.id, seen: false) }
                Divider()
            }
            Picker("Show Conversations", selection: Binding(get: { model.workspaceFilter }, set: { model.setFilter($0) })) {
                ForEach(WorkspaceFilter.allCases, id: \.self) { filter in
                    Text(filter.rawValue).tag(filter)
                }
            }.pickerStyle(.menu)
        } label: { Label("Arrange conversations", systemImage: "rectangle.split.2x2") }
            .help("Split, arrange, or focus conversations")
            .accessibilityLabel("Arrange conversations")
            .accessibilityIdentifier("workspace-layout")
            .popover(isPresented: $model.showingPanePicker) { PanePickerView() }
    }
}

/// A real first responder keeps navigation keys out of text editors and sheets.
/// Escape moves here; Enter explicitly gives the active composer the keyboard.
struct WorkspaceKeyboard: NSViewRepresentable {
    @ObservedObject var model: AppModel
    func makeNSView(context: Context) -> WorkspaceKeyboardView {
        let view = WorkspaceKeyboardView(); view.model = model
        view.setAccessibilityIdentifier("workspace-navigation")
        view.setAccessibilityElement(true); view.setAccessibilityRole(.group)
        view.setAccessibilityLabel("Thread navigation. Tab or arrows to move; Enter to write.")
        return view
    }
    func updateNSView(_ view: WorkspaceKeyboardView, context: Context) {
        view.model = model
        view.requestFocus()
    }
}

final class WorkspaceKeyboardView: NSView {
    weak var model: AppModel?
    private var lastFocusRequest = 0
    private var zoomMonitor: Any?
    private var typingTabID: String?
    private var pendingTyping: [NSEvent] = []
    override var acceptsFirstResponder: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let zoomMonitor { NSEvent.removeMonitor(zoomMonitor); self.zoomMonitor = nil }
        if window != nil {
            zoomMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                guard let self, event.window === self.window, self.window?.attachedSheet == nil else { return event }
                return self.handleZoomCommand(event) ? nil : event
            }
        }
        requestFocus()
    }
    deinit { if let zoomMonitor { NSEvent.removeMonitor(zoomMonitor) } }
    @discardableResult func handleZoomCommand(_ event: NSEvent) -> Bool {
        // The remote canvas owns keyboard equivalents while it has control.
        // Browser zoom inside a remote desktop must not resize this workspace.
        guard !(window?.firstResponder is MacRemoteCanvas) else { return false }
        guard let model, event.modifierFlags.contains(.command), event.modifierFlags.intersection([.control, .option]).isEmpty else { return false }
        switch event.charactersIgnoringModifiers {
        case "=", "+": model.changeZoom(1)
        case "-": model.changeZoom(-1)
        case "0": model.resetZoom()
        default: return false
        }
        return true
    }
    static func find(in view: NSView?) -> WorkspaceKeyboardView? {
        guard let view else { return nil }
        if let navigation = view as? WorkspaceKeyboardView { return navigation }
        for child in view.subviews {
            if let navigation = find(in: child) { return navigation }
        }
        return nil
    }
    private func editor(in view: NSView?, tabID: String) -> ComposerTextView? {
        guard let view else { return nil }
        if let editor = view as? ComposerTextView, editor.workspaceTabID == tabID { return editor }
        for child in view.subviews {
            if let editor = editor(in: child, tabID: tabID) { return editor }
        }
        return nil
    }
    func flushTyping(to editor: ComposerTextView) {
        let events = pendingTyping
        pendingTyping = []
        guard let model, model.workspaceFocus == .writing, model.activeTabID == typingTabID,
              editor.workspaceTabID == typingTabID else { typingTabID = nil; return }
        typingTabID = nil
        for event in events { editor.keyDown(with: event) }
    }
    func requestFocus() {
        guard let model, model.workspaceFocus == .navigation, window != nil,
              lastFocusRequest != model.navigationFocusRequest else { return }
        let request = model.navigationFocusRequest
        lastFocusRequest = request
        pendingTyping = []; typingTabID = nil
        DispatchQueue.main.async { [weak self] in
            guard let self, let model = self.model, model.workspaceFocus == .navigation,
                  model.navigationFocusRequest == request, self.window?.attachedSheet == nil else { return }
            self.window?.makeFirstResponder(self)
        }
    }
    private static func direction(_ key: String) -> PaneDock? {
        switch key.lowercased() { case "h": .left; case "j": .bottom; case "k": .top; case "l": .right; default: nil }
    }
    @discardableResult func handlePaneControl(_ event: NSEvent) -> Bool {
        guard let model, event.modifierFlags.contains(.control), event.modifierFlags.intersection([.command, .option]).isEmpty,
              let direction = Self.direction(event.charactersIgnoringModifiers ?? "") else { return false }
        if event.modifierFlags.contains(.shift) { model.resizeActivePane(direction) }
        else { model.navigatePane(direction); requestFocus() }
        return true
    }
    override func keyDown(with event: NSEvent) {
        guard let model else { super.keyDown(with: event); return }
        if handlePaneControl(event) { return }
        guard event.modifierFlags.intersection([.command, .control, .option]).isEmpty else { super.keyDown(with: event); return }
        // A page may still be mounting when Enter is followed immediately by
        // typing. Keep those events until that exact pane's editor is ready.
        if model.workspaceFocus == .writing, event.keyCode != 53, typingTabID == model.activeTabID {
            pendingTyping.append(event); return
        }
        if model.workspaceFocus == .navigation {
            let key = event.charactersIgnoringModifiers ?? ""
            if let direction = Self.direction(key), event.modifierFlags.contains(.shift) { model.resizeActivePane(direction); return }
            switch event.characters {
            case "v", "%": model.splitAgent(axis: "horizontal", focusEditor: false); requestFocus(); return
            case "h", "\"": model.splitAgent(axis: "vertical", focusEditor: false); requestFocus(); return
            case "j": model.navigatePane(.bottom); return
            case "k": model.navigatePane(.top); return
            case "l": model.navigatePane(.right); return
            case "z": if model.tiledTabIDs.count > 1 { model.toggleFocusMode() }; return
            case "x": model.closeTab(model.activeTabID); model.enterNavigation(); requestFocus(); return
            case "{": model.movePane(-1); return
            case "}": model.movePane(1); return
            case "?": model.showingKeyboardHelp = true; return
            case "s": if model.remoteService != nil { model.showingScreens.toggle() }; return
            default: break
            }
        }
        switch event.keyCode {
        case 53: pendingTyping = []; typingTabID = nil; model.enterNavigation()
        case 48: model.cyclePane(event.modifierFlags.contains(.shift) ? -1 : 1, focusEditor: false)
        case 123: model.navigatePane(.left)
        case 124: model.navigatePane(.right)
        case 125: model.navigatePane(.bottom)
        case 126: model.navigatePane(.top)
        case 36, 76:
            typingTabID = model.activeTabID
            model.focusComposer()
            if let editor = editor(in: window?.contentView, tabID: model.activeTabID), window?.makeFirstResponder(editor) == true { flushTyping(to: editor) }
        default: super.keyDown(with: event)
        }
    }
}

private struct WorkspaceKeyboardHelp: View {
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Workspace shortcuts").font(.title2.bold())
            Text("Press Esc to navigate. Press Enter to write in the selected pane.").foregroundStyle(.secondary)
            Grid(alignment: .leading, horizontalSpacing: 28, verticalSpacing: 12) {
                ForEach([("v / %", "Split right"), ("h / \"", "Split below"), ("← ↑ ↓ → / Ctrl H J K L", "Select pane in that direction"), ("Tab / Shift Tab", "Next / previous pane"), ("Shift H J K L", "Resize divider"), ("Ctrl Shift H J K L", "Resize while writing"), ("z", "Zoom pane / restore layout"), ("x", "Close pane (⌘⇧T to reopen)"), ("{ / }", "Swap previous / next pane"), ("s / ⌘⌥S", "Show / hide remote screens"), ("⌘[ / ⌘]", "Back / forward"), ("⌘⇧O", "Open tab overview"), ("⌘ + / ⌘ − / ⌘ 0", "Zoom in / out / actual size")], id: \.0) { key, action in
                    GridRow { Text(key).font(.system(.body, design: .monospaced)); Text(action) }
                }
            }
            HStack { Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }
        }.padding(28).frame(width: 540)
    }
}

private struct PanePickerView: View {
    @EnvironmentObject private var model: AppModel
    @State private var query = ""
    @State private var selectedID: String?
    @FocusState private var searching: Bool
    private var choices: [WorkspaceTab] {
        model.tabs.filter { $0.id != model.activeTabID && (!model.isTiled || !model.tiledTabIDs.contains($0.id)) && (query.isEmpty || model.title($0).localizedCaseInsensitiveContains(query)) }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Open beside this agent").font(.headline)
            Text("Keep this conversation here and bring another into view.").font(.caption).foregroundStyle(.secondary)
            Picker("Split direction", selection: $model.splitAxis) {
                Label("Right", systemImage: "rectangle.split.2x1").tag("horizontal")
                Label("Below", systemImage: "rectangle.split.1x2").tag("vertical")
            }.pickerStyle(.segmented)
            TextField("Find an open agent", text: $query).textFieldStyle(.roundedBorder).focused($searching)
                .onSubmit { if let tab = choices.first(where: { $0.id == selectedID }) ?? choices.first { model.openBeside(tab.id) } }
                .onKeyPress(.upArrow) { moveSelection(-1); return .handled }
                .onKeyPress(.downArrow) { moveSelection(1); return .handled }
                .onChange(of: query) { selectedID = choices.first?.id }
            ScrollViewReader { proxy in
              ScrollView {
                LazyVStack(alignment: .leading, spacing: 3) {
                    ForEach(choices) { tab in
                        Button { model.openBeside(tab.id) } label: {
                            HStack(spacing: 9) {
                                Image(systemName: "bubble.left")
                                Text(model.title(tab)).lineLimit(2)
                                Spacer(); Image(systemName: "plus").foregroundStyle(.tertiary)
                            }.padding(9).frame(maxWidth: .infinity, alignment: .leading).background(selectedID == tab.id ? Color.accentColor.opacity(0.10) : .clear, in: RoundedRectangle(cornerRadius: 7)).contentShape(Rectangle())
                        }.buttonStyle(.plain).accessibilityIdentifier("add-pane-" + tab.id).id(tab.id)
                    }
                    if choices.isEmpty { Text("No other open agents").font(.callout).foregroundStyle(.secondary).padding(9) }
                }
              }
              .onChange(of: selectedID) { _, id in if let id { proxy.scrollTo(id, anchor: .center) } }
            }.frame(maxHeight: 220)
            Divider()
            Button { model.newTab(beside: true) } label: { Label("New agent beside this one", systemImage: "square.and.pencil") }
                .buttonStyle(.plain).padding(9).accessibilityIdentifier("new-agent-beside")
        }.padding(18).frame(width: 320)
            .onAppear { selectedID = choices.first?.id; searching = true }
            .onExitCommand { model.showingPanePicker = false }
    }
    private func moveSelection(_ offset: Int) {
        guard !choices.isEmpty else { return }
        let index = choices.firstIndex { $0.id == selectedID } ?? 0
        selectedID = choices[min(choices.count - 1, max(0, index + offset))].id
    }
}

/// The page is keyed by an agent, while its data remains live during a swipe.
private struct AgentPage: View {
    @ObservedObject var model: AppModel
    let id: String
    var body: some View {
        if let tab = model.tab(id) {
            WorkspacePane(tab: tab).environmentObject(model).environment(\.workspaceTabID, id).id(id)
        }
    }
}

/// AppKit owns swipe tracking, rubber-banding, cancelled gestures and snapshots.
/// AppKit retains pages by thread identity, including their editor and viewport.
struct InboxPager: NSViewControllerRepresentable {
    @ObservedObject var model: AppModel
    var reduceMotion: Bool
    func makeCoordinator() -> Coordinator { Coordinator(model: model) }
    func makeNSViewController(context: Context) -> InboxPageController {
        let controller = InboxPageController()
        controller.view = NSView()
        controller.transitionStyle = .horizontalStrip
        controller.delegate = context.coordinator
        controller.installGestureRouting()
        context.coordinator.synchronize(controller)
        return controller
    }
    func updateNSViewController(_ controller: InboxPageController, context: Context) {
        controller.reduceMotion = reduceMotion
        context.coordinator.synchronize(controller)
    }
    static func dismantleNSViewController(_ controller: InboxPageController, coordinator: Coordinator) {
        controller.wheelRouter = nil; controller.completeTransition(); controller.delegate = nil
    }
    @MainActor
    final class Coordinator: NSObject, NSPageControllerDelegate {
        let model: AppModel
        private var transitioning = false
        private var synchronizing = false
        private var pages: [String: NSHostingController<AnyView>] = [:]
        private var recentPages: [String] = []
        init(model: AppModel) { self.model = model }
        func synchronize(_ controller: InboxPageController) {
            guard !transitioning else { return }
            let ids = model.visibleTabs.map(\.id)
            guard !ids.isEmpty else { return }
            let index = ids.firstIndex(of: model.activeTabID) ?? 0
            if recentPages.contains(ids[index]) { recentPages.removeAll { $0 == ids[index] }; recentPages.append(ids[index]) }
            synchronizing = true
            defer { synchronizing = false }
            if controller.arrangedObjects as? [String] != ids {
                pages = pages.filter { ids.contains($0.key) }
                recentPages.removeAll { !ids.contains($0) }
                controller.arrangedObjects = ids
                controller.selectedIndex = index
                finish(controller)
            } else if controller.selectedIndex != index {
                // Direct selection is immediate. Only an actual swipe animates
                // through neighboring conversations; rapid clicks never queue.
                controller.selectedIndex = index
                finish(controller)
            }
        }
        private func finish(_ controller: NSPageController) {
            controller.selectedViewController?.view.layoutSubtreeIfNeeded()
            controller.completeTransition()
        }
        func pageController(_ pageController: NSPageController, identifierFor object: Any) -> NSPageController.ObjectIdentifier { object as? String ?? "empty" }
        func pageController(_ pageController: NSPageController, viewControllerForIdentifier identifier: NSPageController.ObjectIdentifier) -> NSViewController {
            recentPages.removeAll { $0 == identifier }; recentPages.append(identifier)
            if let cached = pages[identifier] { return cached }
            let host = NSHostingController(rootView: AnyView(AgentPage(model: model, id: identifier)))
            host.sizingOptions = []
            host.view.autoresizingMask = [.width, .height]
            pages[identifier] = host
            // AppKit's snapshot cache can discard a distant page immediately.
            // Retain a bounded working set of real editors and viewports too.
            while recentPages.count > 8 { pages.removeValue(forKey: recentPages.removeFirst()) }
            return host
        }
        func pageController(_ pageController: NSPageController, prepare viewController: NSViewController, with object: Any?) {
            guard object is String else { return }
            viewController.view.frame = pageController.view.bounds
            // SwiftUI must finish layout before AppKit snapshots or reveals it.
            viewController.view.layoutSubtreeIfNeeded()
        }
        func pageController(_ pageController: NSPageController, didTransitionTo object: Any) {
            guard !synchronizing, let id = object as? String else { return }
            // Swipe navigation keeps read status and drafts intact, and does not
            // focus the editor while the user is reading the conversation.
            if transitioning { return }
            DispatchQueue.main.async { [weak self, weak pageController] in
                guard let pageController, pageController.arrangedObjects[safe: pageController.selectedIndex] as? String == id else { return }
                self?.model.select(id)
            }
        }
        func pageControllerWillStartLiveTransition(_ pageController: NSPageController) { transitioning = true }
        func pageControllerDidEndLiveTransition(_ pageController: NSPageController) {
            finish(pageController); transitioning = false
            guard let controller = pageController as? InboxPageController,
                  let id = controller.arrangedObjects[safe: controller.selectedIndex] as? String else { return }
            model.select(id)
            synchronize(controller)
        }
    }
}

final class InboxPageController: NSPageController {
    var wheelRouter: WorkspaceWheelRouter?
    var reduceMotion = false
    private var lastWheelNavigation: TimeInterval = 0
    func installGestureRouting() {
        wheelRouter = WorkspaceWheelRouter(view: view) { [weak self] event in self?.scrollWheel(with: event) }
    }
    override func wantsScrollEventsForSwipeTracking(on axis: NSEvent.GestureAxis) -> Bool { axis == .horizontal }
    override func scrollWheel(with event: NSEvent) {
        if event.hasPreciseScrollingDeltas && (!event.phase.isEmpty || !event.momentumPhase.isEmpty) {
            super.scrollWheel(with: event)
            return
        }
        // Mouse wheels and accessibility scroll events have no gesture phases.
        // Navigate once per burst, while precise trackpads keep AppKit tracking.
        let delta = event.scrollingDeltaX == 0 && event.modifierFlags.contains(.shift) ? event.scrollingDeltaY : event.scrollingDeltaX
        let now = ProcessInfo.processInfo.systemUptime
        guard abs(delta) > 0, now - lastWheelNavigation > 0.3 else { return }
        let next = selectedIndex + (delta < 0 ? 1 : -1)
        guard arrangedObjects.indices.contains(next) else { return }
        lastWheelNavigation = now
        if reduceMotion { selectedIndex = next; completeTransition() }
        else if delta < 0 { navigateForward(nil) } else { navigateBack(nil) }
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}

final class WorkspaceWheelRouter {
    private var monitor: Any?
    private var horizontal: Bool?
    init(view: NSView, handle: @escaping (NSEvent) -> Void) {
        monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { [weak self, weak view] event in
            guard let self, let view, let window = view.window, event.window === window,
                  view.visibleRect.contains(view.convert(event.locationInWindow, from: nil)) else { return event }
            if event.phase.contains(.began) { self.horizontal = nil }
            defer {
                if event.phase.contains(.ended) || event.phase.contains(.cancelled) || event.momentumPhase.contains(.ended) || (event.phase.isEmpty && event.momentumPhase.isEmpty) { self.horizontal = nil }
            }
            if self.horizontal == nil, abs(event.scrollingDeltaX) + abs(event.scrollingDeltaY) > 0 {
                self.horizontal = event.modifierFlags.contains(.shift) || abs(event.scrollingDeltaX) > abs(event.scrollingDeltaY) * 1.2
            }
            guard self.horizontal == true else { return event }
            var hit = window.contentView?.hitTest(event.locationInWindow)
            while let candidate = hit, candidate !== view {
                if let scroll = candidate as? NSScrollView, scroll.hasHorizontalScroller,
                   (scroll.documentView?.bounds.width ?? 0) > scroll.contentSize.width + 1 { return event }
                hit = candidate.superview
            }
            handle(event)
            return nil
        }
    }
    deinit { if let monitor { NSEvent.removeMonitor(monitor) } }
}

/// Keeps the same native editor/viewports while split ratios change. Only frames
/// change during a divider drag; no SwiftUI tree is rebuilt per mouse event.
struct SplitCanvas: NSViewRepresentable {
    @ObservedObject var model: AppModel
    func makeNSView(context: Context) -> AgentSplitScroll {
        let scroll = AgentSplitScroll(); scroll.drawsBackground = false
        scroll.hasHorizontalScroller = true; scroll.hasVerticalScroller = true; scroll.autohidesScrollers = true
        scroll.documentView = AgentSplitSurface(model: model)
        return scroll
    }
    func updateNSView(_ scroll: AgentSplitScroll, context: Context) {
        guard let surface = scroll.documentView as? AgentSplitSurface, let tree = model.activePaneLayout else { return }
        surface.synchronize(tree)
        let minimum = surface.minimumSize(tree)
        let size = NSSize(width: max(scroll.contentSize.width, minimum.width), height: max(scroll.contentSize.height, minimum.height))
        if surface.frame.size != size { surface.setFrameSize(size); surface.needsLayout = true }
        if surface.selectedID != model.activeTabID {
            surface.selectedID = model.activeTabID
            if let host = surface.hosts[model.activeTabID] { surface.scrollToVisible(host.frame) }
        }
    }
}

final class AgentSplitScroll: NSScrollView {
    private var previousViewportSize = NSSize.zero
    override func layout() {
        super.layout()
        guard let surface = documentView as? AgentSplitSurface, let tree = surface.tree else { return }
        let minimum = surface.minimumSize(tree)
        let size = NSSize(width: max(contentSize.width, minimum.width), height: max(contentSize.height, minimum.height))
        if surface.frame.size != size { surface.setFrameSize(size); surface.needsLayout = true }
        if previousViewportSize != contentSize {
            previousViewportSize = contentSize
            // Reveal after the child has laid out its new frames; resizing a
            // window must not strand the active composer below the viewport.
            DispatchQueue.main.async { [weak surface] in
                guard let surface, let host = surface.hosts[surface.selectedID] else { return }
                surface.scrollToVisible(host.frame)
            }
        }
    }
}

final class AgentSplitSurface: NSView {
    let model: AppModel
    var tree: PaneNode?
    var hosts: [String: NSHostingView<AnyView>] = [:]
    var dividers: [String: AgentSplitDivider] = [:]
    var selectedID = ""
    private var animateNextLayout = false
    private var animatingPlacement = false
    override var isFlipped: Bool { true }
    init(model: AppModel) { self.model = model; super.init(frame: .zero) }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    func synchronize(_ next: PaneNode) {
        guard tree != next else { return }
        animateNextLayout = tree != nil && window?.occlusionState.contains(.visible) == true && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        tree = next
        let leaves = Set(next.leaves)
        for id in Array(hosts.keys) where !leaves.contains(id) { hosts.removeValue(forKey: id)?.removeFromSuperview() }
        for id in next.leaves where hosts[id] == nil {
            let host = NSHostingView(rootView: AnyView(AgentPage(model: model, id: id)))
            host.sizingOptions = []; host.wantsLayer = true; hosts[id] = host; addSubview(host)
        }
        func splitIDs(_ node: PaneNode) -> [String] { node.children.isEmpty ? [] : [node.id] + node.children.flatMap(splitIDs) }
        let ids = Set(splitIDs(next))
        for id in Array(dividers.keys) where !ids.contains(id) { dividers.removeValue(forKey: id)?.removeFromSuperview() }
        for id in ids where dividers[id] == nil {
            let divider = AgentSplitDivider(); divider.splitID = id; divider.surface = self
            dividers[id] = divider; addSubview(divider)
        }
        needsLayout = true
    }
    func minimumSize(_ node: PaneNode) -> NSSize {
        guard node.children.count == 2 else { return NSSize(width: 360, height: 300) }
        let a = minimumSize(node.children[0]), b = minimumSize(node.children[1])
        return node.axis == "horizontal" ? NSSize(width: a.width + b.width + 8, height: max(a.height, b.height)) : NSSize(width: max(a.width, b.width), height: a.height + b.height + 8)
    }
    override func layout() {
        super.layout()
        guard let tree else { return }
        let animate = animateNextLayout; animateNextLayout = false
        animatingPlacement = animate
        NSAnimationContext.runAnimationGroup { context in
            context.duration = animate ? 0.24 : 0
            context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            place(tree, in: bounds)
        }
        animatingPlacement = false
    }
    private func place(_ node: PaneNode, in rect: NSRect) {
        guard node.children.count == 2 else {
            if let host = hosts[node.id] {
                placeHost(host, in: rect, animated: animatingPlacement)
            }
            return
        }
        let horizontal = node.axis == "horizontal"
        let available = (horizontal ? rect.width : rect.height) - 8
        let firstMin = minimumSize(node.children[0]), secondMin = minimumSize(node.children[1])
        let length = max(horizontal ? firstMin.width : firstMin.height, min(available - (horizontal ? secondMin.width : secondMin.height), available * node.fraction))
        let first = NSRect(x: rect.minX, y: rect.minY, width: horizontal ? length : rect.width, height: horizontal ? rect.height : length)
        let gap = NSRect(x: horizontal ? first.maxX : rect.minX, y: horizontal ? rect.minY : first.maxY, width: horizontal ? 8 : rect.width, height: horizontal ? rect.height : 8)
        let second = NSRect(x: horizontal ? gap.maxX : rect.minX, y: horizontal ? rect.minY : gap.maxY, width: horizontal ? available - length : rect.width, height: horizontal ? rect.height : available - length)
        if let divider = dividers[node.id] {
            if animatingPlacement { divider.animator().frame = gap } else { divider.frame = gap }
            divider.horizontal = horizontal; divider.splitRect = rect
            divider.setAccessibilityValue(Int(node.fraction * 100))
            divider.needsDisplay = true
        }
        place(node.children[0], in: first); place(node.children[1], in: second)
    }

    /// Commit the live content's final size once. Only its presentation position
    /// moves, so text does not reflow on every animation frame. Interrupted moves
    /// start at the currently displayed position rather than the previous target.
    func placeHost(_ host: NSHostingView<AnyView>, in rect: NSRect, animated: Bool) {
        // A redundant layout must not cancel an in-flight pane transition.
        // A changed rectangle below still hands geometry back to live resizing.
        if host.frame == rect, host.alphaValue == 1 { return }
        let entering = host.frame.isEmpty
        let position = host.layer?.presentation()?.position ?? host.layer?.position
        let opacity = host.layer?.presentation()?.opacity ?? 1
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        host.layer?.removeAnimation(forKey: "pane-position")
        host.layer?.removeAnimation(forKey: "pane-entry")
        host.frame = rect
        host.alphaValue = 1
        CATransaction.commit()
        guard animated, let layer = host.layer else { return }
        if !entering, let position, position != layer.position {
            let movement = CABasicAnimation(keyPath: "position")
            movement.fromValue = NSValue(point: position)
            movement.toValue = NSValue(point: layer.position)
            movement.duration = 0.24
            movement.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            layer.add(movement, forKey: "pane-position")
        }
        if entering || opacity < 1 {
            let fade = CABasicAnimation(keyPath: "opacity")
            fade.fromValue = entering ? 0 : opacity
            fade.toValue = 1
            fade.duration = 0.24
            layer.add(fade, forKey: "pane-entry")
        }
    }
    func dragSplit(_ id: String, fraction: Double, finished: Bool, animated: Bool = false) {
        animateNextLayout = animated && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        if let tree { self.tree = tree.resizing(id, to: fraction); needsLayout = true; layoutSubtreeIfNeeded() }
        if finished { model.resizeSplit(id, fraction: fraction) }
    }
}

final class AgentSplitDivider: NSView {
    weak var surface: AgentSplitSurface?
    var splitID = ""
    var horizontal = true
    var splitRect = NSRect.zero
    private var tracking: NSTrackingArea?
    private var hovering = false { didSet { needsDisplay = true } }
    private var dragging = false { didSet { needsDisplay = true } }
    private var grabOffset: CGFloat = 0
    private var snapped: CGFloat?
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setAccessibilityElement(true); setAccessibilityRole(.splitter)
        setAccessibilityLabel("Resize panes")
        setAccessibilityHelp("Drag to resize. Double-click to balance. Use increment or decrement to resize with accessibility controls.")
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    override func resetCursorRects() { addCursorRect(bounds, cursor: horizontal ? .resizeLeftRight : .resizeUpDown) }
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self)
        tracking = area; addTrackingArea(area)
    }
    override func mouseEntered(with event: NSEvent) { hovering = true }
    override func mouseExited(with event: NSEvent) { hovering = false }
    override func draw(_ dirtyRect: NSRect) {
        let active = hovering || dragging
        (active ? NSColor.controlAccentColor : NSColor.separatorColor).withAlphaComponent(active ? 0.85 : 0.55).setFill()
        let grip = horizontal ? NSRect(x: bounds.midX - (active ? 2 : 1.5), y: bounds.midY - 22, width: active ? 4 : 3, height: 44)
            : NSRect(x: bounds.midX - 22, y: bounds.midY - (active ? 2 : 1.5), width: 44, height: active ? 4 : 3)
        NSBezierPath(roundedRect: grip, xRadius: 2, yRadius: 2).fill()
    }
    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 2 { surface?.dragSplit(splitID, fraction: 0.5, finished: true, animated: true); return }
        let point = convert(event.locationInWindow, from: nil)
        grabOffset = horizontal ? point.x - bounds.midX : point.y - bounds.midY
        dragging = true
    }
    override func mouseDragged(with event: NSEvent) { track(event, finished: false) }
    override func mouseUp(with event: NSEvent) {
        guard dragging else { return }
        track(event, finished: true); dragging = false; snapped = nil
    }
    private func track(_ event: NSEvent, finished: Bool) {
        guard let surface else { return }
        let point = surface.convert(event.locationInWindow, from: nil)
        let available = max(1, (horizontal ? splitRect.width : splitRect.height) - 8)
        var value = ((horizontal ? point.x - splitRect.minX : point.y - splitRect.minY) - 4 - grabOffset) / available
        let snap = [0.25, 0.5, 0.75].first { abs($0 - value) * available < 6 }
        if let snap {
            value = snap
            if snapped != snap { NSHapticFeedbackManager.defaultPerformer.perform(.alignment, performanceTime: .now) }
        }
        snapped = snap
        surface.dragSplit(splitID, fraction: value, finished: finished)
    }
    override func accessibilityPerformIncrement() -> Bool { adjust(by: 0.05) }
    override func accessibilityPerformDecrement() -> Bool { adjust(by: -0.05) }
    private func adjust(by amount: Double) -> Bool {
        guard let surface else { return false }
        let fraction = (accessibilityValue() as? Int).map { Double($0) / 100 } ?? 0.5
        surface.dragSplit(splitID, fraction: fraction + amount, finished: true, animated: true)
        return true
    }
}


/// Real system glass on current macOS; older systems and accessibility retain
/// an opaque/material control surface. Conversation content stays unfiltered.
private struct WorkspaceGlass: ViewModifier {
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    var cornerRadius: CGFloat
    var enabled: Bool
    var interactive: Bool
    func body(content: Content) -> some View {
        if !enabled { content }
        else if reduceTransparency {
            content.background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: cornerRadius))
        } else if #available(macOS 26.0, *) {
            content.glassEffect(.regular.interactive(interactive), in: RoundedRectangle(cornerRadius: cornerRadius))
        } else {
            content.background(.regularMaterial, in: RoundedRectangle(cornerRadius: cornerRadius))
        }
    }
}

extension View {
    func workspaceGlass(cornerRadius: CGFloat = 12, enabled: Bool = true, interactive: Bool = false) -> some View { modifier(WorkspaceGlass(cornerRadius: cornerRadius, enabled: enabled, interactive: interactive)) }
    func workspaceAction(prominent: Bool = false) -> some View { modifier(WorkspaceAction(prominent: prominent)) }
}

private struct WorkspaceAction: ViewModifier {
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    let prominent: Bool
    @ViewBuilder func body(content: Content) -> some View {
        if #available(macOS 26.0, *), !reduceTransparency {
            if prominent { content.buttonStyle(.glassProminent) }
            else { content.buttonStyle(.glass) }
        } else {
            if prominent { content.buttonStyle(.borderedProminent) }
            else { content.buttonStyle(.bordered) }
        }
    }
}

struct WorkspaceGlassGroup<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View {
        if #available(macOS 26.0, *) { GlassEffectContainer(spacing: 8) { content } }
        else { content }
    }
}

enum WorkspaceDrag {
    static let paneType = UTType(exportedAs: "xyz.paradigm.nanocodex.workspace-pane")
    static let tabType = UTType(exportedAs: "xyz.paradigm.nanocodex.workspace-tab")
}

private struct PaneDragHandle: NSViewRepresentable {
    let model: AppModel
    let id: String
    func makeNSView(context: Context) -> PaneDragHandleView { PaneDragHandleView() }
    func updateNSView(_ view: PaneDragHandleView, context: Context) {
        view.model = model; view.paneID = id
        view.setAccessibilityLabel("Move pane: " + (model.tab(id).map(model.title) ?? "Agent"))
        view.setAccessibilityIdentifier("drag-pane-" + id)
    }
}

final class PaneDragHandleView: NSView, NSDraggingSource {
    weak var model: AppModel?
    var paneID = ""
    private var origin: NSPoint?
    override func resetCursorRects() { addCursorRect(bounds, cursor: .openHand) }
    override func draw(_ dirtyRect: NSRect) {
        NSColor.secondaryLabelColor.withAlphaComponent(0.6).setFill()
        for x in [bounds.midX - 3, bounds.midX + 3] {
            for y in [bounds.midY - 5, bounds.midY, bounds.midY + 5] {
                NSBezierPath(ovalIn: NSRect(x: x - 1, y: y - 1, width: 2, height: 2)).fill()
            }
        }
    }
    override func mouseDown(with event: NSEvent) { origin = event.locationInWindow; model?.select(paneID) }
    override func mouseUp(with event: NSEvent) { origin = nil }
    override func mouseDragged(with event: NSEvent) {
        guard let origin, hypot(event.locationInWindow.x - origin.x, event.locationInWindow.y - origin.y) > 4,
              let model, let tab = model.tab(paneID) else { return }
        self.origin = nil
        let item = NSPasteboardItem()
        item.setString(paneID, forType: NSPasteboard.PasteboardType(WorkspaceDrag.paneType.identifier))
        let dragging = NSDraggingItem(pasteboardWriter: item)
        let image = NSImage(size: NSSize(width: 230, height: 44), flipped: false) { rect in
            NSColor.windowBackgroundColor.withAlphaComponent(0.96).setFill()
            NSBezierPath(roundedRect: rect.insetBy(dx: 1, dy: 1), xRadius: 13, yRadius: 13).fill()
            NSColor.controlAccentColor.withAlphaComponent(0.5).setStroke()
            NSBezierPath(roundedRect: rect.insetBy(dx: 1, dy: 1), xRadius: 13, yRadius: 13).stroke()
            let paragraph = NSMutableParagraphStyle(); paragraph.lineBreakMode = .byTruncatingTail
            (model.title(tab) as NSString).draw(in: NSRect(x: 14, y: 13, width: 202, height: 18), withAttributes: [.font: NSFont.systemFont(ofSize: 12, weight: .medium), .foregroundColor: NSColor.labelColor, .paragraphStyle: paragraph])
            return true
        }
        dragging.setDraggingFrame(NSRect(x: 0, y: bounds.midY - 22, width: 230, height: 44), contents: image)
        model.draggingPaneID = paneID
        let session = beginDraggingSession(with: [dragging], event: event, source: self)
        session.animatesToStartingPositionsOnCancelOrFail = true
    }
    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        context == .withinApplication ? .move : []
    }
    func draggingSession(_ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        model?.cancelPaneDrag(); origin = nil
    }
}

/// Drop hit testing needs the latest size, but a size change does not change
/// the pane's content. A reference avoids a second SwiftUI update per resize.
private final class PaneDropGeometry {
    var size = CGSize.zero
}

private struct PaneDockDelegate: DropDelegate {
    let model: AppModel
    let target: String
    let geometry: PaneDropGeometry
    func validateDrop(info: DropInfo) -> Bool {
        guard let source = model.draggingPaneID else { return false }
        return source != target && model.tab(source) != nil && model.tab(target) != nil
    }
    private func updatePreview(_ info: DropInfo) {
        guard validateDrop(info: info) else { clearPreview(); return }
        model.updatePaneDrop(target: target, edge: PaneDock.destination(at: info.location, size: geometry.size))
    }
    private func clearPreview() {
        if model.paneDropTarget == target { model.updatePaneDrop(target: nil, edge: nil) }
    }
    func dropEntered(info: DropInfo) { updatePreview(info) }
    func dropUpdated(info: DropInfo) -> DropProposal? {
        guard validateDrop(info: info) else { clearPreview(); return DropProposal(operation: .cancel) }
        updatePreview(info)
        return DropProposal(operation: .move)
    }
    func dropExited(info: DropInfo) { clearPreview() }
    func performDrop(info: DropInfo) -> Bool {
        defer { model.cancelPaneDrag() }
        guard validateDrop(info: info), let source = model.draggingPaneID else { return false }
        model.dockPane(source, at: target, edge: PaneDock.destination(at: info.location, size: geometry.size))
        return true
    }
}

private struct PaneDockPreview: View {
    let edge: PaneDock
    let size: CGSize
    private var rect: CGRect {
        switch edge {
        case .left: CGRect(x: 0, y: 0, width: size.width / 2, height: size.height)
        case .right: CGRect(x: size.width / 2, y: 0, width: size.width / 2, height: size.height)
        case .top: CGRect(x: 0, y: 0, width: size.width, height: size.height / 2)
        case .bottom: CGRect(x: 0, y: size.height / 2, width: size.width, height: size.height / 2)
        case .center: CGRect(origin: .zero, size: size).insetBy(dx: 12, dy: 12)
        }
    }
    var body: some View {
        RoundedRectangle(cornerRadius: 14)
            .fill(Color.accentColor.opacity(0.14))
            .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Color.accentColor.opacity(0.75), lineWidth: 2))
            .overlay { Text(edge.label).font(.system(size: 12, weight: .semibold)).padding(.horizontal, 14).padding(.vertical, 9).workspaceGlass() }
            .frame(width: max(0, rect.width - 8), height: max(0, rect.height - 8))
            .position(x: rect.midX, y: rect.midY)
    }
}
