#if DEBUG
import SwiftUI
import InboxCore
import NanocodexChat
import NanocodexChatUI
import NanocodexUI
import NanocodexVoice

/// Executable consumer example, hosted by the app only with --component-gallery.
/// All account, conversation and response data in this gallery are fixtures.
@MainActor struct ProjectConversationGallery: View {
    @StateObject private var transport: GalleryConversationTransport
    @StateObject private var store: ProjectConversationStore
    @State private var appearance = ProcessInfo.processInfo.arguments.contains("--component-native-style") ? "Nanocodex" : "Unstyled"
    @StateObject private var voiceSession = VoiceSession()
    @State private var showAttachments = false
    @State private var hostNotice: String?
    @State private var showTasks = false
    private var nativeOnly: Bool { ProcessInfo.processInfo.arguments.contains("--component-native-style") }
    @State private var showsConversations = false
    @FocusState private var composerFocused: Bool
    private var custom: Bool { appearance == "Nanocodex" }

    init() {
        let transport = GalleryConversationTransport()
        _transport = StateObject(wrappedValue: transport)
        _store = StateObject(wrappedValue: ProjectConversationStore(transport: transport))
    }

    var body: some View {
        ProjectConversationView(store: store) { state in
            VStack(spacing: 0) {
                if !nativeOnly {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Component gallery · fixture data").font(.caption).foregroundStyle(.secondary)
                        Picker("Presentation", selection: $appearance) {
                            Text("Unstyled").tag("Unstyled")
                            Text("Nanocodex").tag("Nanocodex")
                        }.pickerStyle(.segmented).accessibilityIdentifier("gallery-style")
                    }.padding(16)
                }
                if custom {
                    NanocodexProjectConversationContent(store: state, projectTitle: "DJ Booth", identifierPrefix: "gallery") {
                        Button { showAttachments = true } label: {
                            ChatComposerControlLabel { Image(systemName: "plus") }
                        }.accessibilityLabel("Add attachments").accessibilityIdentifier("add-attachments")
                    } voice: {
                        NanocodexVoiceControl(session: voiceSession) {
                            throw NSError(domain: "ComponentGallery", code: 1, userInfo: [NSLocalizedDescriptionKey: "Voice requires a host-provided account connection. This gallery uses fixture data."])
                        }
                    } actions: {
                        Button { hostNotice = "Create a project through the host app." } label: {
                            Image(systemName: "square.and.pencil").frame(width: 44, height: 44)
                        }.accessibilityLabel("New project")
                        scenarios
                    } drawerFooter: {
                        HStack {
                            Button { hostNotice = "Create a project through the host app." } label: {
                                Label("New project", systemImage: "square.and.pencil").padding(12)
                            }.background(NanocodexConversationPalette.userMessage, in: Capsule())
                            Spacer()
                            Button { hostNotice = "Account settings belong to the host app." } label: {
                                Image(systemName: "gearshape").frame(width: 44, height: 44)
                            }.accessibilityLabel("Account settings")
                        }
                    } tasks: {
                        Button { showTasks = true } label: {
                            HStack(spacing: 6) { Image(systemName: "circle.dotted"); Text("0 tasks") }
                                .font(.caption.weight(.medium)).padding(.horizontal, 12).padding(.vertical, 7)
                                .background(ChatPalette.userBubble, in: Capsule()).frame(minHeight: 44)
                        }.buttonStyle(.plain)
                    } media: { _ in EmptyView() }
                } else {
                    VStack(alignment: .leading, spacing: 16) {
                        HStack {
                            Button { showsConversations = true } label: { Label("Conversations", systemImage: "sidebar.left") }
                                .accessibilityIdentifier("gallery-conversations")
                            Spacer(); scenarios
                        }
                        Text(state.cards.first { $0.id == state.selection }?.title ?? "Choose a conversation").font(.headline)
                        transcript(state)
                        if transport.failNextSend { Text("Next send will be interrupted").font(.caption).accessibilityIdentifier("gallery-interruption-armed") }
                        if let error = state.error { Text(error).font(.caption).foregroundStyle(.secondary) }
                        if state.isLoading { ProgressView("Loading conversation") }
                        pending(state)
                        composer(state)
                    }.padding(20).background(Color(uiColor: .systemBackground))
                        .sheet(isPresented: $showsConversations) { sidebar(state) }
                }
            }
        }
        .task { await store.select("booth") }
        .preferredColorScheme(ProcessInfo.processInfo.arguments.contains("--component-dark") ? .dark : nil)
        .sheet(isPresented: $showAttachments) {
            NavigationStack {
                List {
                    Section {
                        attachmentOption("Photos & Videos", icon: "photo.on.rectangle")
                        attachmentOption("Camera", icon: "camera")
                        attachmentOption("Files", icon: "folder")
                    }
                    Section { attachmentOption("Context from other apps", icon: "tray.full") }
                }.navigationTitle("Add to conversation").navigationBarTitleDisplayMode(.inline)
                    .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showAttachments = false } } }
            }.tint(.primary).presentationDetents([.medium, .large]).presentationDragIndicator(.visible).presentationCornerRadius(30)
        }
        .sheet(isPresented: $showTasks) {
            NavigationStack { Text("No tasks yet").foregroundStyle(.secondary).navigationTitle("Tasks")
                    .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showTasks = false } } }
            }.presentationDetents([.medium, .large]).presentationDragIndicator(.visible).presentationCornerRadius(28)
        }
        .alert("Fixture gallery", isPresented: Binding(get: { hostNotice != nil }, set: { if !$0 { hostNotice = nil } })) {
            Button("OK") { hostNotice = nil }
        } message: { Text(hostNotice ?? "") }
    }

    private var scenarios: some View {
        Menu {
            Button("Interrupt next send") { transport.failNextSend = true }
        } label: { Image(systemName: "ellipsis").frame(width: 44, height: 44) }
            .accessibilityIdentifier("gallery-scenarios")
    }
    private func attachmentOption(_ title: String, icon: String) -> some View {
        Button { showAttachments = false; hostNotice = "Attachments require a host-provided upload handler. This gallery uses fixture data." } label: {
            Label(title, systemImage: icon).frame(minHeight: 32)
        }
    }

    private func transcript(_ state: ProjectConversationStore) -> some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                if state.hasOlder {
                    Button("Earlier messages") { Task { await state.loadOlder() } }
                        .disabled(state.isLoadingOlder).accessibilityIdentifier("gallery-earlier")
                }
                ConversationTranscriptContent(items: state.items) { row in
                    GalleryConversationMessage(row: row)
                } activity: { item in
                    DisclosureGroup(item.isRunning ? "Working" : "Activity") {
                        ForEach(item.activity) { row in Text(row.text).font(.caption) }
                    }
                }
                if state.isBrowsingHistory {
                    Button("Latest messages") { Task { await state.jumpToLatest() } }
                        .accessibilityIdentifier("gallery-latest")
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
        }.accessibilityIdentifier("gallery-transcript")
    }

    @ViewBuilder private func pending(_ state: ProjectConversationStore) -> some View {
        if let id = state.selection, let item = state.pending[id] {
            VStack(alignment: .leading, spacing: 8) {
                Text(item.command.input)
                if let error = item.error {
                    Text(error).font(.caption).foregroundStyle(.secondary)
                    Button("Retry same message") { Task { await state.retryPending(for: id) } }
                        .accessibilityIdentifier("gallery-retry")
                } else { Text(item.isSending ? "Sending…" : "Waiting for transcript…").font(.caption) }
            }.padding(12).background(.quaternary, in: RoundedRectangle(cornerRadius: 4))
        }
    }

    private func composer(_ state: ProjectConversationStore) -> some View {
        HStack(alignment: .bottom, spacing: 12) {
            TextField("Message", text: state.draftBinding, axis: .vertical)
                .focused($composerFocused)
                .lineLimit(1...4).textFieldStyle(.roundedBorder).accessibilityIdentifier("gallery-composer")
            if !state.activeTurns.isEmpty {
                Button("Stop") { Task { await state.stop() } }.accessibilityIdentifier("gallery-stop")
            }
            Button("Send") { composerFocused = false; Task { await state.send() } }
                .disabled(state.selection == nil || state.draftBinding.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier("gallery-send")
        }
    }

    private func sidebar(_ state: ProjectConversationStore) -> some View {
        NavigationStack {
            List {
                ConversationListContent(cards: state.cards) { card in
                    Button {
                        showsConversations = false
                        Task { await state.select(card.id) }
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 6) {
                                Text(card.title)
                                Text(card.isRunning ? "Working" : "Conversation").font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            if state.selection == card.id { Image(systemName: "checkmark") }
                        }.padding(.vertical, 2)
                    }.accessibilityIdentifier("gallery-row:" + card.id)
                } empty: { Text("No conversations") }
            }
            .navigationTitle("DJ Booth")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { showsConversations = false } } }
        }.tint(.accentColor)
    }
}

private struct GalleryConversationMessage: View {
    let row: TranscriptRow
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(row.role == "You" ? "You" : "Booth").font(.caption).foregroundStyle(.secondary)
            Text(row.text).textSelection(.enabled)
        }
        .padding(0)
        .frame(maxWidth: .infinity, alignment: .leading)

    }
}

@MainActor private final class GalleryConversationTransport: ObservableObject, ProjectConversationTransport {
    let scope = ProjectConversationScope(projectID: "gallery-fixture", conversations: [
        .init(id: "booth", title: "Opening set"), .init(id: "crate", title: "Late-night crate")
    ])
    @Published var failNextSend = false
    private var archive: [String: [AgentEvent]] = [:]
    private var active: [String: [String]] = [:]
    private var listeners: [String: (UUID, @Sendable (ProjectConversationFrame) async -> Void)] = [:]
    private var completions: [String: Task<Void, Never>] = [:]
    private var nextCursor = 7

    init() {
        for id in ["booth", "crate"] {
            archive[id] = [
                Self.event(1, "turn_accepted", "intro", text: "Keep the opening relaxed and warm."),
                Self.event(2, "turn_completed", "intro", text: "I’ll start with spacious percussion and build the energy gradually."),
                Self.event(3, "turn_accepted", "tempo", text: "Around 120 BPM, with room to move."),
                Self.event(4, "turn_completed", "tempo", text: "That leaves room for a gentle lift into the next hour."),
                Self.event(5, "turn_accepted", "latest", text: id == "booth" ? "What should the first three tracks feel like?" : "Let’s keep a separate crate for later."),
                Self.event(6, "turn_completed", "latest", text: id == "booth" ? "1. A warm, rolling groove.\n2. A little more percussion.\n3. A melodic track that opens up the room." : "This conversation has its own draft and history. We can explore a deeper sound here.")
            ]
        }
    }
    private static func event(_ cursor: Int, _ type: String, _ turn: String, text: String) -> AgentEvent {
        try! AgentEvent(.object(["cursor": .string(String(cursor)), "type": .string(type), "turn_id": .string(turn),
            type == "turn_accepted" ? "input" : "final_message": .string(text)]))
    }
    func state(_ id: String) async throws -> JSON {
        .object(["agent_id": .string(id), "latest_event_cursor": .string(archive[id]?.last?.cursor.rawValue ?? "0"),
                 "active_turns": .array((active[id] ?? []).map(JSON.string))])
    }
    func history(_ id: String, before: Cursor?, after: Cursor?) async throws -> EventPage {
        let all = (archive[id] ?? []).filter { (before == nil || $0.cursor < before!) && (after == nil || $0.cursor > after!) }
        let page = before == nil && after == nil ? Array(all.suffix(2)) : Array(all.suffix(4))
        return try EventPage(.object(["data": .array(page.map(\.data)), "has_more": .bool(all.count > page.count),
            "latest_cursor": .string(archive[id]?.last?.cursor.rawValue ?? "0")]))
    }
    func stream(_ id: String, after: Cursor, receive: @escaping @Sendable (ProjectConversationFrame) async -> Void) async throws {
        let token = UUID(); listeners[id] = (token, receive)
        defer { if listeners[id]?.0 == token { listeners[id] = nil } }
        for event in archive[id] ?? [] where event.cursor > after { await receive(.init(event: event, cursor: event.cursor)) }
        try await Task.sleep(for: .seconds(3600))
    }
    func send(_ command: AgentCommand) async throws {
        if failNextSend { failNextSend = false; throw URLError(.networkConnectionLost) }
        if command.kind == .stop {
            completions[command.turnID]?.cancel()
            active[command.agentID]?.removeAll { $0 == command.turnID }
            await emit(command.agentID, type: "turn_cancelled", turn: command.turnID, text: "Stopped")
            return
        }
        if archive[command.agentID]?.contains(where: { $0.turnID == command.requestID }) == true { return }
        active[command.agentID, default: []].append(command.requestID)
        await emit(command.agentID, type: "turn_accepted", turn: command.requestID, text: command.input)
        completions[command.requestID] = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(12)) } catch { return }
            guard let self else { return }
            self.active[command.agentID]?.removeAll { $0 == command.requestID }
            await self.emit(command.agentID, type: "turn_completed", turn: command.requestID,
                            text: "I’ll keep that direction for this set. The same conversation state works with either presentation.")
            self.completions[command.requestID] = nil
        }
    }
    private func emit(_ id: String, type: String, turn: String, text: String) async {
        let event = Self.event(nextCursor, type, turn, text: text); nextCursor += 1
        archive[id, default: []].append(event)
        if let receive = listeners[id]?.1 { await receive(.init(event: event, cursor: event.cursor)) }
    }
}
#endif
