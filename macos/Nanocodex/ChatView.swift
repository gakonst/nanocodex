import AppKit
import SwiftUI

struct ChatView: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(model.activeTab.map(model.title) ?? "New thread").font(.system(size: 13, weight: .medium)).lineLimit(1)
                Spacer()
                if let snapshot = model.activeSnapshot, !snapshot.connected {
                    Label("Reconnecting…", systemImage: "arrow.triangle.2.circlepath").font(.caption).foregroundStyle(.secondary)
                }
                Button { model.screen = .hands } label: {
                    Label(model.connectedHands.isEmpty ? "Hands" : "\(model.connectedHands.count) Hand\(model.connectedHands.count == 1 ? "" : "s")", systemImage: "hand.raised")
                }.buttonStyle(.plain).font(.system(size: 12)).foregroundStyle(.secondary).help("Manage compute")
            }.padding(.horizontal, 26).padding(.vertical, 20)
            if model.activeMessages.isEmpty && model.pending[model.activeTabID] == nil {
                if let id = model.activeTab?.threadId, model.loading.contains(id) {
                    Spacer(); ProgressView("Loading thread…").controlSize(.small); Spacer()
                } else { WelcomeView() }
            } else { TranscriptView() }
            ComposerView().frame(maxWidth: 780).padding(.horizontal, 26).padding(.top, 14).padding(.bottom, 18)
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct WelcomeView: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 13) {
            Spacer()
            Text("Let’s build").font(.system(size: 43, weight: .semibold, design: .default)).tracking(-1.3)
            Text("Something great.").font(.system(size: 25, weight: .medium)).foregroundStyle(.secondary)
            if !model.state.connected && !model.isStarting {
                Button { model.showingSettings = true } label: { Label("Connect your account", systemImage: "person.crop.circle.badge.checkmark") }
                    .buttonStyle(.borderedProminent).tint(.primary).padding(.top, 12)
            } else {
                Menu {
                    Button("Choose Working Folder…") { model.chooseFolder() }
                    Divider()
                    Button("Let the agent choose compute") { model.updateTarget("") }
                    ForEach(model.selectableHands) { hand in Button(hand.name) { model.updateTarget(hand.id) } }
                    Divider()
                    Button("Add a Hand…") { model.screen = .hands }
                } label: {
                    Label(model.activeTab?.folder.isEmpty == false ? URL(fileURLWithPath: model.activeTab?.folder ?? "").lastPathComponent : "Choose a folder", systemImage: "folder")
                        .font(.system(size: 13)).foregroundStyle(.secondary)
                }.menuStyle(.borderlessButton).fixedSize().padding(.top, 9)
            }
            Spacer()
            HStack(spacing: 10) {
                suggestion("Build something", icon: "hammer") { model.updateDraft("Help me build ") }
                suggestion("Explore my code", icon: "chevron.left.forwardslash.chevron.right") { model.updateDraft("Explore my codebase and explain how it works.") }
                suggestion("Plan a task", icon: "list.bullet") { model.updateDraft("Help me plan ") }
            }.padding(.bottom, 12)
        }.frame(maxWidth: 728, maxHeight: .infinity, alignment: .leading).padding(.horizontal, 26)
    }
    private func suggestion(_ title: String, icon: String, action: @escaping () -> Void) -> some View {
        Button(action: action) { Label(title, systemImage: icon).font(.system(size: 12)).lineLimit(1).foregroundStyle(.secondary).padding(.horizontal, 13).padding(.vertical, 10).background(Color.primary.opacity(0.025), in: Capsule()).overlay(Capsule().stroke(Color.primary.opacity(0.07))) }.buttonStyle(.plain)
    }
}

struct TranscriptView: View {
    @EnvironmentObject private var model: AppModel
    @State private var followOutput = true
    @State private var pendingScroll: Task<Void, Never>?
    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 24) {
                    if model.activeSnapshot?.hasMore == true { Button("Load earlier messages") { Task { await model.loadOlder() } }.buttonStyle(.link).frame(maxWidth: .infinity) }
                    ForEach(model.activeMessages) { entry in MessageView(entry: entry).equatable().id(entry.id) }
                    if let pending = model.pending[model.activeTabID] {
                        MessageView(entry: .init(id: pending.id, turnId: pending.id, kind: .user, text: pending.text))
                    }
                    if model.isRunning || model.pending[model.activeTabID] != nil {
                        HStack(spacing: 9) { ProgressView().controlSize(.mini); Text("Working…").font(.system(size: 13)).foregroundStyle(.secondary) }.accessibilityIdentifier("agent-working")
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }.frame(maxWidth: 728, alignment: .leading).padding(.horizontal, 26).padding(.top, 20).padding(.bottom, 12).frame(maxWidth: .infinity)
                    .background(ScrollFollowObserver(follows: followOutput) { nearBottom in followOutput = nearBottom })
            }
            .defaultScrollAnchor(.bottom)
            .onChange(of: model.activeTabID) { pendingScroll?.cancel(); pendingScroll = nil; followOutput = true; scrollToLatest(proxy) }
            .onChange(of: model.activeMessages.last) { scrollToLatest(proxy) }
            .onChange(of: model.pending[model.activeTabID]?.id) { scrollToLatest(proxy) }
            .onDisappear { pendingScroll?.cancel() }
            .overlay(alignment: .bottomTrailing) {
                if !followOutput {
                    Button { followOutput = true; proxy.scrollTo("bottom", anchor: .bottom) } label: {
                        Label("Latest", systemImage: "arrow.down").font(.system(size: 12, weight: .medium)).padding(.horizontal, 12).padding(.vertical, 8)
                    }.buttonStyle(.plain).background(.regularMaterial, in: Capsule()).overlay(Capsule().strokeBorder(Color.primary.opacity(0.08))).padding(15).help("Jump to the latest response")
                }
            }
        }.accessibilityIdentifier("transcript")
    }
    private func scrollToLatest(_ proxy: ScrollViewProxy) {
        guard followOutput, pendingScroll == nil else { return }
        pendingScroll = Task { @MainActor in
            do { try await Task.sleep(for: .milliseconds(45)) } catch { return }
            pendingScroll = nil
            guard !Task.isCancelled, followOutput else { return }
            proxy.scrollTo("bottom", anchor: .bottom)
        }
    }
}

/// Live-scroll notifications distinguish the user's gesture from programmatic stream following.
struct ScrollFollowObserver: NSViewRepresentable {
    var follows: Bool
    var changed: (Bool) -> Void
    func makeNSView(context: Context) -> ObserverView { let view = ObserverView(); view.changed = changed; view.follows = follows; return view }
    func updateNSView(_ view: ObserverView, context: Context) { view.changed = changed; view.follows = follows; view.scheduleFollow() }
    final class ObserverView: NSView {
        var changed: ((Bool) -> Void)?
        var follows = true
        private var correctionScheduled = false
        private weak var observed: NSScrollView?
        private var tokens: [NSObjectProtocol] = []
        override func layout() {
            super.layout()
            guard let scroll = enclosingScrollView, observed !== scroll else { return }
            tokens.forEach(NotificationCenter.default.removeObserver); tokens = []; observed = scroll
            scroll.documentView?.postsFrameChangedNotifications = true
            if let document = scroll.documentView {
                tokens.append(NotificationCenter.default.addObserver(forName: NSView.frameDidChangeNotification, object: document, queue: .main) { [weak self] _ in self?.scheduleFollow() })
            }
            for name in [NSScrollView.didLiveScrollNotification, NSScrollView.didEndLiveScrollNotification] {
                tokens.append(NotificationCenter.default.addObserver(forName: name, object: scroll, queue: .main) { [weak self, weak scroll] _ in
                    guard let scroll, let document = scroll.documentView else { return }
                    let visible = scroll.documentVisibleRect
                    let remaining = document.isFlipped ? document.bounds.maxY - visible.maxY : visible.minY - document.bounds.minY
                    self?.follows = remaining < 32
                    self?.changed?(remaining < 32)
                })
            }
        }
        func scheduleFollow() {
            guard follows, !correctionScheduled else { return }
            correctionScheduled = true
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.correctionScheduled = false
                guard self.follows, let scroll = self.observed, let document = scroll.documentView else { return }
                // Correct the estimate after lazy rows finish laying out, without animation or a timer loop.
                let clip = scroll.contentView
                let y = document.isFlipped ? max(document.bounds.minY, document.bounds.maxY - clip.bounds.height) : document.bounds.minY
                guard abs(clip.bounds.minY - y) > 1 else { return }
                clip.scroll(to: NSPoint(x: clip.bounds.minX, y: y)); scroll.reflectScrolledClipView(clip)
            }
        }
        deinit { tokens.forEach(NotificationCenter.default.removeObserver) }
    }
}

struct MessageView: View, Equatable {
    let entry: MessageEntry
    static func == (lhs: MessageView, rhs: MessageView) -> Bool { lhs.entry == rhs.entry }
    var body: some View {
        Group {
            switch entry.kind {
            case .user:
                HStack { Spacer(minLength: 80); Text(visiblePrompt(entry.text)).font(.system(size: 14)).textSelection(.enabled).padding(.horizontal, 17).padding(.vertical, 12).background(Color.primary.opacity(0.055), in: RoundedRectangle(cornerRadius: 18)) }.accessibilityIdentifier("user-message")
            case .assistant:
                VStack(alignment: .leading, spacing: 12) {
                    NativeMarkdown(text: entry.text)
                    if !entry.streaming {
                        Button { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(entry.text, forType: .string) } label: { Image(systemName: "doc.on.doc").font(.system(size: 12)).foregroundStyle(.tertiary) }.buttonStyle(.plain).help("Copy response")
                    }
                }.accessibilityIdentifier("assistant-message")
            case .reasoning:
                DisclosureGroup { Text(entry.text).font(.system(size: 13)).foregroundStyle(.secondary).textSelection(.enabled).padding(.top, 8) } label: { Label(entry.streaming ? "Thinking…" : "Thought process", systemImage: "sparkle").font(.system(size: 12)).foregroundStyle(.secondary) }
            case .tool:
                DisclosureGroup {
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
    private func visiblePrompt(_ text: String) -> String {
        let markers = ["\n\n[Selected Hand:", "\n\n[Working folder selected in Nanocodex:"]
        return markers.reduce(text) { value, marker in value.components(separatedBy: marker).first ?? value }
    }
}

struct NativeMarkdown: View {
    let text: String
    private var parts: [String] { text.components(separatedBy: "```") }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(parts.enumerated()), id: \.offset) { index, part in
                if index.isMultiple(of: 2) {
                    Text((try? AttributedString(markdown: part, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(part)).font(.system(size: 14)).lineSpacing(5).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    let lines = part.components(separatedBy: "\n")
                    let code = lines.dropFirst().joined(separator: "\n").trimmingCharacters(in: .newlines)
                    VStack(spacing: 0) {
                        HStack { Text(lines.first ?? "").font(.system(size: 11)).foregroundStyle(.secondary); Spacer(); Button { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(code, forType: .string) } label: { Label("Copy", systemImage: "doc.on.doc").font(.system(size: 11)) }.buttonStyle(.plain) }.padding(10)
                        Divider().opacity(0.4)
                        ScrollView(.horizontal) { Text(code).font(.system(size: 12, design: .monospaced)).textSelection(.enabled).padding(14).frame(maxWidth: .infinity, alignment: .leading) }
                    }.background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
                }
            }
        }
    }
}

struct ComposerView: View {
    @EnvironmentObject private var model: AppModel
    @State private var editorHeight: CGFloat = 72
    @State private var editorFocused = false
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let folder = model.activeTab?.folder, !folder.isEmpty {
                HStack(spacing: 6) { Image(systemName: "folder"); Text(URL(fileURLWithPath: folder).lastPathComponent); Button { model.updateTab { $0.folder = "" } } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary) }.buttonStyle(.plain) }.font(.system(size: 11)).foregroundStyle(.secondary).padding(.horizontal, 16).padding(.top, 12).help("Shared with this thread when you send")
            }
            ZStack(alignment: .topLeading) {
                if model.activeTab?.draft.isEmpty ?? true { Text(model.isRunning ? "Add a follow-up, or steer the current turn…" : "Ask Nanocodex to build anything").font(.system(size: 14)).foregroundStyle(.tertiary).padding(.horizontal, 17).padding(.top, 18).allowsHitTesting(false) }
                NativeComposer(text: Binding(get: { model.activeTab?.draft ?? "" }, set: model.updateDraft), height: $editorHeight, onSubmit: { Task { await model.send() } }, onFocusChange: { editorFocused = $0 }).frame(height: editorHeight).padding(.horizontal, 8).padding(.top, 8)
            }
            HStack(spacing: 13) {
                Menu {
                    Button("Choose Working Folder…") { model.chooseFolder() }
                    Divider()
                    Button("Manage Hands…") { model.screen = .hands }
                    Button("Manage Connections…") { model.openAccount() }
                } label: { Image(systemName: "plus").font(.system(size: 15)).foregroundStyle(.secondary) }.menuStyle(.borderlessButton).fixedSize().help("Add context")
                ModelMenu()
                Menu {
                    Button("Let the agent choose") { model.updateTarget("") }
                    ForEach(model.selectableHands) { hand in Button(hand.name) { model.updateTarget(hand.id) } }
                    Divider()
                    Button("Manage Hands…") { model.screen = .hands }
                } label: {
                    HStack(spacing: 5) { Image(systemName: "hand.raised"); Text(model.state.hands.first(where: { $0.id == model.activeTab?.target })?.name ?? "Auto") }.font(.system(size: 11)).foregroundStyle(.secondary)
                }.menuStyle(.borderlessButton).fixedSize().help("Compute for this tab")
                Spacer(minLength: 0)
                if model.isRunning {
                    if model.hasUnsentMessage { Button("Steer") { Task { await model.steer() } }.buttonStyle(.bordered).controlSize(.small).help("Send this instruction to the current turn") }
                    Button { Task { await model.cancel() } } label: { Image(systemName: "stop.fill").font(.system(size: 11)).frame(width: 30, height: 30).background(Color.primary.opacity(0.07), in: Circle()) }.buttonStyle(.plain).help("Stop current turn (⌘.)").accessibilityIdentifier("stop-turn")
                }
                Button { Task { await model.send() } } label: {
                    Image(systemName: "arrow.up").font(.system(size: 14, weight: .semibold)).foregroundStyle(Color(nsColor: .textBackgroundColor)).frame(width: 30, height: 30).background(Color.primary.opacity(model.hasUnsentMessage ? 1 : 0.18), in: Circle())
                }.buttonStyle(.plain).disabled(!model.hasUnsentMessage || model.pending[model.activeTabID] != nil).help(model.isRunning ? "Queue a follow-up" : "Send message (Return)").accessibilityIdentifier("send-message")
            }.padding(.horizontal, 16).padding(.bottom, 12).padding(.top, 3)
        }
        .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 22))
        .overlay(RoundedRectangle(cornerRadius: 22).strokeBorder(Color.primary.opacity(editorFocused ? 0.27 : 0.14), lineWidth: 1))
        .shadow(color: Color.black.opacity(editorFocused ? 0.045 : 0.025), radius: 8, y: 3)
    }
}

struct ModelMenu: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        Menu {
            Picker("Model", selection: Binding(get: { model.settings.model }, set: { model.settings.selectModel($0); model.updateSettings() })) {
                Text("Astra").tag("gpt-6-astra")
                Text("Sol").tag("gpt-5.6-sol")
                Text("Terra · Balanced").tag("gpt-5.6-terra")
                Text("Luna · Fast").tag("gpt-5.6-luna")
            }.disabled(model.activeSnapshot?.hasAcceptedTurn == true)
            Picker("Reasoning", selection: Binding(get: { model.settings.thinking }, set: { model.settings.thinking = $0; model.updateSettings() })) {
                Text("None").tag("none").disabled(!model.settings.supportsNoReasoning)
                Text("Low").tag("low")
                Text("Medium").tag("medium")
                Text("High").tag("high")
                Text("Extra high").tag("xhigh")
                Text("Max").tag("max")
            }
            Divider()
            Toggle("Pro reasoning", isOn: Binding(get: { model.settings.reasoning_mode == "pro" }, set: { model.settings.reasoning_mode = $0 ? "pro" : "standard"; model.updateSettings() }))
                .disabled(!model.settings.supportsProReasoning || model.activeSnapshot?.hasAcceptedTurn == true)
            Toggle("Fast mode", isOn: Binding(get: { model.settings.fast_mode }, set: { model.settings.fast_mode = $0; model.updateSettings() }))
            if model.activeSnapshot?.hasAcceptedTurn == true {
                Divider()
                Text("Start a new thread to change the model or Pro.")
            }
        } label: {
            HStack(spacing: 5) { Text("\(model.settings.modelName) · \(model.settings.thinking.capitalized)"); if model.settings.fast_mode { Image(systemName: "bolt.fill") } }.font(.system(size: 11)).foregroundStyle(.secondary)
        }.menuStyle(.borderlessButton).fixedSize()
    }
}

struct NativeComposer: NSViewRepresentable {
    @Binding var text: String
    @Binding var height: CGFloat
    var onSubmit: () -> Void
    var onFocusChange: (Bool) -> Void = { _ in }
    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView(), editor = ComposerTextView()
        scroll.drawsBackground = false; scroll.hasVerticalScroller = true; scroll.autohidesScrollers = true
        editor.isRichText = false; editor.drawsBackground = false; editor.font = .systemFont(ofSize: 14)
        editor.textColor = .labelColor; editor.insertionPointColor = .labelColor
        editor.textContainerInset = NSSize(width: 7, height: 10)
        editor.isVerticallyResizable = true; editor.isHorizontallyResizable = false
        editor.autoresizingMask = [.width]; editor.textContainer?.widthTracksTextView = true
        editor.isAutomaticQuoteSubstitutionEnabled = false; editor.isAutomaticDashSubstitutionEnabled = false
        editor.isContinuousSpellCheckingEnabled = true
        editor.delegate = context.coordinator; editor.submit = onSubmit; editor.focusChanged = onFocusChange
        editor.setAccessibilityIdentifier("message-input"); editor.setAccessibilityLabel("Message Nanocodex")
        scroll.documentView = editor
        return scroll
    }
    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let editor = scroll.documentView as? ComposerTextView else { return }
        context.coordinator.parent = self; editor.submit = onSubmit; editor.focusChanged = onFocusChange
        if editor.string != text { editor.string = text; context.coordinator.measure(editor) }
    }
    func makeCoordinator() -> Coordinator { Coordinator(self) }
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: NativeComposer
        init(_ parent: NativeComposer) { self.parent = parent }
        func textDidChange(_ notification: Notification) { guard let editor = notification.object as? NSTextView else { return }; parent.text = editor.string; measure(editor) }
        func measure(_ editor: NSTextView) {
            guard let layout = editor.layoutManager, let container = editor.textContainer else { return }
            layout.ensureLayout(for: container)
            let next = min(180, max(72, layout.usedRect(for: container).height + 26))
            if abs(parent.height - next) > 1 { DispatchQueue.main.async { self.parent.height = next } }
        }
    }
}
final class ComposerTextView: NSTextView {
    var submit: (() -> Void)?
    var focusChanged: ((Bool) -> Void)?
    override func becomeFirstResponder() -> Bool {
        let accepted = super.becomeFirstResponder()
        if accepted { DispatchQueue.main.async { [weak self] in self?.focusChanged?(true) } }
        return accepted
    }
    override func resignFirstResponder() -> Bool {
        let accepted = super.resignFirstResponder()
        if accepted { DispatchQueue.main.async { [weak self] in self?.focusChanged?(false) } }
        return accepted
    }
    override func keyDown(with event: NSEvent) {
        if [36, 76].contains(event.keyCode), !event.modifierFlags.contains(.shift), !hasMarkedText() { submit?(); return }
        super.keyDown(with: event)
    }
}
