import AppKit
import SwiftUI

struct WorkspacePane: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var dragOffset: CGFloat = 0
    @State private var horizontalDrag: Bool?
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
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    Circle().fill(model.hasAttentionError(tab) ? Color.orange : update.running ? Color.green : update.needsAttention(tab) ? Color.accentColor : Color.secondary.opacity(0.35)).frame(width: 6, height: 6)
                    Text(status).font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
                    if let snapshot = model.snapshot(tab.id), !snapshot.connected {
                        Text("Reconnecting…").font(.system(size: 10)).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Label(model.workspaceFocus == .writing ? "Active · Writing" : "Active · Navigate", systemImage: model.workspaceFocus == .writing ? "pencil" : "arrow.left.arrow.right")
                        .font(.system(size: 10, weight: .semibold)).foregroundStyle(Color.accentColor)
                        .padding(.horizontal, 8).padding(.vertical, 4)
                        .background(Color.accentColor.opacity(0.10), in: Capsule())
                        .opacity(selected ? 1 : 0)
                        .accessibilityHidden(!selected).accessibilityIdentifier("active-pane-mode")
                    if model.isTiled {
                        Button { model.removePane(tab.id) } label: { Image(systemName: "xmark").font(.system(size: 10)) }
                            .buttonStyle(.plain).help("Remove from layout · keep agent in sidebar").accessibilityIdentifier("remove-pane-" + tab.id)
                    }
                    Menu {
                        Button("Rename…") { model.renameTab(tab) }
                        Button("Move left") { model.select(tab.id); model.movePane(-1) }
                        Button("Move right") { model.select(tab.id); model.movePane(1) }
                        Button("Focus this agent") { model.focusOnly(tab.id) }
                        if model.isTiled { Button("Remove from layout") { model.removePane(tab.id) } }
                        Divider()
                        Button("Close pane") { model.closeTab(tab.id) }
                    } label: { Image(systemName: "ellipsis") }.menuStyle(.borderlessButton).fixedSize().help("Pane actions")
                }
                HStack(alignment: .top, spacing: 12) {
                    Text(model.title(tab)).font(.system(size: model.isTiled ? 15 : 22, weight: .semibold)).lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                        .help("Drag left or right to switch agents · click to navigate with the keyboard")
                    Button { model.review(tab.id, seen: false) } label: { Image(systemName: "clock") }
                        .help("Later · revisit after a new update").accessibilityIdentifier("pane-later-\(tab.id)")
                    Button { model.review(tab.id, seen: true) } label: { Image(systemName: "checkmark") }
                        .help("Mark update seen (⌘D)").accessibilityIdentifier("pane-seen-\(tab.id)")
                }.buttonStyle(.plain)
            }.padding(model.isTiled ? 16 : 24)
                .background(selected ? Color.accentColor.opacity(0.045) : .clear)
                .contentShape(Rectangle())
                .onTapGesture { model.select(tab.id); model.enterNavigation() }
                .gesture(headerSwipe)
            Divider().opacity(0.45)
            ChatView()
        }
        .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 16))
        .clipShape(RoundedRectangle(cornerRadius: 16))
        .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(selected ? Color.accentColor.opacity(0.85) : Color.primary.opacity(0.09), lineWidth: selected ? 2 : 1))
        .offset(x: reduceMotion ? 0 : dragOffset)
        .background(PaneActivation { model.select(tab.id) })
        .accessibilityElement(children: .contain).accessibilityIdentifier("workspace-pane-\(tab.id)")
    }
    private var headerSwipe: some Gesture {
        DragGesture(minimumDistance: 12)
            .onChanged { value in
                if horizontalDrag == nil {
                    horizontalDrag = abs(value.translation.width) > abs(value.translation.height) * 1.2
                    if horizontalDrag == true { model.select(tab.id); model.enterNavigation() }
                }
                guard horizontalDrag == true else { return }
                dragOffset = max(-48, min(48, value.translation.width * 0.2))
            }
            .onEnded { value in
                let horizontal = horizontalDrag == true
                horizontalDrag = nil
                withAnimation(reduceMotion ? nil : .spring(response: 0.22, dampingFraction: 0.9)) { dragOffset = 0 }
                guard horizontal, abs(value.translation.width) > 60 || (abs(value.translation.width) > 20 && abs(value.predictedEndTranslation.width) > 150) else { return }
                model.cyclePane(value.translation.width < 0 ? 1 : -1, focusEditor: false)
            }
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
            HStack(spacing: 6) {
                ForEach(WorkspaceFilter.allCases, id: \.self) { filter in
                    Button { model.setFilter(filter) } label: {
                        HStack(spacing: 6) {
                            Text(filter.rawValue)
                            if filter != .all {
                                Text("\(filter == .inbox ? model.attentionCount : model.runningCount)")
                                    .monospacedDigit().foregroundStyle(.secondary)
                            }
                        }.font(.system(size: 12, weight: .medium)).padding(.horizontal, 12).padding(.vertical, 7)
                            .background(model.workspaceFilter == filter ? Color.primary.opacity(0.07) : .clear, in: Capsule())
                    }.buttonStyle(.plain).accessibilityIdentifier("workspace-filter-\(filter.rawValue)")
                }
                Spacer(minLength: 8)
                if !model.isTiled, model.tiledTabIDs.count > 1 {
                    Button { model.toggleFocusMode() } label: { Label("Resume layout", systemImage: "rectangle.split.2x1") }
                        .buttonStyle(.plain).font(.system(size: 12)).foregroundStyle(.secondary)
                }
                if model.isTiled {
                    Menu {
                        Button("Fit two panes") { model.paneWidth = 0; model.persistLayout() }
                        Button("Compact · 420 pt") { model.resizePanes(420) }
                        Button("Wide · 720 pt") { model.resizePanes(720) }
                        Divider()
                        Button("Focus this agent") { model.focusOnly(model.activeTabID) }
                    } label: { Label("Layout", systemImage: "rectangle.split.2x1") }
                        .menuStyle(.borderlessButton).fixedSize().help("Arrange panes")
                }
                Button { model.showingPanePicker = true } label: {
                    Label(model.isTiled ? "Add pane" : "Open beside", systemImage: "rectangle.badge.plus")
                        .font(.system(size: 12, weight: .medium)).padding(.horizontal, 11).padding(.vertical, 7)
                        .background(Color.primary.opacity(0.04), in: Capsule())
                }.buttonStyle(.plain).help(Text(verbatim: "Open an existing agent to the right (⌘⇧\\) · New agent to the right (⌘\\)")).accessibilityIdentifier("open-beside")
                    .popover(isPresented: $model.showingPanePicker) { PanePickerView() }
            }.padding(.horizontal, 18).padding(.vertical, 12)
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
                GeometryReader { geometry in
                    let width = model.paneWidth == 0 ? max(420, (geometry.size.width - 36) / 2) : min(model.paneWidth, max(360, geometry.size.width - 24))
                    TiledCanvas(model: model, paneWidth: width, reduceMotion: reduceMotion)
                }.accessibilityIdentifier("tiled-workspace")
            } else {
                InboxPager(model: model, reduceMotion: reduceMotion)
                    .padding(.horizontal, 14).padding(.bottom, 2)
                    .accessibilityIdentifier("inbox-pager")
            }
            HStack(spacing: 12) {
                Button { model.cyclePane(-1) } label: { Image(systemName: "chevron.left").frame(width: 24, height: 28) }
                    .disabled(position == 0).help("Previous agent (⌘⌥←)").accessibilityIdentifier("previous-agent")
                VStack(spacing: 2) {
                    Text("\(navigationTabs.isEmpty ? 0 : position + 1) of \(navigationTabs.count)").font(.system(size: 11, weight: .medium)).monospacedDigit()
                    Text(model.isTiled ? "Scroll or drag a header to switch" : "Swipe or drag the header to switch")
                        .font(.system(size: 10)).foregroundStyle(.tertiary)
                }
                Button { model.cyclePane(1) } label: { Image(systemName: "chevron.right").frame(width: 24, height: 28) }
                    .disabled(position >= navigationTabs.count - 1).help("Next agent (⌘⌥→)").accessibilityIdentifier("next-agent")
                Divider().frame(height: 22).padding(.horizontal, 4)
                Button {
                    if model.workspaceFocus == .writing { model.enterNavigation() } else { model.focusComposer() }
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Label(model.workspaceFocus == .writing ? "Writing" : "Navigate", systemImage: model.workspaceFocus == .writing ? "pencil" : "arrow.left.arrow.right")
                            .font(.system(size: 11, weight: .semibold)).foregroundStyle(Color.accentColor)
                        Text(model.workspaceFocus == .writing ? "Esc to navigate" : "Tab / arrows to move · Enter to write")
                            .font(.system(size: 10)).foregroundStyle(.secondary)
                    }
                }.accessibilityIdentifier("workspace-keyboard-mode")
                Button { model.newTab(beside: true) } label: { Text("⌘\\ New split").font(.system(size: 11)) }
                    .help(Text(verbatim: "New agent to the right (⌘\\)")).accessibilityIdentifier("new-split")
            }.buttonStyle(.plain).foregroundStyle(.secondary).padding(.vertical, 11)
        }.background(Color(nsColor: .windowBackgroundColor).opacity(0.45))
            .background(WorkspaceKeyboard(model: model))
            .onExitCommand { model.enterNavigation() }
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
    private var typingTabID: String?
    private var pendingTyping: [NSEvent] = []
    override var acceptsFirstResponder: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
    override func viewDidMoveToWindow() { super.viewDidMoveToWindow(); requestFocus() }
    static func find(in view: NSView?) -> WorkspaceKeyboardView? {
        guard let view else { return nil }
        return (view as? WorkspaceKeyboardView) ?? view.subviews.lazy.compactMap { find(in: $0) }.first
    }
    private func editor(in view: NSView?, tabID: String) -> ComposerTextView? {
        guard let view else { return nil }
        if let editor = view as? ComposerTextView, editor.workspaceTabID == tabID { return editor }
        return view.subviews.lazy.compactMap { self.editor(in: $0, tabID: tabID) }.first
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
    override func keyDown(with event: NSEvent) {
        guard let model, event.modifierFlags.intersection([.command, .control, .option]).isEmpty else { super.keyDown(with: event); return }
        // A page may still be mounting when Enter is followed immediately by
        // typing. Keep those events until that exact pane's editor is ready.
        if model.workspaceFocus == .writing, event.keyCode != 53, typingTabID == model.activeTabID {
            pendingTyping.append(event); return
        }
        switch event.keyCode {
        case 53: pendingTyping = []; typingTabID = nil; model.enterNavigation()
        case 48: model.cyclePane(event.modifierFlags.contains(.shift) ? -1 : 1, focusEditor: false)
        case 123, 126: model.cyclePane(-1, focusEditor: false)
        case 124, 125: model.cyclePane(1, focusEditor: false)
        case 36, 76:
            typingTabID = model.activeTabID
            model.focusComposer()
            if let editor = editor(in: window?.contentView, tabID: model.activeTabID), window?.makeFirstResponder(editor) == true { flushTyping(to: editor) }
        default: super.keyDown(with: event)
        }
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

/// A native horizontal scroll view lets nested transcripts forward horizontal
/// gestures through the responder chain; vertical reading stays in the transcript.
struct TiledCanvas: NSViewRepresentable {
    @ObservedObject var model: AppModel
    var paneWidth: CGFloat
    var reduceMotion: Bool
    func makeNSView(context: Context) -> WorkspaceRail {
        let scroll = WorkspaceRail()
        scroll.drawsBackground = false; scroll.hasHorizontalScroller = true; scroll.hasVerticalScroller = false
        scroll.autohidesScrollers = true; scroll.horizontalScrollElasticity = .allowed; scroll.verticalScrollElasticity = .none
        scroll.documentView = NSHostingView(rootView: AnyView(Color.clear))
        scroll.wheelRouter = WorkspaceWheelRouter(view: scroll) { [weak scroll] event in scroll?.scrollWheel(with: event) }
        return scroll
    }
    func updateNSView(_ scroll: WorkspaceRail, context: Context) {
        guard let host = scroll.documentView as? NSHostingView<AnyView> else { return }
        let ids = model.canvasTabs.map(\.id)
        if scroll.orderedIDs != ids || scroll.lastWidth != paneWidth {
            host.rootView = AnyView(HStack(spacing: 12) {
                ForEach(ids, id: \.self) { id in AgentPage(model: model, id: id).frame(width: paneWidth) }
            }.padding(.horizontal, 12).padding(.vertical, 2))
            host.sizingOptions = []
        }
        let size = NSSize(width: CGFloat(ids.count) * (paneWidth + 12) + 12, height: max(0, scroll.contentSize.height))
        if host.frame.size != size { host.setFrameSize(size) }
        scroll.paneStride = paneWidth + 12
        scroll.reduceMotion = reduceMotion
        if scroll.selectedID != model.activeTabID || scroll.orderedIDs != ids || scroll.lastWidth != paneWidth {
            scroll.selectedID = model.activeTabID; scroll.orderedIDs = ids; scroll.lastWidth = paneWidth
            if let index = ids.firstIndex(of: model.activeTabID) { scroll.reveal(index: index) }
        }
    }
}

final class WorkspaceRail: NSScrollView {
    var wheelRouter: WorkspaceWheelRouter?
    var selectedID = ""
    var orderedIDs: [String] = []
    var lastWidth: CGFloat = 0
    var paneStride: CGFloat = 0
    var reduceMotion = false
    private var lastViewportWidth: CGFloat = 0
    override func wantsForwardedScrollEvents(for axis: NSEvent.GestureAxis) -> Bool { axis == .horizontal }
    override func layout() {
        super.layout()
        if let documentView, abs(documentView.frame.height - contentSize.height) > 1 { documentView.setFrameSize(NSSize(width: documentView.frame.width, height: contentSize.height)) }
        if abs(lastViewportWidth - contentSize.width) > 1 {
            lastViewportWidth = contentSize.width
            if let index = orderedIDs.firstIndex(of: selectedID) { reveal(index: index) }
        }
    }
    func reveal(index: Int) {
        guard let documentView else { return }
        let left = CGFloat(index) * paneStride
        let right = left + paneStride
        let visible = contentView.bounds
        guard visible.width > 0 else { return }
        let target = left < visible.minX ? left : right > visible.maxX ? right - visible.width : visible.minX
        let x = min(max(0, documentView.frame.width - visible.width), max(0, target))
        guard abs(x - visible.minX) > 1 else { return }
        contentView.scroll(to: NSPoint(x: x, y: 0)); reflectScrolledClipView(contentView)
    }
}

/// Route only horizontal gestures inside this workspace. This also covers native
/// text editors, whose scroll views otherwise consume horizontal wheel events.
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
