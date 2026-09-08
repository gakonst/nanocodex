import AppKit
import SwiftUI
import NanocodexVoice
import NanocodexUI

private struct WorkspaceTabKey: EnvironmentKey { static let defaultValue: String? = nil }
extension EnvironmentValues {
    var workspaceTabID: String? {
        get { self[WorkspaceTabKey.self] }
        set { self[WorkspaceTabKey.self] = newValue }
    }
}

struct ChatView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.workspaceTabID) private var paneID
    private var showingWelcome: Bool {
        model.tab(paneID)?.threadId == nil && model.transcript(paneID).isEmpty && model.pendingMessages(paneID).isEmpty
    }
    var body: some View {
        VStack(spacing: 0) {
            if paneID == nil { HStack {
                Text(model.tab(paneID).map(model.title) ?? "New thread").font(.system(size: 13, weight: .medium)).lineLimit(1)
                Spacer()
                if let snapshot = model.snapshot(paneID), !snapshot.connected {
                    Label("Reconnecting…", systemImage: "arrow.triangle.2.circlepath").font(.caption).foregroundStyle(.secondary)
                }
                Button { model.screen = .hands } label: {
                    Label(model.connectedHands.isEmpty ? "Hands" : "\(model.connectedHands.count) Hand\(model.connectedHands.count == 1 ? "" : "s")", systemImage: "hand.raised")
                }.buttonStyle(.plain).font(.system(size: 12)).foregroundStyle(.secondary).help("Manage compute")
            }.padding(.horizontal, 26).padding(.vertical, 20) }
            if showingWelcome { Spacer(minLength: 24) }
            if showingWelcome {
                ViewThatFits(in: .vertical) {
                    WelcomeView()
                    Text("What should we work on?").font(.system(size: 20, weight: .medium)).padding(.horizontal, 24)
                }
            } else { TranscriptView(initiallyFollowing: model.readingPositions[paneID ?? model.activeTabID]?.followsOutput ?? true) }
            ComposerView().frame(maxWidth: 780).padding(.horizontal, 26).padding(.top, showingWelcome ? 24 : 14).padding(.bottom, 18)
            if showingWelcome { Spacer(minLength: 24) }
        }.frame(maxWidth: .infinity, maxHeight: .infinity).background(ChatPalette.background)
    }
}

struct WelcomeView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.workspaceTabID) private var paneID
    var body: some View {
        VStack(alignment: .center, spacing: 13) {
            Text("What should we work on?").font(.system(size: 28, weight: .medium)).tracking(-0.7)
            Text("Ask a question, explore your code, or start building.").font(.system(size: 15, weight: .regular)).foregroundStyle(.secondary)
            if !model.state.connected && !model.isStarting {
                Button { model.showingSettings = true } label: { Label("Connect your account", systemImage: "person.crop.circle.badge.checkmark") }
                    .buttonStyle(.borderedProminent).tint(.primary).padding(.top, 12)
            } else {
                Menu {
                    Button("Choose Working Folder…") { model.chooseFolder(tabID: paneID) }
                    Divider()
                    Button("Let the agent choose compute") { model.updateTarget("", tabID: paneID) }
                    ForEach(model.handsForTab(paneID)) { hand in Button(hand.name) { model.updateTarget(hand.id, tabID: paneID) } }
                    ForEach(model.otherAccountHands.filter(\.isConnected)) { hand in Button(hand.name) { model.updateTarget(hand.id, tabID: paneID) } }
                    Divider()
                    Button("Add a Hand…") { model.screen = .hands }
                } label: {
                    Label(model.tab(paneID)?.folder.isEmpty == false ? URL(fileURLWithPath: model.tab(paneID)?.folder ?? "").lastPathComponent : "Choose a folder", systemImage: "folder")
                        .font(.system(size: 13)).foregroundStyle(.secondary)
                }.menuStyle(.borderlessButton).fixedSize().padding(.top, 9)
            }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) { suggestionButtons }
                VStack(spacing: 8) { suggestionButtons }
            }.padding(.top, 12)
        }.frame(maxWidth: 728, alignment: .center).padding(.horizontal, 26).multilineTextAlignment(.center)
    }
    private var suggestionButtons: some View {
        Group {
                suggestion("Build something", icon: "hammer") { model.updateDraft("Help me build ", tabID: paneID) }
                suggestion("Explore my code", icon: "chevron.left.forwardslash.chevron.right") { model.updateDraft("Explore my codebase and explain how it works.", tabID: paneID) }
                suggestion("Plan a task", icon: "list.bullet") { model.updateDraft("Help me plan ", tabID: paneID) }
        }
    }
    private func suggestion(_ title: String, icon: String, action: @escaping () -> Void) -> some View {
        Button(action: action) { Label(title, systemImage: icon).font(.system(size: 12)).lineLimit(1).foregroundStyle(.secondary).padding(.horizontal, 13).padding(.vertical, 10).background(Color.primary.opacity(0.025), in: Capsule()).overlay(Capsule().stroke(Color.primary.opacity(0.07))) }.buttonStyle(.plain)
    }
}

struct TranscriptView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.workspaceTabID) private var paneID
    @State private var nearBottom = true
    @State private var loadingHistory = false
    init(initiallyFollowing: Bool = true) { _nearBottom = State(initialValue: initiallyFollowing) }
    private struct Turn: Identifiable { var id: String; var messages: [MessageEntry] }
    private var turns: [Turn] {
        var result: [Turn] = []
        for message in model.displayedTranscript(paneID) {
            if result.last?.id == message.turnId { result[result.count - 1].messages.append(message) }
            else { result.append(Turn(id: message.turnId, messages: [message])) }
        }
        return result
    }
    var body: some View {
        let groups = turns
        GeometryReader { geometry in
            ScrollViewReader { proxy in
                ScrollView {
                    // Real heights keep follow-up insertion from jumping through
                    // a lazy stack's estimated bottom before finding the prompt.
                    VStack(alignment: .leading, spacing: 24) {
                        if groups.isEmpty, let threadID = model.tab(paneID)?.threadId, model.snapshot(paneID) == nil {
                            if let failure = model.threadError(paneID) {
                                ThreadUnavailableView(missing: failure.contains("404") || failure.contains("not_found")) {
                                    Task { await model.observe(threadID) }
                                } newThread: { model.newTab() }
                            } else { ThreadLoadingView() }
                        }
                        ForEach(groups) { turn in
                            VStack(alignment: .leading, spacing: 24) {
                                ForEach(NativeConversationItem.group(turn.messages, working: model.snapshot(paneID)?.activeTurns.contains(turn.id) == true || (turn.id == groups.last?.id && model.working(paneID)))) { item in
                                    Group {
                                        if let entry = item.message { message(entry).id(entry.id) }
                                        else if !item.generatedOutputs.isEmpty { ChatGeneratedOutputs(outputs: item.generatedOutputs).id(item.id) }
                                        else { activity(item).id(item.id) }
                                    }.background(TranscriptItemAnchor(id: item.id))
                                }
                            }
                            // Reserve the space below the newest prompt. Replies
                            // grow downward without pushing that prompt upward.
                            .frame(maxWidth: .infinity, minHeight: turn.id == groups.last?.id ? max(0, geometry.size.height - 32) : 0, alignment: .topLeading)
                            .id(turn.id)
                        }
                        if let conversationID = model.tab(paneID)?.threadId {
                            NanocodexVoiceTranscript(session: model.voice, conversationID: conversationID,
                                                     durableRows: model.voiceTranscriptRows(paneID)) {
                                if nearBottom { proxy.scrollTo("bottom", anchor: .bottom) }
                            }
                        }
                        Color.clear.frame(height: 1).id("bottom")
                    }
                    .frame(maxWidth: 728, alignment: .leading).padding(.horizontal, 26).padding(.top, 20).padding(.bottom, 12).frame(maxWidth: .infinity)
                    .background(TranscriptScrollObserver(restoreOffset: model.readingPositions[paneID ?? model.activeTabID]?.offset,
                                                         threadID: model.tab(paneID)?.threadId,
                                                         historyStart: model.snapshot(paneID)?.events.first?.cursor,
                                                         hasMore: model.snapshot(paneID)?.hasMore == true,
                                                         loadOlder: { await model.loadOlder(tabID: paneID) },
                                                         loadingChanged: { loadingHistory = $0 }) { atBottom, offset in
                        guard !groups.isEmpty else { return }
                        if nearBottom != atBottom { nearBottom = atBottom }
                        model.readingPositions[paneID ?? model.activeTabID] = .init(anchor: nil, followsOutput: atBottom, offset: offset)
                    })
                }
                .defaultScrollAnchor(.top)
                .onAppear {
                    if model.readingPositions[paneID ?? model.activeTabID]?.offset == nil, let id = groups.last?.id { proxy.scrollTo(id, anchor: .top) }
                }
                .onChange(of: groups.last?.id) { previous, id in
                    guard let id else { return }
                    // Loading a retained conversation honors its reading position;
                    // sending the next message starts that turn at the top.
                    if previous != nil || model.readingPositions[paneID ?? model.activeTabID]?.offset == nil { proxy.scrollTo(id, anchor: .top) }
                }
                .overlay(alignment: .top) {
                    if loadingHistory {
                        ProgressView("Loading earlier messages…").controlSize(.small).font(.caption)
                            .padding(.horizontal, 12).padding(.vertical, 8)
                            .background(.regularMaterial, in: Capsule()).padding(8)
                            .allowsHitTesting(false).accessibilityIdentifier("history-loading")
                    }
                }
                .overlay(alignment: .bottomTrailing) {
                    if !nearBottom {
                        Button { proxy.scrollTo("bottom", anchor: .bottom) } label: {
                            Label("Latest", systemImage: "arrow.down").font(.system(size: 12, weight: .medium)).padding(.horizontal, 12).padding(.vertical, 8)
                        }.buttonStyle(.plain).background(.regularMaterial, in: Capsule()).overlay(Capsule().strokeBorder(Color.primary.opacity(0.08))).padding(15).help("Jump to the latest response")
                    }
                }
            }
        }.accessibilityIdentifier("transcript")
    }
    private func activity(_ item: NativeConversationItem) -> some View {
        let id = paneID ?? model.activeTabID
        return NativeActivityView(item: item, expandedIDs: model.expandedMessages[id] ?? []) { key in
            if model.expandedMessages[id]?.contains(key) == true { model.expandedMessages[id]?.remove(key) }
            else { model.expandedMessages[id, default: []].insert(key) }
        }
    }
    private func message(_ entry: MessageEntry) -> some View {
        let id = paneID ?? model.activeTabID
        return MessageView(entry: entry, isExpanded: model.expandedMessages[id]?.contains(entry.id) == true) { expanded in
            if expanded { model.expandedMessages[id, default: []].insert(entry.id) }
            else { model.expandedMessages[id]?.remove(entry.id) }
        }.equatable()
    }
}

struct ThreadUnavailableView: View {
    var missing: Bool
    var retry: () -> Void
    var newThread: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Couldn’t open this conversation", systemImage: "bubble.left.and.exclamationmark.bubble.right")
                .font(.system(size: 17, weight: .semibold))
            Text(missing ? "This conversation is no longer available in this account. Your draft is still here." : "The conversation couldn’t load. Try again to reconnect. Your draft is still here.")
                .font(.system(size: 13)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 12) {
                Button("Try again", action: retry).accessibilityIdentifier("retry-thread")
                Button("New thread", action: newThread).accessibilityIdentifier("new-thread-from-error")
            }.buttonStyle(.bordered).controlSize(.small)
        }.padding(.vertical, 16).frame(maxWidth: .infinity, alignment: .leading).accessibilityIdentifier("thread-unavailable")
    }
}

/// Native row geometry keeps prepends independent of streamed growth below it.
struct TranscriptItemAnchor: NSViewRepresentable {
    var id: String
    func makeNSView(context: Context) -> MarkerView { let view = MarkerView(); view.itemID = id; return view }
    func updateNSView(_ view: MarkerView, context: Context) { view.itemID = id }
    final class MarkerView: NSView {
        var itemID = ""
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
    }
}

/// Retains the viewport and pages only on user scrolling, without publishing
/// per-pixel SwiftUI state. Streaming alone never scrolls or fetches history.
struct TranscriptScrollObserver: NSViewRepresentable {
    var restoreOffset: CGFloat?
    var threadID: String?
    var historyStart: String?
    var hasMore: Bool
    var loadOlder: () async -> Void
    var loadingChanged: (Bool) -> Void
    var changed: (Bool, CGFloat) -> Void
    func makeNSView(context: Context) -> ObserverView { let view = ObserverView(); view.restoreOffset = restoreOffset; updateNSView(view, context: context); return view }
    func updateNSView(_ view: ObserverView, context: Context) {
        view.changed = changed; view.loadOlder = loadOlder; view.loadingChanged = loadingChanged
        view.updateHistory(threadID: threadID, start: historyStart, hasMore: hasMore)
    }
    final class ObserverView: NSView {
        var changed: ((Bool, CGFloat) -> Void)?
        var restoreOffset: CGFloat?
        var loadOlder: (() async -> Void)?
        var loadingChanged: ((Bool) -> Void)?
        private weak var observed: NSScrollView?
        private var tokens: [NSObjectProtocol] = []
        private var reportScheduled = false
        private var userScrolled = false
        private var threadID: String?
        private var historyStart: String?
        private var hasMore = false
        private var loading = false
        private var boundaryArmed = true
        private var requestID = UUID()
        private struct Anchor { var id: String; var viewportY: CGFloat }
        private var anchor: Anchor?
        private weak var anchorView: TranscriptItemAnchor.MarkerView?
        private var restoringPrepend = false

        func updateHistory(threadID: String?, start: String?, hasMore: Bool) {
            if self.threadID != threadID {
                self.threadID = threadID; requestID = UUID()
                let wasLoading = loading
                loading = false; boundaryArmed = true; anchor = nil; anchorView = nil; restoringPrepend = false
                if wasLoading { DispatchQueue.main.async { [weak self] in self?.loadingChanged?(false) } }
            } else if loading, historyStart != start, anchor != nil {
                restoringPrepend = true
                // SwiftUI finishes its new row geometry on the following pass.
                DispatchQueue.main.async { [weak self] in self?.restoreAnchor() }
            }
            historyStart = start; self.hasMore = hasMore
        }
        override func layout() {
            super.layout()
            guard let scroll = enclosingScrollView, observed !== scroll else { return }
            tokens.forEach(NotificationCenter.default.removeObserver); tokens = []; observed = scroll
            if let offset = restoreOffset {
                DispatchQueue.main.async { [weak scroll] in
                    guard let scroll, let document = scroll.documentView else { return }
                    let y = min(max(0, document.bounds.height - scroll.contentSize.height), max(0, offset))
                    scroll.contentView.scroll(to: NSPoint(x: 0, y: y)); scroll.reflectScrolledClipView(scroll.contentView)
                }
            }
            scroll.documentView?.postsFrameChangedNotifications = true
            scroll.contentView.postsBoundsChangedNotifications = true
            let observed: [(Notification.Name, AnyObject?)] = [
                (NSView.frameDidChangeNotification, scroll.documentView),
                (NSView.boundsDidChangeNotification, scroll.contentView),
                (NSScrollView.didLiveScrollNotification, scroll)
            ]
            for (name, object) in observed {
                tokens.append(NotificationCenter.default.addObserver(forName: name, object: object, queue: .main) { [weak self] _ in
                    self?.reportPosition(userScrolled: name == NSScrollView.didLiveScrollNotification)
                })
            }
        }
        private func reportPosition(userScrolled: Bool = false) {
            self.userScrolled = self.userScrolled || userScrolled
            guard !reportScheduled else { return }; reportScheduled = true
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }; self.reportScheduled = false
                guard let scroll = self.observed, let document = scroll.documentView else { return }
                self.restoreAnchor()
                let visible = scroll.documentVisibleRect
                let remaining = document.isFlipped ? document.bounds.maxY - visible.maxY : visible.minY - document.bounds.minY
                self.changed?(remaining < 32, scroll.contentView.bounds.minY)
                let top = document.isFlipped ? visible.minY - document.bounds.minY : document.bounds.maxY - visible.maxY
                if top > 400 { self.boundaryArmed = true }
                if self.userScrolled {
                    self.userScrolled = false
                    if self.loading, !self.restoringPrepend, let marker = self.anchorView {
                        self.anchor?.viewportY = marker.convert(marker.bounds, to: document).minY - visible.minY
                    }
                    if top < 240 { self.loadHistory(in: scroll) }
                }
            }
        }
        private func markers(in view: NSView) -> [TranscriptItemAnchor.MarkerView] {
            if let marker = view as? TranscriptItemAnchor.MarkerView { return [marker] }
            return view.subviews.flatMap { markers(in: $0) }
        }
        private func visibleAnchor(in scroll: NSScrollView) -> Anchor? {
            guard let document = scroll.documentView else { return nil }
            let visible = scroll.documentVisibleRect
            let candidates = markers(in: document).map { ($0, $0.convert($0.bounds, to: document)) }
            guard let first = candidates.filter({ $0.1.maxY > visible.minY && $0.1.minY < visible.maxY }).min(by: { $0.1.minY < $1.1.minY }) else { return nil }
            anchorView = first.0
            return Anchor(id: first.0.itemID, viewportY: first.1.minY - visible.minY)
        }
        private func restoreAnchor() {
            guard restoringPrepend, let anchor, let scroll = observed, let document = scroll.documentView else { return }
            document.layoutSubtreeIfNeeded()
            guard let marker = markers(in: document).first(where: { $0.itemID == anchor.id }) else { return }
            let y = marker.convert(marker.bounds, to: document).minY - anchor.viewportY
            let clamped = min(max(0, document.bounds.height - scroll.contentSize.height), max(0, y))
            if abs(scroll.contentView.bounds.minY - clamped) > 0.5 {
                scroll.contentView.scroll(to: NSPoint(x: 0, y: clamped)); scroll.reflectScrolledClipView(scroll.contentView)
            }
        }
        private func loadHistory(in scroll: NSScrollView) {
            guard hasMore, boundaryArmed, !loading, let loadOlder else { return }
            boundaryArmed = false; loading = true; anchor = visibleAnchor(in: scroll); loadingChanged?(true)
            let request = UUID(); requestID = request
            let previousStart = historyStart
            Task { @MainActor [weak self] in
                await loadOlder()
                // Let the published snapshot finish its SwiftUI layout before
                // releasing the saved row; completion never requests another page.
                DispatchQueue.main.async { [weak self] in
                    guard let self, self.requestID == request else { return }
                    self.window?.contentView?.layoutSubtreeIfNeeded()
                    self.restoreAnchor()
                    if self.historyStart != previousStart { self.boundaryArmed = true }
                    self.loading = false; self.anchor = nil; self.anchorView = nil; self.restoringPrepend = false
                    self.loadingChanged?(false)
                    self.reportPosition()
                }
            }
        }
        deinit { tokens.forEach(NotificationCenter.default.removeObserver) }
    }
}

struct NativeActivityView: View {
    let item: NativeConversationItem
    let expandedIDs: Set<String>
    var toggle: (String) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var expanded: Bool { expandedIDs.contains(item.id) }
    private var failures: Int { item.activity.filter { $0.status == "failed" }.count }
    private var title: String {
        guard item.isRunning else { return "Activity" }
        return item.activity.last.map { ($0.kind == .tool && $0.status == "running") || $0.kind == .reasoning ? $0.activityTitle : "Working" } ?? "Working"
    }
    var body: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.16)) { toggle(item.id) }
            } label: {
                HStack(spacing: 10) {
                    Group {
                        if item.isRunning { ProgressView().controlSize(.mini) }
                        else { Image(systemName: failures > 0 ? "exclamationmark.circle" : "checkmark").font(.system(size: 11, weight: .medium)) }
                    }.frame(width: 18, height: 18)
                    Text(title).font(.system(size: 12, weight: .medium)).lineLimit(1)
                    Spacer(minLength: 4)
                    if failures > 0 { Text("\(failures) issue\(failures == 1 ? "" : "s")").foregroundStyle(.orange).font(.system(size: 11)) }
                    if !item.activity.isEmpty { Text("\(item.activity.count) step\(item.activity.count == 1 ? "" : "s")").font(.system(size: 11)).monospacedDigit() }
                    Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold)).rotationEffect(.degrees(expanded ? 90 : 0))
                }.foregroundStyle(.secondary).padding(.horizontal, 12).frame(height: 40).contentShape(Rectangle())
            }.buttonStyle(.plain).help(expanded ? "Hide activity" : "Show thinking and tool activity")
                .accessibilityIdentifier("activity-disclosure").accessibilityLabel(title)
                .accessibilityValue("\(item.activity.count) step\(item.activity.count == 1 ? "" : "s"), \(failures) issue\(failures == 1 ? "" : "s"), " + (expanded ? "Expanded" : "Collapsed"))
            if expanded {
                Divider().opacity(0.45).padding(.horizontal, 12)
                ScrollView {
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(item.activity) { entry in
                            NativeActivityStep(entry: entry, live: item.isRunning && (entry.streaming || entry.status == "running"), expanded: expandedIDs.contains(entry.id)) { toggle(entry.id) }
                        }
                        if item.activity.isEmpty { Text("Waiting for the first update…").font(.system(size: 12)).foregroundStyle(.secondary).padding(12) }
                    }.padding(6)
                }.frame(maxHeight: 300).fixedSize(horizontal: false, vertical: true).accessibilityIdentifier("activity-timeline")
            }
        }.background(Color.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color.primary.opacity(0.07), lineWidth: 0.5))
            .accessibilityElement(children: .contain).accessibilityIdentifier("activity-group")
    }
}

private struct NativeActivityStep: View {
    let entry: MessageEntry
    let live: Bool
    let expanded: Bool
    var toggle: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button(action: toggle) {
                HStack(alignment: .top, spacing: 9) {
                    Image(systemName: entry.status == "failed" ? "exclamationmark.circle" : entry.kind == .tool ? "terminal" : "text.alignleft")
                        .font(.system(size: 11)).frame(width: 18, height: 18).foregroundStyle(entry.status == "failed" ? Color.orange : .secondary)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(entry.activityTitle).font(.system(size: 12, weight: .medium)).lineLimit(1)
                        if !entry.activitySubject.isEmpty { Text(entry.activitySubject).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1) }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                    if live { ProgressView().controlSize(.mini) }
                    Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold)).foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(expanded ? 90 : 0)).padding(.top, 3)
                }.padding(9).contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("activity-step-" + entry.id).accessibilityValue(expanded ? "Expanded" : "Collapsed")
            if expanded {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        if entry.kind == .tool {
                            if !entry.text.isEmpty { field("Input", entry.text) }
                            if !entry.output.isEmpty { field("Result", entry.output) }
                            if !live, entry.output.isEmpty { Text(entry.status == "cancelled" ? "Stopped" : "Result unavailable").font(.system(size: 12)).foregroundStyle(.secondary) }
                        } else {
                            Text(entry.text).font(.system(size: 13)).lineSpacing(4).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }.padding(12)
                }.frame(maxHeight: 240).fixedSize(horizontal: false, vertical: true)
                    .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 8))
                    .padding(.leading, 27).padding(.horizontal, 6).padding(.bottom, 8)
                    .accessibilityIdentifier("activity-detail-" + entry.id)
            }
        }
    }
    private func field(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(.system(size: 10, weight: .semibold)).foregroundStyle(.secondary)
            Text(value).font(.system(size: 12, design: .monospaced)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

struct MessageView: View, Equatable {
    let entry: MessageEntry
    var isExpanded = false
    var onExpansionChanged: (Bool) -> Void = { _ in }
    static func == (lhs: MessageView, rhs: MessageView) -> Bool { lhs.entry == rhs.entry && lhs.isExpanded == rhs.isExpanded }
    private var expanded: Binding<Bool> {
        Binding(get: { isExpanded }, set: onExpansionChanged)
    }
    var body: some View {
        Group {
            switch entry.kind {
            case .user:
                HStack { Spacer(minLength: 80); Text(entry.displayText).font(.system(size: 16)).lineSpacing(5).textSelection(.enabled).padding(.horizontal, 17).padding(.vertical, 12).background(ChatPalette.userBubble, in: RoundedRectangle(cornerRadius: 24)) }.accessibilityIdentifier("user-message")
            case .assistant:
                VStack(alignment: .leading, spacing: 12) {
                    NativeMarkdown(text: entry.text)
                    if !entry.streaming {
                        ChatCopyButton(text: entry.text).padding(.leading, -8)
                    }
                }.accessibilityIdentifier("assistant-message")
            case .reasoning:
                DisclosureGroup(isExpanded: expanded) { Text(entry.text).font(.system(size: 13)).foregroundStyle(.secondary).textSelection(.enabled).padding(.top, 8) } label: { Label(entry.streaming ? "Thinking…" : "Thought process", systemImage: "sparkle").font(.system(size: 12)).foregroundStyle(.secondary) }
            case .tool:
                DisclosureGroup(isExpanded: expanded) {
                    VStack(alignment: .leading, spacing: 12) {
                        if !entry.text.isEmpty { codeBlock(entry.text) }
                        if !entry.output.isEmpty { codeBlock(entry.output) }
                    }.padding(.top, 10)
                } label: {
                    HStack(spacing: 8) {
                        if entry.status == "running" { ProgressView().controlSize(.mini) }
                        else { Image(systemName: entry.status == "completed" ? "checkmark" : "exclamationmark.circle").font(.system(size: 11)) }
                        Text(friendlyTool(entry.name)).font(.system(size: 12, weight: .medium))
                        Spacer(); Text(entry.status == "running" ? "Running" : "").font(.caption).foregroundStyle(.tertiary)
                    }.foregroundStyle(.secondary)
                }.padding(12).background(Color.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 9))
            case .notice:
                Label(entry.text, systemImage: "arrow.turn.up.right").font(.system(size: 12)).foregroundStyle(.secondary)
            case .error:
                Label(entry.text, systemImage: entry.text == "Stopped by you." ? "stop.circle" : "exclamationmark.circle").font(.system(size: 13)).foregroundStyle(entry.text == "Stopped by you." ? Color.secondary : .orange).textSelection(.enabled)
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
    private func codeBlock(_ text: String) -> some View {
        ScrollView(.horizontal) { Text(text).font(.system(size: 12, design: .monospaced)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading).padding(10) }.background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 6))
    }
    private func friendlyTool(_ name: String) -> String {
        switch name { case "exec_command": return "Running a command"; case "write_stdin": return "Reading a process"; case "accountInfo": return "Checking available Hands"; case "mount": return "Connecting a Hand"; default: return name }
    }
}

/// A first fetch uses the transcript's own surface, never the new-chat welcome.
/// Static placeholders avoid a spinner or shimmer flashing during a quick load.
private struct ThreadLoadingView: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            RoundedRectangle(cornerRadius: 12).fill(Color.primary.opacity(0.045)).frame(width: 210, height: 42).frame(maxWidth: .infinity, alignment: .trailing)
            ForEach([0.9, 0.75, 0.5], id: \.self) { width in
                GeometryReader { geometry in Capsule().fill(Color.primary.opacity(0.04)).frame(width: geometry.size.width * width) }.frame(height: 10)
            }
        }.padding(.vertical, 12).accessibilityElement(children: .ignore).accessibilityLabel("Loading conversation")
            .accessibilityIdentifier("thread-loading")
    }
}

struct NativeMarkdown: View {
    let text: String
    var body: some View { ChatMarkdown(text: text) }
}

struct ComposerView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.workspaceTabID) private var paneID
    @State private var editorHeight: CGFloat = 56
    @State private var editorFocused = false
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if model.pendingMessages(paneID).contains(where: { !$0.predecessor.isEmpty || $0.phase == .failed }) { PendingMessagesView() }
            if let folder = model.tab(paneID)?.folder, !folder.isEmpty {
                HStack(spacing: 6) { Image(systemName: "folder"); Text(URL(fileURLWithPath: folder).lastPathComponent); Button { model.updateTab(tabID: paneID) { $0.folder = "" } } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary) }.buttonStyle(.plain) }.font(.system(size: 11)).foregroundStyle(.secondary).padding(.horizontal, 16).padding(.top, 12).help("Shared with this thread when you send")
            }
            ZStack(alignment: .topLeading) {
                if model.tab(paneID)?.draft.isEmpty ?? true { Text(model.running(paneID) ? "Queue a follow-up…" : "Ask Nanocodex").font(.system(size: 16)).foregroundStyle(.tertiary).padding(.horizontal, 17).padding(.top, 18).allowsHitTesting(false) }
                NativeComposer(text: Binding(get: { model.tab(paneID)?.draft ?? "" }, set: { model.updateDraft($0, tabID: paneID) }), height: $editorHeight, tabID: paneID ?? model.activeTabID, focusRequest: (paneID ?? model.activeTabID) == model.requestedEditorTabID && (paneID ?? model.activeTabID) == model.activeTabID ? model.editorFocusRequest : 0, onSubmit: { let id = paneID ?? model.activeTabID; Task { await model.send(tabID: id) } }, onFocusChange: { editorFocused = $0; if $0 { model.composerFocused(paneID ?? model.activeTabID) } }, onEscape: { model.enterNavigation() }).frame(height: editorHeight).padding(.horizontal, 8).padding(.top, 8)
            }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) { contextControls; Spacer(minLength: 8); ModelMenu(); responseControls }
                VStack(spacing: 4) {
                    HStack(spacing: 8) { contextControls; Spacer(minLength: 8); ModelMenu() }
                    HStack { Spacer(); responseControls }
                }
            }.padding(.horizontal, 16).padding(.bottom, 12).padding(.top, 3)
        }
        .background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 28))
        .overlay(RoundedRectangle(cornerRadius: 28).strokeBorder(Color.primary.opacity(editorFocused ? 0.27 : 0.14), lineWidth: 1))
        .shadow(color: Color.black.opacity(editorFocused ? 0.045 : 0.025), radius: 8, y: 3)
    }
    private var contextControls: some View {
        HStack(spacing: 8) {
                Menu {
                    Button("Choose Working Folder…") { model.chooseFolder(tabID: paneID) }
                    Divider()
                    Button("Manage Hands…") { model.screen = .hands }
                    Button("Manage Connections…") { model.openAccount() }
                } label: { Image(systemName: "plus").font(.system(size: 15)).foregroundStyle(.secondary) }.menuStyle(.borderlessButton).fixedSize().help("Add context")
                Menu {
                    Button("Let the agent choose") { model.updateTarget("", tabID: paneID) }
                    ForEach(model.handsForTab(paneID)) { hand in Button(hand.name) { model.updateTarget(hand.id, tabID: paneID) } }
                    Divider()
                    Button("Manage Hands…") { model.screen = .hands }
                } label: {
                    HStack(spacing: 5) { Image(systemName: "hand.raised"); Text(model.state.hands.first(where: { $0.id == model.tab(paneID)?.target })?.name ?? model.otherAccountHands.first(where: { $0.id == model.tab(paneID)?.target })?.name ?? "Auto").lineLimit(1).truncationMode(.middle).frame(maxWidth: 90) }.font(.system(size: 11)).foregroundStyle(.secondary)
                }.menuStyle(.borderlessButton).fixedSize().help("Compute for this tab")
        }
    }
    private var responseControls: some View {
        let voiceTabID = paneID ?? model.activeTabID
        return HStack(spacing: 8) {
                NanocodexVoiceControl(session: model.voice) {
                    try await model.voiceConfiguration(tabID: voiceTabID)
                }
                .disabled(!model.state.connected)
                if !model.controllableTurns(paneID).isEmpty {
                    Button { let id = paneID ?? model.activeTabID; Task { await model.cancel(tabID: id) } } label: { Image(systemName: "stop.fill").font(.system(size: 11)).frame(width: 36, height: 36).background(Color.primary.opacity(0.07), in: Circle()) }.buttonStyle(.plain).help("Stop current turn (⌘.)").accessibilityIdentifier("stop-turn")
                }
                Button { let id = paneID ?? model.activeTabID; Task { await model.send(tabID: id) } } label: {
                    Image(systemName: "arrow.up").font(.system(size: 14, weight: .semibold)).foregroundStyle(Color(nsColor: .textBackgroundColor)).frame(width: 36, height: 36).background(Color.primary.opacity(model.hasDraft(paneID) ? 1 : 0.18), in: Circle())
                }.buttonStyle(.plain).disabled(!model.hasDraft(paneID) || !model.canSend(paneID)).help(model.running(paneID) ? "Queue a follow-up" : "Send message (Return)").accessibilityIdentifier("send-message")
        }
    }

}

/// A waiting follow-up stays here until its own turn starts, including across
/// navigation and relaunch. Controls capture the message, never the active pane.
struct PendingMessagesView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.workspaceTabID) private var paneID
    private var messages: [PendingMessage] { model.pendingMessages(paneID).filter { !$0.predecessor.isEmpty || $0.phase == .failed } }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(messages) { message in
                    HStack(alignment: .center, spacing: 10) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(label(message.phase)).font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
                            Text(message.text).font(.system(size: 13)).lineLimit(1)
                            if let error = message.error { Text(error).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(2) }
                        }.frame(maxWidth: .infinity, alignment: .leading)
                        if message.phase == .failed {
                            Button("Retry") { Task { await model.retryPending(message.id) } }.accessibilityIdentifier("retry-" + message.id)
                        } else if messages.first?.id == message.id, message.interruption != nil {
                            Button("Steer now") { Task { await model.steerNow(message.id) } }.help("Stop the current turn and start this queued message").accessibilityIdentifier("steer-" + message.id)
                        }
                        Button { Task { await model.cancelPending(message.id) } } label: {
                            Image(systemName: "xmark").font(.system(size: 11)).padding(4)
                        }.buttonStyle(.plain).help("Cancel queued message").accessibilityLabel("Cancel queued message").accessibilityIdentifier("cancel-queued-" + message.id)
                    }.buttonStyle(.bordered).controlSize(.small)
                        .disabled(model.isBusy(paneID) || message.phase == .starting || message.phase == .cancelling)
                        .accessibilityElement(children: .contain).accessibilityIdentifier("queued-" + message.id)
                }
            }.padding(.horizontal, 16).padding(.vertical, 10)
        }.frame(height: messages.count > 1 ? 112 : messages.first?.error == nil ? 64 : 92)
        Divider().padding(.horizontal, 16)
    }
    private func label(_ phase: PendingMessage.Phase) -> String {
        switch phase {
        case .submitting: return "Sending…"
        case .queued: return "Queued"
        case .starting: return "Stopping current turn…"
        case .cancelling: return "Cancelling…"
        case .failed: return "Needs retry"
        }
    }
}

struct ModelMenu: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.workspaceTabID) private var paneID
    var body: some View {
        Menu {
            Picker("Model", selection: Binding(get: { model.settingsForTab(paneID ?? model.activeTabID).model }, set: { let value = $0; model.changeSettings(tabID: paneID) { $0.selectModel(value) } })) {
                Text("GPT-6 Astra").tag("gpt-6-astra")
                Text("GPT-5.6 Sol").tag("gpt-5.6-sol")
                Text("GPT-5.6 Terra").tag("gpt-5.6-terra")
                Text("GPT-5.6 Luna").tag("gpt-5.6-luna")
            }.disabled(model.snapshot(paneID)?.hasAcceptedTurn == true)
            Picker("Reasoning", selection: Binding(get: { model.settingsForTab(paneID ?? model.activeTabID).thinking }, set: { let value = $0; model.changeSettings(tabID: paneID) { $0.thinking = value } })) {
                Text("None").tag("none").disabled(!model.settingsForTab(paneID ?? model.activeTabID).supportsNoReasoning)
                Text("Low").tag("low")
                Text("Medium").tag("medium")
                Text("High").tag("high")
                Text("Extra high").tag("xhigh")
                Text("Max").tag("max")
            }
            Divider()
            Toggle("Pro reasoning", isOn: Binding(get: { model.settingsForTab(paneID ?? model.activeTabID).reasoning_mode == "pro" }, set: { let value = $0; model.changeSettings(tabID: paneID) { $0.reasoning_mode = value ? "pro" : "standard" } }))
                .disabled(!model.settingsForTab(paneID ?? model.activeTabID).supportsProReasoning || model.snapshot(paneID)?.hasAcceptedTurn == true)
            Toggle("Fast mode", isOn: Binding(get: { model.settingsForTab(paneID ?? model.activeTabID).fast_mode }, set: { let value = $0; model.changeSettings(tabID: paneID) { $0.fast_mode = value } }))
            if model.snapshot(paneID)?.hasAcceptedTurn == true {
                Divider()
                Text("Start a new thread to change the model or Pro.")
            }
        } label: {
            HStack(spacing: 5) { Text("\(model.settingsForTab(paneID ?? model.activeTabID).modelName) · \(model.settingsForTab(paneID ?? model.activeTabID).thinking.capitalized)"); if model.settingsForTab(paneID ?? model.activeTabID).fast_mode { Image(systemName: "bolt.fill") } }.font(.system(size: 13)).foregroundStyle(.secondary).padding(.vertical, 8)
        }.menuStyle(.borderlessButton).fixedSize().help("Model and thinking settings")
    }
}

struct NativeComposer: NSViewRepresentable {
    @Binding var text: String
    @Binding var height: CGFloat
    var tabID: String?
    var focusRequest = 0
    var onSubmit: () -> Void
    var onFocusChange: (Bool) -> Void = { _ in }
    var onEscape: () -> Void = {}
    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView(), editor = ComposerTextView()
        scroll.drawsBackground = false; scroll.hasVerticalScroller = true; scroll.autohidesScrollers = true
        editor.isRichText = false; editor.drawsBackground = false; editor.font = .systemFont(ofSize: 16)
        editor.textColor = .labelColor; editor.insertionPointColor = .labelColor
        editor.textContainerInset = NSSize(width: 7, height: 10)
        editor.isVerticallyResizable = true; editor.isHorizontallyResizable = false
        editor.autoresizingMask = [.width]; editor.textContainer?.widthTracksTextView = true
        editor.isAutomaticQuoteSubstitutionEnabled = false; editor.isAutomaticDashSubstitutionEnabled = false
        editor.isContinuousSpellCheckingEnabled = true
        editor.delegate = context.coordinator; editor.submit = onSubmit; editor.focusChanged = onFocusChange; editor.escape = onEscape
        editor.workspaceTabID = tabID
        editor.setAccessibilityIdentifier("message-input"); editor.setAccessibilityLabel("Message Nanocodex")
        scroll.documentView = editor
        return scroll
    }
    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let editor = scroll.documentView as? ComposerTextView else { return }
        context.coordinator.parent = self; editor.submit = onSubmit; editor.focusChanged = onFocusChange; editor.escape = onEscape
        editor.workspaceTabID = tabID
        if editor.string != text { editor.string = text; context.coordinator.measure(editor) }
        if focusRequest > 0, context.coordinator.lastFocusRequest != focusRequest {
            context.coordinator.lastFocusRequest = focusRequest
            let request = focusRequest, coordinator = context.coordinator
            DispatchQueue.main.async { [weak editor] in
                guard coordinator.parent.focusRequest == request, let editor else { return }
                if let window = editor.window, window.makeFirstResponder(editor) {
                    WorkspaceKeyboardView.find(in: window.contentView)?.flushTyping(to: editor)
                }
            }
        }
    }
    func makeCoordinator() -> Coordinator { Coordinator(self) }
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: NativeComposer
        var lastFocusRequest = 0
        init(_ parent: NativeComposer) { self.parent = parent }
        func textDidChange(_ notification: Notification) { guard let editor = notification.object as? NSTextView else { return }; parent.text = editor.string; measure(editor) }
        func measure(_ editor: NSTextView) {
            guard let layout = editor.layoutManager, let container = editor.textContainer else { return }
            layout.ensureLayout(for: container)
            let next = min(180, max(56, layout.usedRect(for: container).height + 26))
            if abs(parent.height - next) > 1 { DispatchQueue.main.async { self.parent.height = next } }
        }
    }
}
final class ComposerTextView: NSTextView {
    var workspaceTabID: String?
    var submit: (() -> Void)?
    var focusChanged: ((Bool) -> Void)?
    var escape: (() -> Void)?
    override func becomeFirstResponder() -> Bool {
        let accepted = super.becomeFirstResponder()
        if accepted { DispatchQueue.main.async { [weak self] in
            guard let self, self.window?.firstResponder === self else { return }
            self.focusChanged?(true)
        } }
        return accepted
    }
    override func resignFirstResponder() -> Bool {
        let accepted = super.resignFirstResponder()
        if accepted { DispatchQueue.main.async { [weak self] in
            guard let self, self.window?.firstResponder !== self else { return }
            self.focusChanged?(false)
        } }
        return accepted
    }
    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53, !hasMarkedText() {
            escape?()
            if let navigation = WorkspaceKeyboardView.find(in: window?.contentView) { window?.makeFirstResponder(navigation) }
            return
        }
        if [36, 76].contains(event.keyCode), !event.modifierFlags.contains(.shift), !hasMarkedText() { submit?(); return }
        super.keyDown(with: event)
    }
}
