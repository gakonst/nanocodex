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
            HStack(spacing: 9) {
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
                .background(selected ? Color.primary.opacity(0.035) : .clear)
                .contentShape(Rectangle())
                .onTapGesture { model.select(tab.id); model.enterNavigation() }
                .gesture(headerSwipe)
            Divider().opacity(0.45)
            ChatView()
        }
        .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 11))
        .clipShape(RoundedRectangle(cornerRadius: 11))
        .overlay(RoundedRectangle(cornerRadius: 11).strokeBorder(selected ? Color.accentColor.opacity(0.45) : Color.primary.opacity(0.09), lineWidth: 1))
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
                Menu {
                    Button("Split Right") { model.splitAgent(axis: "horizontal") }
                    Button("Split Below") { model.splitAgent(axis: "vertical") }
                    if model.isTiled { Button("Focus this agent") { model.focusOnly(model.activeTabID) } }
                } label: { Image(systemName: "rectangle.split.2x2") }
                    .menuStyle(.borderlessButton).fixedSize().help("Split layout")
                Button { model.showingPanePicker = true } label: {
                    Label(model.isTiled ? "Add pane" : "Open beside", systemImage: "rectangle.badge.plus")
                        .font(.system(size: 12, weight: .medium)).padding(.horizontal, 11).padding(.vertical, 7)
                        .background(Color.primary.opacity(0.04), in: Capsule())
                }.buttonStyle(.plain).help(Text(verbatim: "Open an existing agent to the right (⌘⇧\\) · New agent to the right (⌘\\)")).accessibilityIdentifier("open-beside")
                    .popover(isPresented: $model.showingPanePicker) { PanePickerView() }
            }.padding(.horizontal, 14).padding(.vertical, 6)
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
                    VStack(alignment: .leading, spacing: 2) {
                        Label(model.workspaceFocus == .writing ? "Writing" : "Navigate", systemImage: model.workspaceFocus == .writing ? "pencil" : "arrow.left.arrow.right")
                            .font(.system(size: 11, weight: .semibold)).foregroundStyle(Color.accentColor)
                        Text(model.workspaceFocus == .writing ? "Esc to navigate" : "Tab / arrows to move · Enter to write")
                            .font(.system(size: 10)).foregroundStyle(.secondary)
                    }
                }.accessibilityIdentifier("workspace-keyboard-mode")
                Button { model.newTab(beside: true) } label: { Text("⌘\\ New split").font(.system(size: 11)) }
                    .help(Text(verbatim: "New agent to the right (⌘\\)")).accessibilityIdentifier("new-split")
            }.buttonStyle(.plain).foregroundStyle(.secondary).padding(.vertical, 5)
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
        surface.setFrameSize(NSSize(width: max(scroll.contentSize.width, minimum.width), height: max(scroll.contentSize.height, minimum.height)))
        surface.needsLayout = true
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
    override var isFlipped: Bool { true }
    init(model: AppModel) { self.model = model; super.init(frame: .zero) }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
    func synchronize(_ next: PaneNode) {
        guard tree != next else { return }; tree = next
        let leaves = Set(next.leaves)
        for id in Array(hosts.keys) where !leaves.contains(id) { hosts.removeValue(forKey: id)?.removeFromSuperview() }
        for id in next.leaves where hosts[id] == nil {
            let host = NSHostingView(rootView: AnyView(AgentPage(model: model, id: id)))
            host.sizingOptions = []; hosts[id] = host; addSubview(host)
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
        super.layout(); if let tree { place(tree, in: bounds) }
    }
    private func place(_ node: PaneNode, in rect: NSRect) {
        guard node.children.count == 2 else { hosts[node.id]?.frame = rect; return }
        let horizontal = node.axis == "horizontal"
        let available = (horizontal ? rect.width : rect.height) - 8
        let firstMin = minimumSize(node.children[0]), secondMin = minimumSize(node.children[1])
        let length = max(horizontal ? firstMin.width : firstMin.height, min(available - (horizontal ? secondMin.width : secondMin.height), available * node.fraction))
        let first = NSRect(x: rect.minX, y: rect.minY, width: horizontal ? length : rect.width, height: horizontal ? rect.height : length)
        let gap = NSRect(x: horizontal ? first.maxX : rect.minX, y: horizontal ? rect.minY : first.maxY, width: horizontal ? 8 : rect.width, height: horizontal ? rect.height : 8)
        let second = NSRect(x: horizontal ? gap.maxX : rect.minX, y: horizontal ? rect.minY : gap.maxY, width: horizontal ? available - length : rect.width, height: horizontal ? rect.height : available - length)
        if let divider = dividers[node.id] { divider.frame = gap; divider.horizontal = horizontal; divider.splitRect = rect }
        place(node.children[0], in: first); place(node.children[1], in: second)
    }
    func dragSplit(_ id: String, fraction: Double, finished: Bool) {
        if let tree { self.tree = tree.resizing(id, to: fraction); needsLayout = true; layoutSubtreeIfNeeded() }
        if finished { model.resizeSplit(id, fraction: fraction) }
    }
}

final class AgentSplitDivider: NSView {
    weak var surface: AgentSplitSurface?
    var splitID = ""
    var horizontal = true
    var splitRect = NSRect.zero
    override func resetCursorRects() { addCursorRect(bounds, cursor: horizontal ? .resizeLeftRight : .resizeUpDown) }
    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 2 { surface?.dragSplit(splitID, fraction: 0.5, finished: true); return }
        track(event, finished: false)
    }
    override func mouseDragged(with event: NSEvent) { track(event, finished: false) }
    override func mouseUp(with event: NSEvent) { track(event, finished: true) }
    private func track(_ event: NSEvent, finished: Bool) {
        guard let surface else { return }
        let point = surface.convert(event.locationInWindow, from: nil)
        let value = horizontal ? (point.x - splitRect.minX) / max(1, splitRect.width - 8) : (point.y - splitRect.minY) / max(1, splitRect.height - 8)
        surface.dragSplit(splitID, fraction: value, finished: finished)
    }
}
