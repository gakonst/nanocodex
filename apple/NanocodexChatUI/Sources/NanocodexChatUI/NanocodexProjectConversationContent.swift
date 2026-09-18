#if os(iOS)
import SwiftUI
import InboxCore
import NanocodexChat
import NanocodexUI

/// Nanocodex's native presentation for an already observed project store.
/// Place inside one ProjectConversationView. Host slots supply authorized
/// attachment/voice actions, project controls and account navigation.
@MainActor public struct NanocodexProjectConversationContent<Leading: View, Voice: View, Actions: View, Footer: View, Tasks: View, Media: View>: View {
    @ObservedObject private var store: ProjectConversationStore
    private let projectTitle: String
    private let identifierPrefix: String
    private let leading: Leading
    private let voice: Voice
    private let actions: Actions
    private let footer: Footer
    private let tasks: Tasks
    private let media: (TranscriptRow) -> Media
    @State private var focused = false
    @State private var overflowing = false
    @State private var expandedEditor = false
    @State private var drawer = false
    @State private var drawerTranslation: CGFloat = 0
    @State private var query = ""
    @State private var pendingHeight: CGFloat = 64
    @State private var followsLatest = true
    @State private var userScrolling = false
    @State private var visibleRow: String?
    @State private var expandedProject = true
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var background: Color { Color(uiColor: .systemBackground) }
    private var selected: AgentCard? { store.cards.first { $0.id == store.selection } }
    private var showsStop: Bool { store.draftBinding.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !store.activeTurns.isEmpty }
    private var canSend: Bool { store.selection != nil && !store.draftBinding.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && store.selection.flatMap { store.pending[$0] } == nil }

    public init(store: ProjectConversationStore, projectTitle: String, identifierPrefix: String = "project-chat",
                @ViewBuilder leading: () -> Leading, @ViewBuilder voice: () -> Voice,
                @ViewBuilder actions: () -> Actions, @ViewBuilder drawerFooter: () -> Footer,
                @ViewBuilder tasks: () -> Tasks, @ViewBuilder media: @escaping (TranscriptRow) -> Media) {
        self.store = store; self.projectTitle = projectTitle; self.identifierPrefix = identifierPrefix
        self.leading = leading(); self.voice = voice(); self.actions = actions()
        self.footer = drawerFooter(); self.tasks = tasks(); self.media = media
    }

    public var body: some View {
        GeometryReader { geometry in
            let width = min(geometry.size.width - 24, 420)
            let reveal = drawer ? width + drawerTranslation : drawerTranslation
            ZStack(alignment: .leading) {
                if drawer || drawerTranslation > 0 {
                    sidebar.frame(width: width, height: geometry.size.height)
                        .allowsHitTesting(drawer).accessibilityHidden(!drawer).transition(.opacity)
                }
                conversation.frame(width: geometry.size.width, height: geometry.size.height)
                    .background(background)
                    .clipShape(RoundedRectangle(cornerRadius: reveal > 0 ? 28 : 0))
                    .shadow(color: .black.opacity(reveal > 0 ? 0.12 : 0), radius: 16, x: -4)
                    .overlay { if drawer { Color.clear.contentShape(Rectangle()).onTapGesture { showDrawer(false) } } }
                    .accessibilityHidden(drawer).offset(x: reveal)
            }
            .clipped().contentShape(Rectangle())
            .simultaneousGesture(DragGesture(minimumDistance: 16).onChanged { value in
                guard drawer || value.startLocation.x <= 28,
                      abs(value.translation.width) > abs(value.translation.height) * 1.5 else { return }
                if drawer { drawerTranslation = max(-width, min(0, value.translation.width)) }
                else if value.translation.width > 0 { focused = false; drawerTranslation = min(width, value.translation.width) }
            }.onEnded { value in
                guard drawer || value.startLocation.x <= 28 else { return }
                let horizontal = abs(value.translation.width) > abs(value.translation.height) * 1.5
                let visible = drawer
                    ? !(horizontal && (value.translation.width < -width * 0.25 || value.predictedEndTranslation.width < -width * 0.5))
                    : horizontal && (value.translation.width > width * 0.25 || value.predictedEndTranslation.width > width * 0.5)
                showDrawer(visible)
            })
        }
        .foregroundStyle(Color.primary).tint(.primary).background(background)
        .onChange(of: store.selection) { _, _ in focused = false; expandedEditor = false; followsLatest = true; visibleRow = nil }
    }

    private func showDrawer(_ visible: Bool) {
        focused = false
        withAnimation(reduceMotion ? nil : .spring(response: 0.32, dampingFraction: 0.92)) {
            drawerTranslation = 0; drawer = visible
        }
    }

    private var conversation: some View {
        VStack(spacing: 0) {
            ChatConversationHeader {
                Button { showDrawer(true) } label: {
                    Image(systemName: "line.3.horizontal").frame(width: 44, height: 44).contentShape(Rectangle())
                }.modifier(ChatHeaderGlass()).accessibilityLabel("Conversations").accessibilityIdentifier(identifierPrefix + "-conversations")
            } title: {
                Button { showDrawer(true) } label: {
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 6) {
                            if selected?.isRunning == true { Circle().fill(NanocodexConversationPalette.running).frame(width: 6, height: 6).accessibilityHidden(true) }
                            Text(projectTitle).font(.subheadline.weight(.semibold)).lineLimit(1)
                        }
                        Text("Agent · " + (selected?.title ?? "")).font(.caption2).foregroundStyle(.secondary)
                    }.frame(minWidth: 0, maxWidth: .infinity, minHeight: 44, alignment: .leading).contentShape(Rectangle())
                }
            } trailing: { HStack(spacing: 0) { actions }.modifier(ChatHeaderGlass()) }
            .zIndex(1)
            transcript
                .overlay(alignment: .topTrailing) {
                    ChatConnectionStatusView(status: !store.isLoading && store.connection == .disconnected ? "Reconnecting" : "",
                                             retry: { Task { await store.resume() } })
                        .padding(.horizontal, 16)
                }
            if let error = store.error {
                Text(error).font(.caption).foregroundStyle(.secondary).padding(12)
                    .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 12)).padding(.horizontal, 12)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 0) { tasks; composer.frame(maxWidth: 620) }
                .padding(.bottom, 4)
                .background {
                    LinearGradient(colors: [background.opacity(0), background, background], startPoint: .top, endPoint: .bottom)
                        .ignoresSafeArea(edges: .bottom)
                }
        }
    }

    private var transcript: some View {
        ScrollViewReader { scroll in
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if store.hasOlder {
                        Button("Earlier messages") { followsLatest = false; Task { await store.loadOlder() } }
                            .font(.caption).disabled(store.isLoadingOlder).accessibilityIdentifier(identifierPrefix + "-earlier")
                    }
                    if store.items.isEmpty && !store.isLoading {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Start a conversation").font(.title2.weight(.medium))
                            Text("Send a message to begin.").foregroundStyle(.secondary)
                        }.padding(.top, 24)
                    }
                    ConversationTranscriptContent(items: store.items) { row in
                        NanocodexMessageContent(row: row) {
                            if let images = row.images {
                                ForEach(Array(images.enumerated()), id: \.offset) { _, image in
                                    ChatImageAttachment(source: image).frame(maxWidth: 240).accessibilityIdentifier("message-image")
                                }
                            }
                            media(row)
                        }
                    } activity: { item in NanocodexActivityView(item: item) }
                    if store.isLoading { ProgressView() }
                    if store.isBrowsingHistory {
                        Button("Latest messages") { followsLatest = true; Task { await store.jumpToLatest(); scroll.scrollTo("latest", anchor: .bottom) } }
                            .font(.caption).accessibilityIdentifier(identifierPrefix + "-latest")
                    }
                    Color.clear.frame(height: 1).id("latest")
                }.scrollTargetLayout().padding(.horizontal, 20).padding(.vertical, 24).frame(maxWidth: 780).frame(maxWidth: .infinity)
            }
            .defaultScrollAnchor(.top)
            .defaultScrollAnchor(.bottom, for: .initialOffset)
            .defaultScrollAnchor(followsLatest ? .bottom : .top, for: .sizeChanges)
            .scrollPosition(id: $visibleRow, anchor: .top)
            .onScrollPhaseChange { _, phase in userScrolling = phase == .interacting || phase == .decelerating }
            .onScrollGeometryChange(for: Bool.self) { geometry in
                geometry.contentSize.height - geometry.visibleRect.maxY <= 25
            } action: { _, atLatest in
                if userScrolling { followsLatest = atLatest }
            }
            .onChange(of: store.items.last?.id) { _, _ in
                if followsLatest && !store.isBrowsingHistory { scroll.scrollTo("latest", anchor: .bottom) }
            }
            .onChange(of: store.isLoading) { _, loading in
                if !loading && followsLatest && !store.isBrowsingHistory { scroll.scrollTo("latest", anchor: .bottom) }
            }
            .contentShape(Rectangle()).scrollDismissesKeyboard(.interactively)
            .accessibilityIdentifier(identifierPrefix + "-transcript")
        }
    }

    private var composer: some View {
        ChatComposerShell(focused: focused, background: background) {
            if let id = store.selection, let item = store.pending[id] {
                ScrollView {
                HStack(alignment: .center, spacing: 8) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(item.error == nil ? "Sending…" : "Couldn’t confirm delivery").font(.system(size: 11)).foregroundStyle(.secondary)
                        Text(item.command.input).font(.system(size: 14)).fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
                        if let error = item.error { Text(error).font(.caption2).foregroundStyle(.secondary) }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                    if item.error != nil {
                        Button { Task { await store.retryPending(for: id) } } label: { Text("Retry").frame(minHeight: 44) }
                            .accessibilityIdentifier(identifierPrefix + "-retry")
                    }
                }.font(.system(size: 13, weight: .medium)).buttonStyle(.plain)
                    .padding(.horizontal, 16).padding(.vertical, 8)
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { pendingHeight = $0 }
                }.frame(height: min(pendingHeight, 180)).padding(.top, 4)
                Rectangle().fill(Color(uiColor: .separator)).frame(height: 0.5).padding(.horizontal, 16)
            }
            ChatComposerInputRow(hasPendingMessages: store.selection.flatMap { store.pending[$0] } != nil) {
                leading
                ChatComposerEditor(text: store.draftBinding, focused: $focused, overflowing: $overflowing)
                    .accessibilityIdentifier(identifierPrefix + "-composer")
                    .overlay(alignment: .topLeading) {
                        if store.draftBinding.wrappedValue.isEmpty {
                            Text("Message " + (selected?.title ?? projectTitle)).lineLimit(1).font(.body).foregroundStyle(.tertiary)
                                .padding(.top, 8).allowsHitTesting(false).accessibilityHidden(true)
                        }
                    }
                voice
                Button {
                    if showsStop { Task { await store.stop() } }
                    else { focused = false; followsLatest = true; Task { await store.send() } }
                } label: {
                    ChatComposerPrimaryControlLabel(active: showsStop || canSend, foreground: background) {
                        Image(systemName: showsStop ? "stop.fill" : "arrow.up")
                    }
                }.buttonStyle(.plain).disabled(!showsStop && !canSend)
                    .accessibilityLabel(showsStop ? "Stop turn" : "Send message")
                    .accessibilityIdentifier(identifierPrefix + (showsStop ? "-stop" : "-send"))
                    .keyboardShortcut(showsStop ? nil : KeyboardShortcut(.return, modifiers: .command))
            } expansion: {
                if overflowing {
                    Button { focused = false; expandedEditor = true } label: {
                        ChatComposerControlLabel { Image(systemName: "arrow.up.left.and.arrow.down.right") }
                    }.buttonStyle(.plain).foregroundStyle(.secondary).accessibilityLabel("Expand message editor")
                        .accessibilityIdentifier("expand-composer")
                }
            }
        }
        .sheet(isPresented: $expandedEditor) {
            ChatExpandedComposer(draft: store.draftBinding, canSend: canSend, attachmentCount: 0, background: background,
                onCollapse: { expandedEditor = false }, onSend: { expandedEditor = false; followsLatest = true; Task { await store.send() } })
        }
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                Text("Nanocodex").font(.headline); Spacer()
                Button { showDrawer(false) } label: { Image(systemName: "xmark").frame(width: 44, height: 44) }
                    .accessibilityLabel("Return to conversation")
            }
            HStack {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search projects", text: $query).textInputAutocapitalization(.never).autocorrectionDisabled()
                if !query.isEmpty {
                    Button { query = "" } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary) }.accessibilityLabel("Clear search")
                }
            }.padding(12).background(ChatPalette.userBubble, in: RoundedRectangle(cornerRadius: 14))
            Text("Projects").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            ScrollView {
                LazyVStack(spacing: 4) {
                    HStack(spacing: 0) {
                        Text(projectTitle).font(.subheadline).lineLimit(2).padding(.horizontal, 14).padding(.vertical, 12)
                            .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
                        Button { withAnimation(reduceMotion ? nil : .snappy(duration: 0.2)) { expandedProject.toggle() } } label: {
                            Image(systemName: expandedProject ? "chevron.down" : "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                                .frame(width: 44, height: 48).contentShape(Rectangle())
                        }.accessibilityLabel((expandedProject ? "Collapse " : "Expand ") + projectTitle)
                    }
                    if expandedProject || !query.isEmpty {
                        ConversationListContent(cards: store.cards.filter { query.isEmpty || projectTitle.localizedCaseInsensitiveContains(query) || $0.title.localizedCaseInsensitiveContains(query) }) { card in
                            Button { showDrawer(false); Task { await store.select(card.id) } } label: {
                                ChatConversationRowLabel(title: card.title, running: card.isRunning, selected: store.selection == card.id,
                                                         runningColor: NanocodexConversationPalette.running)
                            }.accessibilityIdentifier(identifierPrefix + "-row:" + card.id)
                                .accessibilityAddTraits(store.selection == card.id ? [.isSelected] : [])
                        } empty: { EmptyView() }
                    }
                }
            }.scrollDismissesKeyboard(.interactively)
            footer
        }.padding(16).background(background).buttonStyle(.plain)
    }
}
#endif
