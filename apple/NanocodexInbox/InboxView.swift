import SwiftUI
import PhotosUI
import UniformTypeIdentifiers
import ImageIO
import AVKit
import InboxCore
import NanocodexRemote
import NanocodexVoice
import NanocodexContext
import NanocodexUI
import UIKit
import AVFoundation

private enum Ink {
    static let background = ChatPalette.background
    static let card = Color(uiColor: .secondarySystemGroupedBackground)
    static let surface = ChatPalette.userBubble
    static let border = Color(uiColor: .separator)
    static let text = Color.primary
    static let muted = Color.secondary
    static let accent = Color.primary
    static let amber = Color.secondary
}

struct InboxView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drag: CGFloat = 0
    @State private var upwardDrag: CGFloat = 0
    @State private var cardDrag: CardDrag?
    @State private var previewGeometry = CardPreviewGeometry()
    @State private var showThread = false
    @State private var showAgents = false
    @GestureState(resetTransaction: Transaction(animation: .snappy(duration: 0.28))) private var sidebarDrag: CGFloat = 0
    @State private var showScheduledJobs = false
    @State private var showSettings = false
    @State private var showScreens = false
    @FocusState private var composerFocused: Bool

    private let newThreadPullThreshold: CGFloat = 160
    private var newThreadPullProgress: CGFloat { min(1, max(0, -upwardDrag / newThreadPullThreshold)) }
    private var newThreadPullReady: Bool { -upwardDrag >= newThreadPullThreshold }
    private var newThreadPullOffset: CGFloat {
        let distance = max(0, -upwardDrag)
        return -(min(distance, newThreadPullThreshold) * 0.6 + max(0, distance - newThreadPullThreshold) * 0.15)
    }

    var body: some View {
        NavigationStack {
            inbox
                #if os(iOS)
                .navigationTitle("Inbox")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar(.hidden, for: .navigationBar)
                #endif
                .navigationDestination(isPresented: $showScheduledJobs) {
                    ScheduledJobsView(model: model) {
                        showScheduledJobs = false
                        model.openThread(); showThread = true
                    }
                    #if os(iOS)
                    .toolbar(.visible, for: .navigationBar)
                    .navigationBarTitleDisplayMode(.inline)
                    #endif
                }
                .navigationDestination(isPresented: $showSettings) {
                    settings
                        #if os(iOS)
                        .toolbar(.visible, for: .navigationBar)
                        .navigationBarTitleDisplayMode(.inline)
                        #endif
                }
        }
        .foregroundStyle(Ink.text)
        .tint(Ink.accent)
        .sheet(isPresented: $showScreens) {
            NavigationStack {
                if let service = model.remoteService {
                    RemoteDashboard(service: service).id(ObjectIdentifier(service))
                        .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showScreens = false } } }
                }
            }
        }
        .sheet(isPresented: $showThread, onDismiss: { model.closeThread() }) { ConversationView(model: model).tint(Ink.accent) }
        .sheet(isPresented: Binding(get: { model.showContext && !showThread }, set: { model.showContext = $0 })) { ContextInboxView(model: model).tint(Ink.accent) }
        .onChange(of: model.focused?.activeTurns ?? []) { _, turns in
            if !turns.contains(model.selectedTurn) { model.selectedTurn = turns.first ?? "" }
        }
        .onChange(of: model.focusedConversationIdentity) { _, _ in drag = 0; upwardDrag = 0; cardDrag = nil; composerFocused = false }
        .onChange(of: model.connected) { _, connected in
            if !connected { showScreens = false; showScheduledJobs = false; showSettings = false; showAgents = false; showThread = false }
        }
    }

    private var inbox: some View {
        ZStack {
            Ink.background.ignoresSafeArea()
            if model.restoringAccount {
                accountRestoration
            } else if model.connected {
                sidebarLayout
            } else { ConnectView(model: model) }
        }
    }
    private var sidebarLayout: some View {
        GeometryReader { geometry in
            let width = max(1, min(360, geometry.size.width * 0.78))
            let offset = min(width, max(0, (showAgents ? width : 0) + sidebarDrag))
            let progress = offset / width
            ZStack(alignment: .topLeading) {
                if showAgents || sidebarDrag > 0 {
                    agentList
                        .frame(width: width, height: geometry.size.height)
                        .background(Ink.background.ignoresSafeArea())
                        .offset(x: reduceMotion ? 0 : -24 * (1 - progress))
                        .contentShape(Rectangle())
                        .simultaneousGesture(sidebarGesture(width: width, closing: true))
                        .accessibilityHidden(!showAgents)
                        .allowsHitTesting(showAgents)
                        .transition(.opacity)
                }
                inboxContent
                    .frame(width: geometry.size.width, height: geometry.size.height)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("inbox-panel")
                    .accessibilityHidden(showThread || showAgents)
                    .padding(.top, geometry.safeAreaInsets.top)
                    .padding(.bottom, geometry.safeAreaInsets.bottom)
                    .background(Ink.background)
                    .clipShape(RoundedRectangle(cornerRadius: 38 * progress, style: .continuous))
                    .shadow(color: .black.opacity(0.16 * progress), radius: 24, x: -8)
                    .overlay {
                        Color.clear.contentShape(Rectangle())
                            .onTapGesture { setSidebar(false) }
                            .gesture(sidebarGesture(width: width, closing: true))
                            .allowsHitTesting(showAgents)
                            .accessibilityHidden(true)
                    }
                    .offset(x: offset, y: -geometry.safeAreaInsets.top)
            }
            .frame(width: geometry.size.width, height: geometry.size.height, alignment: .topLeading)
            .overlay(alignment: .leading) {
                Color.clear.frame(width: 16).contentShape(Rectangle())
                    .gesture(sidebarGesture(width: width, closing: false))
                    .allowsHitTesting(!showAgents)
                    .accessibilityHidden(true)
            }
            .coordinateSpace(name: "inbox-sidebar-space")
            .transaction { if reduceMotion { $0.animation = nil } }
        }
    }
    private var inboxContent: some View {
        VStack(spacing: 10) {
            Group {
                if let card = model.focused { deck(card) }
                else { emptyState.frame(maxHeight: .infinity) }
            }
            .overlay(alignment: .bottomLeading) {
                if model.canGoBack {
                    Button(action: undoSwipe) {
                        Image(systemName: "arrow.uturn.backward")
                            .font(.system(size: 17, weight: .medium))
                            .frame(width: 44, height: 44)
                            .background(.regularMaterial, in: Circle())
                            .shadow(color: .black.opacity(0.09), radius: 12, y: 4)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Undo swipe")
                    .accessibilityHint("Restore the previous agent and its review state.")
                    .accessibilityIdentifier("undo-swipe")
                    .padding(.leading, 18)
                    .padding(.bottom, model.focused == nil ? 10 : composerFocused ? 22 : 34)
                    .transition(.opacity)
                }
            }
            if let error = model.error {
                HStack(alignment: .top) {
                    Text(error).font(.caption).foregroundStyle(Ink.amber)
                    Button { model.error = nil } label: { Image(systemName: "xmark") }.accessibilityLabel("Dismiss error")
                }
                .padding(12).background(Ink.card, in: RoundedRectangle(cornerRadius: 12))
            } else if let notice = model.notice, !composerFocused {
                Text(notice).font(.caption).foregroundStyle(Ink.muted).accessibilityIdentifier("notice")
            }
        }
        .padding(.horizontal, 8).padding(.top, 4).padding(.bottom, 4)
        .frame(maxWidth: 620)
        .overlay(alignment: .top) {
            if !composerFocused {
                // Refresh hidden navigation accessibility nodes without resetting the deck or composer.
                header.id(showAgents).padding(.horizontal, 16).padding(.top, 8)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if model.focused != nil, !showThread {
                AgentComposerView(model: model, focused: $composerFocused, onVoiceChat: {
                    composerFocused = false; model.openThread(); showThread = true
                }).frame(maxWidth: 620)
            }
        }
    }
    private func sidebarGesture(width: CGFloat, closing: Bool) -> some Gesture {
        DragGesture(minimumDistance: 12, coordinateSpace: .named("inbox-sidebar-space"))
            .updating($sidebarDrag) { value, drag, _ in
                guard abs(value.translation.width) > abs(value.translation.height) * 1.3 else { return }
                drag = closing ? min(0, value.translation.width) : max(0, value.translation.width)
            }
            .onEnded { value in
                guard abs(value.translation.width) > abs(value.translation.height) * 1.3 else { return }
                let projected = value.predictedEndTranslation.width
                let travel = value.translation.width
                if closing {
                    setSidebar(!(travel < -width * 0.3 || projected < -width * 0.3))
                } else {
                    setSidebar(travel > width * 0.3 || projected > width * 0.3)
                }
            }
    }
    private var accountRestoration: some View {
        VStack(spacing: 20) {
            Text("Nanocodex").font(.system(size: 20, weight: .medium))
                .frame(maxWidth: .infinity, alignment: .leading)
            Spacer()
            Image(systemName: "tray").font(.system(size: 38)).foregroundStyle(Ink.muted)
            if let error = model.restorationError {
                Text("Couldn’t open your inbox").font(.title3.weight(.semibold))
                Text(error).font(.subheadline).foregroundStyle(Ink.muted).multilineTextAlignment(.center)
                Button("Retry") { Task { await model.restoreSavedAccount() } }
                    .buttonStyle(.borderedProminent).disabled(model.signingIn)
                    .accessibilityIdentifier("retry-account-restoration")
            } else {
                ProgressView()
                Text("Opening your inbox…").font(.subheadline).foregroundStyle(Ink.muted)
            }
            Spacer()
        }
        .padding(24).frame(maxWidth: 620)
        .accessibilityIdentifier("account-restoration")
    }
    private var header: some View {
        HStack(spacing: 8) {
            Button { setSidebar(true) } label: {
                Image(systemName: "sidebar.left").font(.system(size: 20)).frame(width: 44, height: 44)
                    .background(.regularMaterial, in: Circle())
            }.accessibilityLabel("Browse agents").accessibilityIdentifier("inbox-sidebar-toggle")
            ScrollView(.horizontal) { filters }
                .scrollIndicators(.hidden)
                .clipShape(Capsule())
            Button { showScreens = true } label: {
                Image(systemName: "display").frame(width: 44, height: 44)
                    .background(.regularMaterial, in: Circle())
            }.accessibilityLabel("Remote screens").disabled(model.remoteService == nil)
            ConnectionStatusView(status: model.connection, retry: { model.retryConnection() }, signIn: { showSettings = true })
        }.foregroundStyle(Ink.text).buttonStyle(.plain).frame(height: 44)
            .shadow(color: .black.opacity(0.09), radius: 12, y: 4)
    }

    private var filters: some View {
        HStack(spacing: 6) {
            ForEach(InboxModel.Filter.allCases, id: \.self) { filter in
                Button { model.filter = filter } label: {
                    HStack(spacing: 5) {
                        Text(filter.rawValue)
                        if filter != .all {
                            Text(String(filter == .inbox ? model.attentionCount : model.runningCount))
                                .monospacedDigit().opacity(0.7)
                        }
                    }.font(.system(size: 14, weight: .medium))
                        .padding(.horizontal, 11).padding(.vertical, 8)
                        .frame(minHeight: 44)
                        .foregroundStyle(model.filter == filter ? Ink.background : Ink.muted)
                        .background {
                            if model.filter == filter { Capsule().fill(Ink.accent) }
                            else { Capsule().fill(.regularMaterial) }
                        }
                }.buttonStyle(.plain).accessibilityIdentifier("filter-" + filter.rawValue)
            }
        }
    }
    private func deck(_ card: AgentCard) -> some View {
        GeometryReader { _ in
            ZStack {
                RoundedRectangle(cornerRadius: 28).fill(Ink.surface.opacity(0.6)).padding(.horizontal, 18).offset(y: 16)
                RoundedRectangle(cornerRadius: 28).fill(Ink.surface).padding(.horizontal, 9).offset(y: 8)
                cardFace(card).id(card.id).transition(.identity)
                    .overlay(alignment: drag > 0 ? .topLeading : .topTrailing) {
                        if abs(drag) > 24 {
                            Text(drag > 0 ? "SEEN" : "LATER")
                                .font(.system(.title2).weight(.semibold))
                                .padding(12).background(Ink.background, in: RoundedRectangle(cornerRadius: 10))
                                .foregroundStyle(drag > 0 ? Ink.accent : Ink.amber)
                                .overlay(RoundedRectangle(cornerRadius: 10).stroke(drag > 0 ? Ink.accent : Ink.amber, lineWidth: 2))
                                .rotationEffect(.degrees(drag > 0 ? -9 : 9)).padding(24)
                        }
                    }
                    .rotationEffect(.degrees(reduceMotion ? 0 : Double(drag / 28)))
                    .offset(x: drag)
                    .simultaneousGesture(DragGesture(minimumDistance: 18, coordinateSpace: .named("agent-card-deck"))
                        .onChanged { value in
                            if cardDrag == nil {
                                if abs(value.translation.width) > abs(value.translation.height) * 1.3 {
                                    cardDrag = .horizontal
                                } else if !composerFocused, value.translation.height < 0,
                                          abs(value.translation.height) > abs(value.translation.width) * 1.3,
                                          !previewGeometry.isScrollable || !previewGeometry.viewport.contains(value.startLocation)
                                            || previewGeometry.gestureRegion.contains(value.startLocation) {
                                    cardDrag = .newAgent
                                } else {
                                    cardDrag = .scroll
                                }
                            }
                            if cardDrag == .horizontal { drag = value.translation.width }
                            if cardDrag == .newAgent { upwardDrag = min(0, value.translation.height) }
                        }
                        .onEnded { value in
                            if cardDrag == .horizontal && (abs(drag) > 65 || (abs(drag) > 24 && abs(value.predictedEndTranslation.width) > 130)) { advance(reviewed: drag > 0) }
                            // Only the distance at release commits; a quick flick or pulling back cancels.
                            if cardDrag == .newAgent && -value.translation.height >= newThreadPullThreshold { createAgent() }
                            cardDrag = nil
                            withAnimation(reduceMotion ? nil : .snappy(duration: 0.16)) { drag = 0 }
                            withAnimation(reduceMotion ? nil : .spring(response: 0.35, dampingFraction: 0.75)) { upwardDrag = 0 }
                        })
            }
            .offset(y: newThreadPullOffset)
            .background(alignment: .bottom) {
                if upwardDrag < -18 { newThreadPullIndicator.transition(.opacity) }
            }
            .coordinateSpace(name: "agent-card-deck")
            .onPreferenceChange(CardPreviewGeometryKey.self) { previewGeometry = $0 }
            .onChange(of: newThreadPullReady) { _, ready in
                if ready { UIImpactFeedbackGenerator(style: .light).impactOccurred() }
            }
        }.frame(minHeight: composerFocused ? 0 : 220, maxHeight: .infinity).padding(.bottom, composerFocused ? 12 : 24)
            .clipped()
    }
    private var newThreadPullIndicator: some View {
        VStack(spacing: 6) {
            ZStack {
                Circle().stroke(Ink.accent.opacity(0.12), lineWidth: 2)
                Circle().trim(from: 0, to: newThreadPullProgress)
                    .stroke(Ink.accent, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                Image(systemName: "plus").font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(newThreadPullReady ? Ink.background : Ink.muted)
                    .frame(width: 22, height: 22)
                    .background(newThreadPullReady ? Ink.accent : Color.clear, in: Circle())
            }.frame(width: 28, height: 28)
            Text(newThreadPullReady ? "Release for new thread" : "Pull up for new thread")
                .font(.caption).foregroundStyle(newThreadPullReady ? Ink.accent : Ink.muted)
        }
        .padding(.bottom, 2)
        .opacity(min(1, max(0, (-upwardDrag - 18) / 40)))
        .allowsHitTesting(false)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("new-thread-pull-indicator")
    }
    private func cardFace(_ card: AgentCard) -> some View {
        let rows = model.rows.isEmpty ? card.previewRows : model.rows
        let latestUser = rows.lastIndex(where: { $0.role == "You" })
        let pending = model.focusedPending.last
        let input = pending?.input ?? latestUser.map { rows[$0].text }
        let images: [AttachmentImageSource?] = pending != nil
            ? (pending?.attachments ?? []).filter { !$0.isVideo }.map { model.attachmentURL($0).map(AttachmentImageSource.file) }
            : (latestUser.map { rows[$0].images ?? [] } ?? []).filter { $0.hasPrefix("data:image/") }.map { .inline($0) }
        let pendingVideos = (pending?.attachments ?? []).filter(\.isVideo)
        let videos = pending == nil ? (latestUser.map { rows[$0].videos ?? [] } ?? []) : []
        // Parse complete blocks: taking the tail can discard an opening fence,
        // heading, or list marker and turn the rest of the reply into plain text.
        let reply = rows.dropFirst(latestUser.map { $0 + 1 } ?? 0)
            .last(where: { $0.role == "Agent" && $0.phase != "commentary" && $0.agentID == nil && !$0.text.isEmpty })?.text
        let waiting = pending?.phase == .failed ? "Message not confirmed" : pending?.phase == .submitting ? "Sending…" : pending != nil ? "Queued" : card.isRunning ? "Working…" : ""
        let preview = pending != nil ? waiting : reply ?? (input == nil ? card.preview : waiting)
        return ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(card.title)
                        .font(.system(size: 17, weight: .semibold))
                        .fixedSize(horizontal: false, vertical: true).accessibilityIdentifier("agent-title")
                    if let input {
                        HStack {
                            Spacer(minLength: 44)
                            VStack(alignment: .leading, spacing: 8) {
                                if !input.isEmpty {
                                    Text(ContextPrompt.separate(input)?.request ?? input).font(.system(size: 17)).fixedSize(horizontal: false, vertical: true)
                                        .accessibilityIdentifier("card-user-message")
                                }
                                if let image = images.first {
                                    HStack(spacing: 8) {
                                        AttachmentImageView(source: image, contentMode: .fit).frame(width: 96, height: 72)
                                            .accessibilityIdentifier("card-image")
                                        if images.count > 1 { Text("\(images.count) images").font(.caption).foregroundStyle(Ink.muted) }
                                    }
                                }
                                ForEach(pendingVideos) { attachment in
                                    AttachmentMovieThumbnail(attachment: attachment, poster: model.attachmentURL(attachment), movie: model.attachmentMovieURL(attachment))
                                        .frame(width: 200, height: 130)
                                }
                                ForEach(videos) { VideoAttachmentView(video: $0, model: model, agentID: card.id) }
                            }.padding(14).background(Ink.surface, in: RoundedRectangle(cornerRadius: 20))
                        }
                    }
                    Group {
                        if !waiting.isEmpty, preview == waiting, pending?.phase != .failed {
                            PulsingText(text: preview)
                        } else {
                            ChatMarkdown(text: preview)
                        }
                    }
                    .font(.system(size: 17)).foregroundStyle(Ink.text)
                    .lineSpacing(3).fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("agent-preview")
                    if pending == nil {
                        InboxGeneratedOutputView(rows: Array(rows.dropFirst(latestUser.map { $0 + 1 } ?? 0))).equatable()
                    }
                    if let error = card.error {
                        HStack(alignment: .top, spacing: 8) {
                            Text(error).font(.caption).foregroundStyle(Ink.muted)
                            Spacer(minLength: 0)
                            Button("Retry") { Task { await model.refresh() } }
                                .font(.caption.weight(.medium)).disabled(model.refreshing)
                                .accessibilityLabel("Retry agent update")
                        }.accessibilityIdentifier("card-update-error")
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 18)
                    .padding(.top, composerFocused ? 18 : 64)
                    .padding(.bottom, 62)
                    .background {
                        GeometryReader { geometry in
                            Color.clear.preference(key: CardPreviewGeometryKey.self, value: CardPreviewGeometry(contentHeight: geometry.size.height))
                        }
                    }
            }
            .modifier(CardScrollAnchors(isComposing: composerFocused))
            .scrollDismissesKeyboard(.interactively)
            .scrollBounceBehavior(.always, axes: .vertical)
            .background {
                GeometryReader { geometry in
                    let frame = geometry.frame(in: .named("agent-card-deck"))
                    Color.clear.preference(key: CardPreviewGeometryKey.self, value: CardPreviewGeometry(
                        viewport: frame,
                        gestureRegion: CGRect(x: frame.minX, y: frame.maxY - 54, width: frame.width, height: 54)
                    ))
                }
            }
            .scrollIndicators(.hidden).accessibilityIdentifier("card-content")
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Ink.card, in: RoundedRectangle(cornerRadius: 28))
        .clipShape(RoundedRectangle(cornerRadius: 28))
        .overlay(RoundedRectangle(cornerRadius: 28).stroke(Ink.border, lineWidth: 0.75))
        .accessibilityElement(children: .contain)
        .onTapGesture { composerFocused = false; model.openThread(); showThread = true }
        .accessibilityAction(named: "Open thread") { composerFocused = false; model.openThread(); showThread = true }
        .accessibilityAction(named: "Previous agent") { model.back() }
        .contextMenu { Button("Previous agent") { model.back() }.disabled(!model.canGoBack) }
        .accessibilityAction(named: "Next agent, revisit later") { advance(reviewed: false) }
        .accessibilityAction(named: "Mark update seen") { advance(reviewed: true) }
        .accessibilityAction(named: "New agent") { createAgent() }
        .accessibilityIdentifier("agent-card")
    }
    private var emptyState: some View {
        VStack(spacing: 18) {
            Image(systemName: "tray").font(.system(size: 46, weight: .ultraLight)).foregroundStyle(Ink.accent)
            Text(model.filter == .running ? "Nothing running" : model.filter == .inbox ? "Nothing in your inbox" : "You're caught up").font(.title2.weight(.medium))
            Text("Start something new, or check all your agents.").font(.subheadline).foregroundStyle(Ink.muted).multilineTextAlignment(.center)
            Button("New agent") { createAgent() }.buttonStyle(.borderedProminent).foregroundStyle(Ink.background)
            Button("View all agents") { model.filter = .all }
            Button("Context from other apps") { model.showContext = true }
        }
    }
    private var agentList: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Nanocodex").font(.system(size: 23, weight: .semibold))
                Spacer()
            }.frame(height: 44).padding(.leading, 20).padding(.trailing, 8).padding(.top, 4)
            VStack(spacing: 2) {
                Button { setSidebar(false); showScheduledJobs = true } label: {
                    Label("Scheduled jobs", systemImage: "clock")
                        .frame(maxWidth: .infinity, alignment: .leading).frame(minHeight: 44)
                }.accessibilityIdentifier("inbox-scheduled-jobs")
            }.font(.system(size: 16, weight: .medium)).padding(.horizontal, 20).padding(.vertical, 8)
            List {
                Section("Agents") {
                    ForEach(model.cards) { card in
                        Button { model.select(card.id); setSidebar(false) } label: {
                            VStack(alignment: .leading, spacing: 6) {
                                Text(card.title).foregroundStyle(Ink.text)
                                Text(card.status).font(.caption).foregroundStyle(card.isRunning ? Ink.amber : Ink.muted)
                            }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 6)
                        }.listRowBackground(model.focused?.id == card.id ? Ink.surface : Color.clear)
                    }
                }
            }.listStyle(.plain).scrollContentBackground(.hidden)
                .contentMargins(.bottom, 88, for: .scrollContent)
                .accessibilityIdentifier("sidebar-agents")
        }
        .overlay(alignment: .bottom) { sidebarActions }
        .buttonStyle(.plain)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("inbox-sidebar")
        .accessibilityAction(.escape) { setSidebar(false) }
    }
    private var sidebarActions: some View {
        HStack(spacing: 12) {
            Button { createAgent() } label: {
                Label("New agent", systemImage: "square.and.pencil")
                    .font(.system(size: 17, weight: .semibold))
                    .padding(.horizontal, 20).frame(height: 52)
                    .foregroundStyle(Ink.background)
                    .background(Ink.accent, in: Capsule())
            }.keyboardShortcut("n", modifiers: .command).accessibilityIdentifier("sidebar-new-agent")
            Spacer(minLength: 0)
            Button { setSidebar(false); showSettings = true } label: {
                Image(systemName: "gearshape").font(.system(size: 22, weight: .medium))
                    .frame(width: 52, height: 52)
                    .background(.regularMaterial, in: Circle())
            }.accessibilityLabel("Account settings").accessibilityIdentifier("sidebar-settings")
        }
        .padding(.horizontal, 20).padding(.bottom, 14)
        .shadow(color: .black.opacity(0.14), radius: 20, y: 5)
    }
    private var settings: some View {
        Form {
            Section("Account") {
                Text(model.isDemo ? "Demo · sample agents" : model.connection == "Sign in again" ? "Sign in again to reconnect your account." : "Nanocodex account connected")
                Text("Agents keep running when you swipe away or close the app.").foregroundStyle(.secondary)
                Button(model.isDemo ? "Connect account" : model.connection == "Sign in again" ? "Sign in again" : "Disconnect account") {
                    do { try model.disconnect(); showSettings = false } catch { model.error = error.localizedDescription }
                }
            }
            if !model.isDemo {
                Section("This device") {
                    Toggle("Make this device available as a Hand", isOn: $model.deviceHandEnabled)
                        .accessibilityIdentifier("device-hand-enabled")
                    Label(model.deviceHandStatus, systemImage: "hand.raised")
                        .accessibilityIdentifier("device-hand-status")
                    Text("Connects automatically to your account unless disabled. Agents can work with workspace files and query captured messages when capture is enabled.").font(.caption).foregroundStyle(.secondary)
                    Text("Tasks you start can keep this Hand connected in the background on iOS 26 or later. iOS shows progress and lets you stop the task. When idle, this phone connects only during brief background windows or while Nanocodex is open. Force-quitting ends background work.").font(.caption).foregroundStyle(.secondary)
                    if let error = model.handBackgroundError { Text(error).font(.caption).foregroundStyle(.secondary) }
                }
            }
            Section("Controls") {
                Text("Swipe left: revisit later\nSwipe right: mark this update seen\nSwipe up: create a new agent\nTap card: read the conversation\nSend: queue a message\nSteer now: stop the current turn so the queued message can start")
                Text("Long previews scroll vertically. Pull up from the bottom edge of a card to start a new agent. Release when the indicator fills, or pull back to cancel.").font(.caption)
                Text("Undo swipe brings the previous agent back and restores its review state. You can undo several swipes in order. ⌘Return sends your message.").font(.caption)
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Settings")
        .accessibilityIdentifier("inbox-settings")
    }
    private func advance(reviewed: Bool) {
        composerFocused = false
        #if os(iOS)
        UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        #endif
        withAnimation(reduceMotion ? nil : .snappy(duration: 0.18)) { model.advance(reviewed: reviewed); drag = 0 }
    }
    private func undoSwipe() {
        composerFocused = false
        UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        withAnimation(reduceMotion ? nil : .snappy(duration: 0.18)) { model.back(); drag = 0 }
    }
    private func setSidebar(_ visible: Bool) {
        composerFocused = false
        withAnimation(reduceMotion ? nil : .snappy(duration: 0.28)) { showAgents = visible }
    }
    private func createAgent() {
        composerFocused = false
        #if os(iOS)
        UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        #endif
        setSidebar(false)
        model.newAgent()
    }
    private enum CardDrag { case horizontal, newAgent, scroll }
    private struct CardPreviewGeometry: Equatable {
        var viewport: CGRect = .zero
        var gestureRegion: CGRect = .zero
        var contentHeight: CGFloat = 0
        var isScrollable: Bool { contentHeight > viewport.height + 1 }
    }
    private struct CardPreviewGeometryKey: PreferenceKey {
        static let defaultValue = CardPreviewGeometry()
        static func reduce(value: inout CardPreviewGeometry, nextValue: () -> CardPreviewGeometry) {
            let next = nextValue()
            if !next.viewport.isEmpty { value.viewport = next.viewport }
            if !next.gestureRegion.isEmpty { value.gestureRegion = next.gestureRegion }
            value.contentHeight = max(value.contentHeight, next.contentHeight)
        }
    }
}

private struct ConnectionStatusView: View {
    let status: String
    let retry: () -> Void
    let signIn: () -> Void
    @State private var showDelay = false

    private var isWaiting: Bool { status == "Connecting" || status == "Reconnecting" }

    var body: some View {
        HStack(spacing: 0) {
            if status == "Demo" {
                Text("Demo").accessibilityIdentifier("connection")
            } else if status == "Sign in again" {
                Button("Sign in again", action: signIn).accessibilityIdentifier("connection-sign-in")
            } else if isWaiting, showDelay {
                Button(action: retry) {
                    VStack(alignment: .trailing, spacing: 2) {
                        Text("Updates delayed")
                        Text("Retry").fontWeight(.medium)
                    }
                }
                .frame(minHeight: 44)
                .accessibilityLabel("Updates delayed. Retry connection")
                .accessibilityIdentifier("connection-retry")
            }
        }
        .font(.system(size: 12)).foregroundStyle(Ink.muted).lineLimit(1)
        .task(id: isWaiting) {
            showDelay = false
            guard isWaiting else { return }
            // Opening another agent normally involves a brief stream handshake.
            // Only a sustained delay needs attention in the inbox header.
            do { try await Task.sleep(for: .seconds(5)) } catch { return }
            showDelay = true
        }
    }
}

private struct AgentComposerView: View {
    @ObservedObject var model: InboxModel
    @FocusState.Binding var focused: Bool
    var onVoiceChat: @MainActor () -> Void = {}
    @State private var showPhotos = false
    @State private var showFiles = false
    @State private var selectedPhotos: [PhotosPickerItem] = []
    @State private var photoTarget: InboxModel.AttachmentTarget?
    @State private var fileTarget: InboxModel.AttachmentTarget?
    @State private var pickerError: String?
    #if os(iOS)
    @State private var showCamera = false
    @State private var cameraTarget: InboxModel.AttachmentTarget?
    @State private var cameraPermissionDenied = false
    #endif

    private var visiblePending: [PendingMessage] {
        model.focusedPending.filter { !$0.predecessor.isEmpty || $0.phase == .failed }
    }
    private var sendShowsStop: Bool {
        model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && model.focusedAttachments.isEmpty && !model.preparingAttachments
            && !model.stopTarget.isEmpty
    }
    private var stopRequest: PendingTurnCancellation? {
        model.focused.flatMap { model.cancellation(agentID: $0.id, turnID: model.stopTarget) }
    }
    private var pendingHeight: CGFloat {
        let messages = visiblePending
        if messages.count > 1 { return messages.contains { $0.attachments?.isEmpty == false } ? 136 : 104 }
        return messages.first?.error == nil && (messages.first?.attachments?.isEmpty ?? true) ? 48 : 82
    }

    var body: some View {
        VStack(spacing: 0) {
            if let error = model.creationError {
                HStack {
                    Text(error).font(.caption).foregroundStyle(Ink.muted)
                    Spacer()
                    Button("Retry") { model.retryCreation() }.accessibilityIdentifier("retry-creation")
                }.padding(12)
            }
            if let agentID = model.focused?.id, !model.contextForAgent(agentID).isEmpty {
                Button { focused = false; model.showContext = true } label: {
                    Label("Context for your next message (\(model.contextForAgent(agentID).count))", systemImage: "tray.full")
                        .font(.caption).padding(.vertical, 10)
                }.accessibilityIdentifier("composer-context")
            }
            if model.controllableTurns.count > 1 {
                Picker("Active turn", selection: $model.selectedTurn) {
                    ForEach(model.controllableTurns, id: \.self) { id in Text("Turn \(model.controllableTurns.firstIndex(of: id).map { $0 + 1 } ?? 1)").tag(id) }
                }.pickerStyle(.menu)
            }
            if !visiblePending.isEmpty {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(visiblePending) { message in
                            HStack(alignment: .center, spacing: 8) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(message.phase == .submitting ? "Sending…" : message.phase == .starting ? "Stopping current turn…" : message.phase == .cancelling ? "Cancelling…" : message.phase == .failed ? "Not confirmed" : "Queued")
                                        .font(.system(size: 11)).foregroundStyle(Ink.muted)
                                    Text(ContextPrompt.separate(message.input)?.request ?? message.input).font(.system(size: 13)).lineLimit(1)
                                        .accessibilityIdentifier("pending-message")
                                    if let attachments = message.attachments, !attachments.isEmpty {
                                        Label(attachments.map(\.name).joined(separator: ", "), systemImage: "photo")
                                            .font(.caption2).lineLimit(1).accessibilityIdentifier("pending-attachments")
                                    }
                                    if let error = message.error { Text(error).font(.caption2).foregroundStyle(Ink.muted) }
                                }.frame(maxWidth: .infinity, alignment: .leading)
                                if message.phase == .failed {
                                    Button { model.retryPending(message.id) } label: { Text("Retry").frame(minHeight: 44) }.accessibilityIdentifier("retry-pending")
                                        .disabled(model.busy.contains(message.agentID))
                                } else if model.steeringTarget(message) != nil {
                                    Button { model.steerNow(message.id) } label: { Text("Steer now").frame(minHeight: 44) }.accessibilityIdentifier("steer-now")
                                }
                                Button { model.cancelPending(message.id) } label: {
                                    Image(systemName: "xmark").frame(width: 44, height: 44).contentShape(Rectangle())
                                }.accessibilityLabel("Cancel queued message")
                                    .disabled(model.cancellation(agentID: message.agentID, turnID: message.id).map { $0.error == nil } ?? false)
                            }.font(.system(size: 13, weight: .medium)).buttonStyle(.plain)
                                .padding(.leading, 16)
                        }
                    }
                }.frame(maxHeight: pendingHeight)
                    .padding(.top, 4)
                    .accessibilityIdentifier("pending-messages")
                Rectangle().fill(Ink.border).frame(height: 0.5).padding(.horizontal, 16)
            }
            if !model.focusedAttachments.isEmpty {
                ScrollView(.horizontal) {
                    HStack(spacing: 10) {
                        ForEach(model.focusedAttachments) { attachment in
                            VStack(alignment: .leading, spacing: 4) {
                                Group {
                                    if attachment.isVideo {
                                        AttachmentMovieThumbnail(attachment: attachment, poster: model.attachmentURL(attachment), movie: model.attachmentMovieURL(attachment))
                                    } else {
                                        AttachmentImageView(source: model.attachmentURL(attachment).map(AttachmentImageSource.file))
                                    }
                                }
                                    .frame(width: 88, height: 76)
                                    .overlay(alignment: .topTrailing) {
                                        Button { model.removeAttachment(attachment.id) } label: {
                                            Image(systemName: "xmark.circle.fill").symbolRenderingMode(.palette)
                                                .foregroundStyle(Ink.text, Ink.background).frame(width: 44, height: 44)
                                        }.buttonStyle(.plain).accessibilityLabel("Remove " + attachment.name)
                                            .accessibilityIdentifier("remove-attachment-" + attachment.id)
                                    }
                                Text(attachment.name).font(.caption2).lineLimit(1)
                            }.frame(width: 88).accessibilityElement(children: .contain)
                                .accessibilityIdentifier("attachment-" + attachment.id)
                        }
                    }.padding(.horizontal, 16).padding(.top, 10).padding(.bottom, 6)
                }.scrollIndicators(.hidden).accessibilityIdentifier("composer-attachments")
                if model.focusedAttachments.contains(where: \.isVideo) {
                    Text("Original video").font(.caption).foregroundStyle(Ink.muted)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.bottom, 6)
                        .accessibilityIdentifier("video-analysis-description")
                }
            }
            if model.preparingAttachments {
                ProgressView("Preparing attachments…").font(.caption).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16).padding(.vertical, 8).accessibilityIdentifier("preparing-attachments")
            }
            if let error = pickerError ?? model.attachmentError {
                Text(error).font(.caption).foregroundStyle(Ink.muted).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16).padding(.vertical, 8).accessibilityIdentifier("attachment-error")
            }
            HStack(alignment: .bottom, spacing: 8) {
                Menu {
                    #if os(iOS)
                    Button { Task { await openCamera() } } label: { Label("Camera", systemImage: "camera") }
                        .disabled(model.preparingAttachments).accessibilityIdentifier("choose-camera")
                    #endif
                    Button {
                        guard let target = model.captureAttachmentTarget() else { return }
                        photoTarget = target; selectedPhotos = []; pickerError = nil; focused = false; showPhotos = true
                    } label: { Label("Photos & Videos", systemImage: "photo.on.rectangle") }
                        .disabled(model.preparingAttachments).accessibilityIdentifier("choose-photos")
                    Button {
                        guard let target = model.captureAttachmentTarget() else { return }
                        fileTarget = target; pickerError = nil; focused = false; showFiles = true
                    } label: { Label("Files", systemImage: "folder") }
                        .disabled(model.preparingAttachments).accessibilityIdentifier("choose-files")
                    Button { focused = false; model.showContext = true } label: {
                        Label("Context from other apps", systemImage: "tray.full")
                    }
                } label: {
                    Image(systemName: "plus").frame(width: 44, height: 44).contentShape(Rectangle())
                }.menuStyle(.borderlessButton).accessibilityLabel("Add attachments").accessibilityIdentifier("add-attachments")
                TextField("Ask Nanocodex", text: $model.draft, axis: .vertical)
                    .lineLimit(1...4).textFieldStyle(.plain).font(.body).focused($focused)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.vertical, 8).accessibilityIdentifier("composer")
                if let agentID = model.focused?.id {
                    NanocodexVoiceControl(session: model.voice, onReturnToChat: onVoiceChat) {
                        focused = false
                        return try await model.voiceConfiguration(agentID: agentID)
                    }.id(model.focusedConversationIdentity)
                }
                Button {
                    if sendShowsStop, let card = model.focused {
                        model.stop(agentID: card.id, turnID: model.stopTarget)
                    } else if model.send() {
                        #if os(iOS)
                        focused = false
                        #endif
                    }
                } label: {
                    Group {
                        if sendShowsStop, let stopRequest, stopRequest.error == nil {
                            ProgressView().tint(Ink.background)
                        } else {
                            Image(systemName: sendShowsStop ? "stop.fill" : model.busy.contains(model.focused?.id ?? "") ? "ellipsis" : "arrow.up")
                        }
                    }.font(.system(size: 18, weight: .semibold)).frame(width: 44, height: 44)
                        .background(Ink.accent.opacity(sendShowsStop || model.canSend ? 1 : 0.22), in: Circle()).foregroundStyle(Ink.background)
                }.buttonStyle(.plain)
                    .disabled(sendShowsStop ? stopRequest.map { $0.error == nil } ?? false : !model.canSend)
                    .accessibilityLabel(sendShowsStop ? stopRequest.map { $0.error == nil ? "Stopping turn" : "Retry stop" } ?? "Stop turn" : model.focused?.isRunning == true ? "Queue message" : "Send message")
                    .accessibilityIdentifier("send")
                    .keyboardShortcut(sendShowsStop ? nil : KeyboardShortcut(.return, modifiers: .command))
            }.padding(.horizontal, 8).padding(.bottom, 8).padding(.top, visiblePending.isEmpty ? 8 : 0)
                .padding(.leading, 6).accessibilityElement(children: .contain).accessibilityIdentifier("composer-input")

        }.background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 28))
            .overlay(RoundedRectangle(cornerRadius: 28).strokeBorder(Color.primary.opacity(focused ? 0.18 : 0.1)))
            .shadow(color: .black.opacity(0.035), radius: 8, y: 2)
            .padding(.horizontal, 12).padding(.top, 6).padding(.bottom, 8).background(Ink.background)
            .onChange(of: model.focusedConversationIdentity) { _, _ in focused = false }
            #if os(iOS)
            .fullScreenCover(isPresented: $showCamera, onDismiss: { cameraTarget = nil }) {
                CameraPicker { image in
                    if let image, let target = cameraTarget { model.importCameraPhoto(image, target: target) }
                    showCamera = false
                }.ignoresSafeArea()
            }
            .alert("Allow camera access", isPresented: $cameraPermissionDenied) {
                Button("Open Settings") { UIApplication.shared.open(URL(string: UIApplication.openSettingsURLString)!) }
                Button("Cancel", role: .cancel) {}
            } message: { Text("Enable Camera in Settings to take a photo for your message.") }
            #endif
            .photosPicker(isPresented: $showPhotos, selection: $selectedPhotos, maxSelectionCount: 4, matching: .any(of: [.images, .videos]))
            .onChange(of: selectedPhotos) { _, items in
                guard !items.isEmpty, let target = photoTarget else { return }
                model.importAttachmentPhotos(items, target: target)
                selectedPhotos = []; photoTarget = nil
            }
            .fileImporter(isPresented: $showFiles, allowedContentTypes: [.image, .movie], allowsMultipleSelection: true) { result in
                guard let target = fileTarget else { return }
                fileTarget = nil
                switch result {
                case .success(let urls): model.importAttachmentFiles(urls, target: target)
                case .failure(let error): pickerError = error.localizedDescription
                }
            }
    }

    #if os(iOS)
    @MainActor private func openCamera() async {
        guard let target = model.captureAttachmentTarget() else { return }
        pickerError = nil
        guard UIImagePickerController.isSourceTypeAvailable(.camera) else {
            pickerError = "Camera is unavailable on this device. Choose Photos or Files instead."
            return
        }
        let status = AVCaptureDevice.authorizationStatus(for: .video)
        let allowed: Bool
        if status == .notDetermined { allowed = await AVCaptureDevice.requestAccess(for: .video) }
        else { allowed = status == .authorized }
        guard allowed else { cameraPermissionDenied = true; return }
        cameraTarget = target; focused = false; showCamera = true
    }
    #endif
}

#if os(iOS)
private struct CameraPicker: UIViewControllerRepresentable {
    let finish: (UIImage?) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(finish: finish) }
    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.mediaTypes = [UTType.image.identifier]
        picker.cameraCaptureMode = .photo
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}
    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let finish: (UIImage?) -> Void
        init(finish: @escaping (UIImage?) -> Void) { self.finish = finish }
        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { finish(nil) }
        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            finish(info[.originalImage] as? UIImage)
        }
    }
}
#endif

/// Animate only the status glyphs, leaving the surrounding transcript layout still.
private struct PulsingText: View {
    let text: String
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    var body: some View {
        Text(text)
            .phaseAnimator(reduceMotion ? [false] : [false, true]) { content, dimmed in
                content.opacity(dimmed ? 0.4 : 1)
            } animation: { _ in .easeInOut(duration: 1.1) }
    }
}

private enum AttachmentImageSource: Hashable, Sendable {
    case file(URL)
    case inline(String)
}

private func videoTime(_ seconds: Double) -> String {
    let value = max(0, Int(seconds.rounded(.down)))
    return String(format: "%d:%02d", value / 60, value % 60)
}

private struct AttachmentMovieThumbnail: View {
    let attachment: MessageAttachment
    let poster: URL?
    let movie: URL?
    @State private var showingPreview = false
    var body: some View {
        Button { showingPreview = true } label: {
            AttachmentImageView(source: poster.map(AttachmentImageSource.file))
                .overlay { Image(systemName: "play.circle.fill").font(.title).foregroundStyle(.white).shadow(radius: 3) }
                .overlay(alignment: .bottomLeading) {
                    Text(videoTime(attachment.video?.duration ?? 0)).font(.caption2.monospacedDigit()).foregroundStyle(.white)
                        .padding(.horizontal, 5).padding(.vertical, 2).background(.black.opacity(0.65), in: Capsule()).padding(5)
                }
        }.buttonStyle(.plain).disabled(movie == nil)
            .accessibilityLabel("Preview " + attachment.name).accessibilityIdentifier("preview-video-" + attachment.id)
            .sheet(isPresented: $showingPreview) {
                if let movie { MoviePreview(url: movie, name: attachment.name) }
            }
    }
}

private struct MoviePreview: View {
    let url: URL
    let name: String
    @Environment(\.dismiss) private var dismiss
    @State private var player: AVPlayer?
    var body: some View {
        NavigationStack {
            VStack(spacing: 12) {
                VideoPlayer(player: player).accessibilityIdentifier("video-player")
            }.navigationTitle(name)
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }.frame(minWidth: 320, minHeight: 360)
            .onAppear { player = AVPlayer(url: url); player?.play() }
            .onDisappear { player?.pause(); player = nil }
    }
}

private struct VideoAttachmentView: View {
    let video: TranscriptVideo
    let model: InboxModel
    let agentID: String
    @State private var movie: URL?
    @State private var loading = false
    @State private var error: String?
    @State private var showingPreview = false
    @State private var selected = 0.0
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(video.name + " · " + videoTime(video.duration), systemImage: "film").font(.caption.weight(.medium)).lineLimit(2)
            if video.path != nil {
                Button {
                    loading = true; error = nil
                } label: {
                    if loading { ProgressView("Loading video…") }
                    else { Label("Play video", systemImage: "play.circle.fill") }
                }.disabled(loading).accessibilityIdentifier("play-original-video")
                if let error { Text(error).font(.caption).foregroundStyle(Ink.muted) }
            } else if !video.images.isEmpty {
                let index = min(video.images.count - 1, max(0, Int(selected)))
                AttachmentImageView(source: .inline(video.images[index]), contentMode: .fit).frame(height: 150)
                if video.images.count > 1 {
                    Slider(value: $selected, in: 0...Double(video.images.count - 1), step: 1)
                        .accessibilityLabel("Video frame").accessibilityIdentifier("video-frame-slider")
                }
                Text(videoTime(video.timestamps[index]) + " · \(video.images.count) frames · No audio")
                    .font(.caption2).foregroundStyle(Ink.muted)
            }
        }.frame(maxWidth: 260).accessibilityElement(children: .contain).accessibilityIdentifier("message-video")
            .task(id: loading) {
                guard loading else { return }
                do { movie = try await model.downloadVideo(video, agentID: agentID); showingPreview = true }
                catch is CancellationError { }
                catch { self.error = error.localizedDescription }
                loading = false
            }
            .sheet(isPresented: $showingPreview, onDismiss: {
                if let movie { try? FileManager.default.removeItem(at: movie) }
                movie = nil
            }) { if let movie { MoviePreview(url: movie, name: video.name) } }
    }
}

private struct AttachmentImageView: View {
    let source: AttachmentImageSource?
    var contentMode: ContentMode = .fill
    @State private var thumbnail: CGImage?

    var body: some View {
        Group {
            if let thumbnail {
                Image(decorative: thumbnail, scale: 1).resizable().aspectRatio(contentMode: contentMode)
            } else {
                Image(systemName: "photo").font(.title2).foregroundStyle(Ink.muted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Ink.surface).clipShape(RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .ignore).accessibilityLabel("Attached image")
        .task(id: source) {
            thumbnail = nil
            let captured = source
            let decoded = await Task.detached(priority: .utility) { Self.decode(captured) }.value
            guard !Task.isCancelled else { return }
            thumbnail = decoded
        }
    }

    private nonisolated static func decode(_ source: AttachmentImageSource?) -> CGImage? {
        let image: CGImageSource?
        switch source {
        case .file(let url):
            guard url.isFileURL else { return nil }
            image = CGImageSourceCreateWithURL(url as CFURL, nil)
        case .inline(let value):
            guard value.hasPrefix("data:image/"), let separator = value.firstIndex(of: ","),
                  value[..<separator].hasSuffix(";base64"),
                  let data = Data(base64Encoded: String(value[value.index(after: separator)...])) else { return nil }
            image = CGImageSourceCreateWithData(data as CFData, nil)
        case nil: return nil
        }
        guard let image else { return nil }
        return CGImageSourceCreateThumbnailAtIndex(image, 0, [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 480,
            kCGImageSourceShouldCacheImmediately: true,
        ] as CFDictionary)
    }
}

private struct ConnectView: View {
    @ObservedObject var model: InboxModel
    @State private var origin = "https://nanocodex.gakonst.workers.dev"
    @State private var phone = ""
    @State private var region = PhoneNumberInput.defaultRegion()
    @State private var code = SMSCodeInput()
    @State private var inputError: String?
    @State private var verificationScheduled = false
    @FocusState private var focus: Field?
    private enum Field { case phone, code }
    private var normalizedPhone: String? { try? PhoneNumberInput.normalize(phone, region: region) }
    private var canVerify: Bool {
        !model.signingIn && !verificationScheduled && model.challenge != nil
            && (model.signInRetryAt ?? .distantPast) <= .now
    }
    private func verify(_ value: String) {
        guard canVerify else { return }
        verificationScheduled = true; code.markSubmitted()
        Task { await model.verifySignIn(code: value); verificationScheduled = false }
    }
    var body: some View {
        ScrollView {
        VStack(alignment: .leading, spacing: 24) {
            Image(systemName: "bubble.left.and.bubble.right").font(.system(size: 36, weight: .regular)).foregroundStyle(Ink.accent)
            Text(model.challenge == nil ? "What are we working on?" : "Check your messages")
                .font(.system(size: 34, weight: .semibold))
            Text(model.challenge.map { "Enter the 6-digit code sent to \($0.phone)." }
                 ?? "Sign in with the same phone number you use on Nanocodex. Your agents will be here.")
                .foregroundStyle(Ink.muted)
            VStack(alignment: .leading, spacing: 14) {
                Text(model.challenge == nil ? "Phone number" : "Verification code").font(.subheadline.weight(.medium))
                if model.challenge == nil {
                    Picker("Country", selection: $region) {
                        ForEach(PhoneNumberInput.countries) { country in
                            Text(country.label).tag(country.id)
                        }
                    }.pickerStyle(.menu).accessibilityIdentifier("phone-country")
                    TextField(PhoneNumberInput.example(region: region), text: $phone)
                        .textContentType(.telephoneNumber)
                        #if os(iOS)
                        .keyboardType(.phonePad)
                        #endif
                        .focused($focus, equals: .phone).accessibilityIdentifier("phone-number")
                        .padding(15).background(Ink.surface, in: RoundedRectangle(cornerRadius: 12))
                        .onChange(of: phone) { _, _ in inputError = nil }
                } else {
                    TextField("000000", text: Binding(get: { code.text }, set: { value in
                        let automatic = canVerify && (model.challenge?.expiresAt ?? .distantPast) > .now
                        if let complete = code.update(value, canSubmit: automatic) { verify(complete) }
                    }))
                        .textContentType(.oneTimeCode)
                        #if os(iOS)
                        .keyboardType(.numberPad)
                        #endif
                        .font(.title2.monospacedDigit()).tracking(7).multilineTextAlignment(.center)
                        .focused($focus, equals: .code).accessibilityIdentifier("verification-code")
                        .padding(15).background(Ink.surface, in: RoundedRectangle(cornerRadius: 12))
                }
                Text(model.challenge == nil ? normalizedPhone.map { "We’ll text a code to \($0)." } ?? "We’ll add the country code for you." : "You’ll stay signed in securely on this device.")
                    .font(.caption).foregroundStyle(Ink.muted)
                    .accessibilityIdentifier("sign-in-hint")
            }.disabled(model.signingIn)
            if let error = inputError ?? model.signInError ?? model.error {
                Text(error).font(.subheadline).foregroundStyle(Ink.amber).accessibilityIdentifier("sign-in-error")
            }
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let retry = max(0, Int(ceil((model.signInRetryAt ?? .distantPast).timeIntervalSince(context.date))))
                VStack(spacing: 18) {
                    Button {
                        if model.challenge == nil {
                            do {
                                let normalized = try PhoneNumberInput.normalize(phone, region: region)
                                inputError = nil
                                Task { await model.startSignIn(phone: normalized, origin: origin) }
                            } catch { inputError = error.localizedDescription }
                        } else {
                            verify(code.text)
                        }
                    } label: {
                        HStack(spacing: 9) {
                            if model.signingIn { ProgressView().tint(Ink.background) }
                            Text(model.signingIn ? "Connecting…" : retry > 0 ? "Try again in \(retry)s" : model.challenge == nil ? "Continue" : "Sign in")
                                .fontWeight(.semibold)
                        }.frame(maxWidth: .infinity).padding(.vertical, 14)
                            .foregroundStyle(Ink.background).background(Ink.accent, in: RoundedRectangle(cornerRadius: 13))
                    }
                    .buttonStyle(.plain).accessibilityIdentifier("sign-in-submit")
                    .disabled(model.signingIn || verificationScheduled || retry > 0 || (model.challenge == nil ? phone.trimmingCharacters(in: .whitespaces).isEmpty : code.text.count != 6 || !canVerify))
                    if let challenge = model.challenge {
                        let seconds = max(retry, max(0, Int(ceil(challenge.resendAt.timeIntervalSince(context.date)))))
                        if context.date >= challenge.expiresAt {
                            Text("This code expired. Request another one.").font(.caption).foregroundStyle(Ink.muted)
                        }
                        HStack {
                            Button("Change number") {
                                Task { if await model.cancelSignIn() { code = SMSCodeInput(); focus = .phone } }
                            }
                            Spacer()
                            Button(seconds > 0 ? "Resend in \(seconds)s" : "Resend code") {
                                Task { await model.startSignIn(phone: challenge.phone, origin: origin); code = SMSCodeInput() }
                            }.disabled(seconds > 0)
                        }.font(.subheadline).disabled(model.signingIn)
                    }
                }
            }
            if model.challenge == nil {
                DisclosureGroup("Advanced") {
                    TextField("Server", text: $origin).textFieldStyle(.roundedBorder).autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.never).keyboardType(.URL)
                        #endif
                        .padding(.top, 8)
                }.font(.caption).foregroundStyle(Ink.muted).disabled(model.signingIn)
            }
            #if DEBUG
            Button("Explore the demo") {
                Task { if await model.cancelSignIn() { model.demo() } }
            }.disabled(model.signingIn)
            #endif
        }.padding(32).frame(maxWidth: 480)
        }.scrollDismissesKeyboard(.interactively)
            .onChange(of: model.challenge?.phone) { _, value in if value != nil { code = SMSCodeInput(); focus = .code } }
            .onChange(of: region) { _, _ in inputError = nil }
            .accessibilityIdentifier("phone-onboarding")
    }
}

private struct ConversationView: View {
    private let verticalPadding: CGFloat = 24
    @ObservedObject var model: InboxModel
    @Environment(\.dismiss) private var dismiss
    @FocusState private var composerFocused: Bool
    @State private var hasInitialPosition = false
    @State private var rowFrames: [String: CGRect] = [:]
    @State private var historyRestore: (id: String, anchor: UnitPoint)?
    @State private var historyContent = ConversationContentPosition()
    @State private var historyReady = false
    @State private var historyRequestAllowed = true
    @State private var historyRequestInFlight = false
    @State private var historyRequestFirstID: String?
    @State private var resizeRestore: [(id: String, y: CGFloat, height: CGFloat)]?
    private func rememberHistoryPosition(in viewport: GeometryProxy) {
        guard let first = rowFrames.filter({ $0.value.maxY > 0 && $0.value.minY < viewport.size.height })
            .min(by: { $0.value.minY < $1.value.minY }) else { return }
        let available = viewport.size.height - first.value.height
        historyRestore = (first.key, UnitPoint(x: 0, y: abs(available) > 0.5 ? first.value.minY / available : 0))
    }
    private func loadEarlierIfNeeded(in viewport: GeometryProxy) {
        guard historyReady, historyContent.nearTop, historyRequestAllowed, historyContent.firstID == model.rows.first?.id,
              model.hasOlder, !model.threadLoading, !model.loadingOlder, !historyRequestInFlight else { return }
        historyRequestAllowed = false
        historyRequestInFlight = true
        historyRequestFirstID = model.rows.first?.id
        rememberHistoryPosition(in: viewport)
        Task {
            await model.loadOlder()
            let inserted = model.rows.first?.id != historyRequestFirstID
            if !inserted { historyRestore = nil }
            historyRequestInFlight = false
            // Successful pages can prefetch again when their new beginning
            // enters view. A failed page needs a new approach to the top.
            if inserted { historyRequestAllowed = true }
            updateHistoryPosition(in: viewport)
        }
    }
    private func updateHistoryPosition(in viewport: GeometryProxy) {
        guard hasInitialPosition, historyContent.isMeasured else { return }
        // Ignore the transient top layout before a newly opened conversation
        // reaches its initial position at the bottom.
        if !historyReady {
            guard historyContent.atLatest else { return }
            historyReady = true
        }
        if !historyContent.nearTop { historyRequestAllowed = true }
        loadEarlierIfNeeded(in: viewport)
    }
    private func rememberReadingPosition(in viewport: GeometryProxy) {
        let frame = viewport.frame(in: .global)
        resizeRestore = rowFrames.filter { $0.value.maxY > 0 && $0.value.minY < frame.height }
            .sorted {
                let leftVisible = $0.value.minY >= 0 && $0.value.maxY <= frame.height
                let rightVisible = $1.value.minY >= 0 && $1.value.maxY <= frame.height
                return leftVisible == rightVisible ? $0.value.minY < $1.value.minY : leftVisible
            }
            .map { ($0.key, $0.value.minY + frame.minY, $0.value.height) }
    }
    var body: some View {
        NavigationStack {
            ScrollViewReader { scroll in
            GeometryReader { viewport in
            ZStack(alignment: .top) {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                LazyVStack(alignment: .leading, spacing: 18) {
                    if model.threadLoading { ProgressView("Loading conversation…") }
                    if let error = model.threadError { Text(error).font(.subheadline).foregroundStyle(Ink.muted) }
                    ForEach(ConversationItem.group(model.rows, activeTurns: Set(model.focused?.activeTurns ?? []))) { item in
                        Group {
                        if let row = item.message {
                        HStack(alignment: .top, spacing: 0) {
                            if row.role == "You" { Spacer(minLength: 44) }
                            VStack(alignment: .leading, spacing: 10) {
                                if row.role == "Thinking" {
                                    DisclosureGroup {
                                        ChatMarkdown(text: row.text)
                                    } label: {
                                        if row.running { PulsingText(text: "Thinking…") }
                                        else { Text("Thought process") }
                                    }.font(.system(size: 13)).foregroundStyle(Ink.muted)
                                } else if row.role == "You", let content = ContextPrompt.separate(row.text) {
                                    Text(content.request).font(.system(size: 17)).lineSpacing(5).textSelection(.enabled)
                                    DisclosureGroup("Captured context (\(content.captures.count))") {
                                        ForEach(Array(content.captures.enumerated()), id: \.offset) { _, capture in
                                            VStack(alignment: .leading, spacing: 4) {
                                                Text(capture.source + (capture.sender.isEmpty ? "" : " · " + capture.sender)).font(.caption.weight(.semibold))
                                                Text(capture.text.isEmpty ? capture.url : capture.text).font(.subheadline).textSelection(.enabled)
                                            }.padding(.vertical, 4)
                                        }
                                    }.font(.caption).foregroundStyle(Ink.muted)
                                } else if row.role == "Agent", !row.text.isEmpty {
                                    ChatMarkdown(text: row.text)
                                    if !row.running { ChatCopyButton(text: row.text).padding(.leading, -8) }
                                } else if !row.text.isEmpty {
                                    Text(row.text).font(.system(size: row.role == "Status" ? 14 : 17))
                                        .lineSpacing(5).textSelection(.enabled)
                                        .foregroundStyle(row.role == "Status" ? Ink.muted : Ink.text)
                                }
                                if let images = row.images {
                                    ForEach(Array(images.filter { $0.hasPrefix("data:image/") }.enumerated()), id: \.offset) { _, image in
                                        AttachmentImageView(source: .inline(image), contentMode: .fit)
                                            .frame(maxWidth: 240).frame(height: 180)
                                            .accessibilityIdentifier("message-image")
                                    }
                                }
                                ForEach(row.videos ?? []) { VideoAttachmentView(video: $0, model: model, agentID: model.focused?.id ?? "") }
                            }
                            .padding(row.role == "You" ? 16 : 0)
                            .background(row.role == "You" ? Ink.surface : Color.clear, in: RoundedRectangle(cornerRadius: 24))
                            if row.role != "You" { Spacer(minLength: 0) }
                        }.frame(maxWidth: .infinity, alignment: row.role == "You" ? .trailing : .leading)
                        } else {
                            VStack(alignment: .leading, spacing: 14) {
                                ConversationActivityView(item: item)
                                InboxGeneratedOutputView(rows: item.activity).equatable()
                            }
                        }
                        }.id(item.id)
                            .background(GeometryReader { geometry in
                                Color.clear.preference(key: ConversationRowFrames.self, value: [item.id: geometry.frame(in: .named("conversation-viewport"))])
                            })
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier("message-" + item.id)
                    }
                }.scrollTargetLayout()
                    // Keep the bottom target outside the lazy rows so it has a
                    // stable position even when the last reply fills many screens.
                    if let agentID = model.focused?.id {
                        NanocodexVoiceTranscript(session: model.voice, conversationID: agentID, durableRows: model.rows) {
                            if historyContent.atLatest { scroll.scrollTo("latest", anchor: .bottom) }
                        }
                    }
                    Color.clear.frame(height: 1).id("latest")
                }.padding(.horizontal, 20).padding(.vertical, verticalPadding).frame(maxWidth: 780).frame(minHeight: viewport.size.height, alignment: .top).frame(maxWidth: .infinity)
                    .background(GeometryReader { geometry in
                        let frame = geometry.frame(in: .named("conversation-viewport"))
                        // Publish threshold changes, not every fractional offset
                        // or lazy height estimate, to avoid driving another layout.
                        Color.clear.preference(key: ConversationContentFrame.self, value: ConversationContentPosition(
                            firstID: model.rows.first?.id, nearTop: frame.minY >= -240,
                            // The latest marker precedes the bottom padding.
                            atLatest: frame.maxY <= viewport.size.height + verticalPadding + 1,
                            isMeasured: frame.height > 0))
                    })
            }
            .modifier(ChatScrollAnchors())
            .scrollDismissesKeyboard(.interactively)
            .scrollBounceBehavior(.always, axes: .vertical)
            .coordinateSpace(name: "conversation-viewport")
            .onPreferenceChange(ConversationRowFrames.self) {
                rowFrames = $0
                // Continue following the reader during a slow history request,
                // until the insertion changes the coordinate space.
                if historyRequestInFlight, model.rows.first?.id == historyRequestFirstID {
                    rememberHistoryPosition(in: viewport)
                }
            }
            .onPreferenceChange(ConversationContentFrame.self) { content in
                historyContent = content
                updateHistoryPosition(in: viewport)
            }
            .onChange(of: hasInitialPosition) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.hasOlder) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.threadLoading) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: model.loadingOlder) { _, _ in updateHistoryPosition(in: viewport) }
            .onChange(of: composerFocused) { _, _ in rememberReadingPosition(in: viewport) }
            .onChange(of: model.draft) { _, _ in
                if resizeRestore == nil { rememberReadingPosition(in: viewport) }
            }
            .onChange(of: viewport.frame(in: .global)) { _, frame in
                // Modern scroll views retain their top edge when only the
                // viewport changes. A second scrollTo using lazy row estimates
                // can move the reader when the keyboard resizes the sheet.
                if #available(iOS 18.0, macOS 15.0, *) { return }
                // Keep the same screen position through keyboard and composer resizing.
                // A clipped row can require an impossible unit anchor; prefer a fully
                // visible row and try the other captured rows before moving anything.
                guard let targets = resizeRestore else { return }
                for target in targets {
                    let available = frame.height - target.height
                    guard abs(available) > 0.5 else { continue }
                    let anchorY = (target.y - frame.minY) / available
                    guard (0...1).contains(anchorY) else { continue }
                    var transaction = Transaction()
                    transaction.disablesAnimations = true
                    withTransaction(transaction) { scroll.scrollTo(target.id, anchor: UnitPoint(x: 0, y: anchorY)) }
                    break
                }
            }
            .simultaneousGesture(DragGesture(minimumDistance: 1).onChanged { _ in resizeRestore = nil })
            .background(Ink.background)
            .accessibilityIdentifier("conversation")
            if model.loadingOlder {
                ProgressView("Loading earlier messages…").font(.caption)
                    .padding(10).background(Ink.background, in: Capsule())
                    .allowsHitTesting(false)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Loading earlier messages")
                    .accessibilityIdentifier("loading-older")
            }
            }
            }
            .onChange(of: model.rows.first?.id, initial: true) { _, _ in
                if !hasInitialPosition, !model.rows.isEmpty {
                    // Position a newly opened conversation once. Later viewport
                    // changes, including the keyboard, retain its top edge.
                    scroll.scrollTo("latest", anchor: .bottom)
                    hasInitialPosition = true
                    return
                }
                guard let target = historyRestore else { return }
                var transaction = Transaction()
                transaction.disablesAnimations = true
                withTransaction(transaction) {
                    scroll.scrollTo(target.id, anchor: target.anchor)
                    historyRestore = nil
                }
            }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                AgentComposerView(model: model, focused: $composerFocused).frame(maxWidth: 620)
            }
            .navigationTitle(model.focused?.title ?? "Conversation")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }

        }.foregroundStyle(Ink.text).frame(minWidth: 340)
            .presentationDetents([.large]).presentationDragIndicator(.visible)
            // A downward drag while typing belongs to the keyboard, not the sheet.
            .interactiveDismissDisabled(composerFocused)
            .sheet(isPresented: $model.showContext) { ContextInboxView(model: model) }
    }
}

private struct InboxGeneratedOutputView: View, Equatable {
    private let results: [[String]]
    @State private var outputs: [ChatGeneratedOutput] = []

    init(rows: [TranscriptRow]) {
        results = rows.compactMap { $0.tool?.generatedResults }
    }
    static func == (lhs: Self, rhs: Self) -> Bool { lhs.results == rhs.results }
    var body: some View {
        Group { if !outputs.isEmpty { ChatGeneratedOutputs(outputs: outputs) } }
            .task(id: results) {
                let captured = results
                let parsed = await Task.detached(priority: .utility) {
                    // This also applies to replayed rows that used to opt exec
                    // output into chat. Tool text belongs inside Activity.
                    var seen = Set<String>()
                    return captured.flatMap { ChatGeneratedOutput.parse(results: $0) }
                        .filter { seen.insert($0.id).inserted }
                }.value
                guard !Task.isCancelled else { return }
                outputs = parsed
            }
    }
}

private struct CardScrollAnchors: ViewModifier {
    let isComposing: Bool
    @State private var hasInteracted = false

    func body(content: Content) -> some View {
        Group {
            if #available(iOS 18.0, macOS 15.0, *) {
                content.modifier(ChatScrollAnchors())
                    .defaultScrollAnchor(hasInteracted ? .top : .bottom, for: .sizeChanges)
            } else {
                content.defaultScrollAnchor(hasInteracted ? .top : .bottom)
            }
        }
        // Card content can arrive after the view first appears. Follow it
        // until the reader scrolls or starts composing.
        .simultaneousGesture(DragGesture(minimumDistance: 1).onChanged { _ in hasInteracted = true })
        .onChange(of: isComposing, initial: true) { _, focused in
            if focused { hasInteracted = true }
        }
    }
}

private struct ChatScrollAnchors: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 18.0, macOS 15.0, *) {
            // Lazy Markdown rows can acquire their height after ScrollViewReader's
            // first scrollTo. Let the scroll view place new chats itself,
            // while keyboard and history resizing continue to retain the top edge.
            content.defaultScrollAnchor(.top).defaultScrollAnchor(.bottom, for: .initialOffset)
        } else {
            content.defaultScrollAnchor(.bottom)
        }
    }
}

private struct ConversationRowFrames: PreferenceKey {
    static var defaultValue: [String: CGRect] { [:] }
    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue()) { _, new in new }
    }
}

private struct ConversationContentPosition: Equatable {
    var firstID: String?
    var nearTop = false
    var atLatest = false
    var isMeasured = false
}

private struct ConversationContentFrame: PreferenceKey {
    static var defaultValue: ConversationContentPosition { ConversationContentPosition() }
    static func reduce(value: inout ConversationContentPosition, nextValue: () -> ConversationContentPosition) {
        let next = nextValue()
        if next.isMeasured { value = next }
    }
}

private struct ConversationActivityView: View {
    let item: ConversationItem
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var expanded = false
    private var failures: Int { item.activity.filter { $0.tool?.status == "Failed" }.count }
    private var title: String {
        guard item.isRunning else { return "Activity" }
        if let last = item.activity.last, last.role == "Tool", last.running { return last.tool?.title ?? "Working" }
        return item.activity.last?.role == "Thinking" ? "Thinking" : "Working"
    }
    var body: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.16)) { expanded.toggle() }
            } label: {
                HStack(spacing: 10) {
                    Group {
                        if item.isRunning { ProgressView().controlSize(.mini) }
                        else { Image(systemName: failures > 0 ? "exclamationmark.circle" : "checkmark").font(.system(size: 12, weight: .medium)) }
                    }.frame(width: 18, height: 18)
                    Text(title).font(.system(size: 13, weight: .medium)).lineLimit(1)
                    Spacer(minLength: 4)
                    if failures > 0 { Text("\(failures) issue\(failures == 1 ? "" : "s")").foregroundStyle(Ink.amber).font(.caption) }
                    if !item.activity.isEmpty { Text("\(item.activity.count) step\(item.activity.count == 1 ? "" : "s")").font(.caption).monospacedDigit() }
                    Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).rotationEffect(.degrees(expanded ? 90 : 0))
                }.foregroundStyle(Ink.muted).padding(.horizontal, 13).frame(minHeight: 44).contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("activity-disclosure")
                .accessibilityLabel(title).accessibilityValue("\(item.activity.count) step\(item.activity.count == 1 ? "" : "s"), \(failures) issue\(failures == 1 ? "" : "s"), " + (expanded ? "Expanded" : "Collapsed"))
            if expanded {
                Rectangle().fill(Ink.border).frame(height: 0.5).padding(.horizontal, 13)
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(item.activity) { row in
                            ConversationActivityStep(row: row, live: item.isRunning && row.running)
                        }
                        if item.activity.isEmpty { Text("Waiting for the first update…").font(.caption).foregroundStyle(Ink.muted).padding(12) }
                    }.padding(6)
                }.frame(maxHeight: 300).fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("activity-timeline")
            }
        }.background(Ink.surface.opacity(0.55), in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Ink.border.opacity(0.7), lineWidth: 0.5))
            .accessibilityElement(children: .contain).accessibilityIdentifier("activity-group")
    }
}

private struct ConversationActivityStep: View {
    let row: TranscriptRow
    let live: Bool
    @State private var expanded = false
    private var failed: Bool { row.tool?.status == "Failed" }
    private var title: String { row.tool?.title ?? (row.role == "Thinking" ? "Thinking" : "Progress update") }
    private var subject: String {
        if let subject = row.tool?.subject { return subject }
        let firstLine = String(row.text.split(whereSeparator: \.isNewline).first ?? "")
        let parsed = try? AttributedString(markdown: firstLine, options: .init(failurePolicy: .returnPartiallyParsedIfPossible))
        return parsed.map { String($0.characters) } ?? firstLine
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { expanded.toggle() } label: {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: failed ? "exclamationmark.circle" : row.role == "Tool" ? "terminal" : "text.alignleft")
                        .font(.system(size: 12)).frame(width: 18, height: 18).foregroundStyle(failed ? Ink.amber : Ink.muted)
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 8) {
                            Text(title).font(.system(size: 13, weight: .medium)).foregroundStyle(Ink.text).lineLimit(1)
                            if let status = row.tool?.status {
                                Text(status).font(.system(size: 11)).foregroundStyle(failed ? Ink.amber : Ink.muted)
                            }
                        }
                        if !subject.isEmpty { Text(subject).font(.system(size: 12)).foregroundStyle(Ink.muted).lineLimit(1) }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                    if live { ProgressView().controlSize(.mini) }
                    Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold))
                        .rotationEffect(.degrees(expanded ? 90 : 0)).foregroundStyle(Ink.muted).padding(.top, 3)
                }.padding(9).frame(minHeight: 44).contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("activity-step-" + row.id)
                .accessibilityValue(expanded ? "Expanded" : "Collapsed")
            if expanded {
                ScrollView {
                    Group {
                        if row.tool != nil { ToolActivityView(row: row) }
                        else if row.role == "Thinking" { ChatMarkdown(text: row.text) }
                        else { Text(row.text).font(.system(size: 14)).lineSpacing(4).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
                    }.padding(12)
                }.frame(maxHeight: 240).fixedSize(horizontal: false, vertical: true)
                    .background(Ink.background.opacity(0.8), in: RoundedRectangle(cornerRadius: 9))
                    .padding(.leading, 28).padding(.horizontal, 6).padding(.bottom, 8)
                    .accessibilityIdentifier("activity-detail-" + row.id)
            }
        }
    }
}

private struct ToolActivityView: View {
    let row: TranscriptRow
    private var tool: ToolPresentation {
        if let tool = row.tool { return tool }
        var fallback = ToolPresentation(name: row.text, arguments: .null)
        if !row.running { fallback.finish(.string(row.detail)) }
        return fallback
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if !tool.input.isEmpty { fields(tool.input, heading: "Input") }
            if !tool.output.isEmpty { fields(tool.output, heading: "Result") }
        }.foregroundStyle(Ink.muted).accessibilityIdentifier("tool-activity")
    }
    private func fields(_ values: [ToolField], heading: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(heading).font(.system(size: 12, weight: .semibold)).foregroundStyle(Ink.muted)
            ForEach(Array(values.enumerated()), id: \.offset) { _, field in
                VStack(alignment: .leading, spacing: 3) {
                    if field.label != heading { Text(field.label).font(.system(size: 12)).foregroundStyle(Ink.muted) }
                    Text(field.value).font(.system(size: 14, design: field.code ? .monospaced : .default))
                        .foregroundStyle(Ink.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}
