import SwiftUI
import InboxCore
#if os(iOS)
import UIKit
#endif

private enum Ink {
    static let background = Color.white
    static let card = Color.white
    static let surface = Color(white: 0.96)
    static let text = Color(white: 0.08)
    static let muted = Color(white: 0.43)
    static let border = Color(white: 0.89)
    static let accent = Color(white: 0.08)
    static let amber = Color(white: 0.36)
}

struct InboxView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drag: CGFloat = 0
    @State private var voiceTarget: AgentCard?
    @State private var showThread = false
    @State private var showAgents = false
    @State private var showSettings = false
    @State private var showStop = false
    @State private var stopTarget: (agent: String, turn: String, title: String)?
    @FocusState private var composerFocused: Bool

    var body: some View {
        ZStack {
            Ink.background.ignoresSafeArea()
            if model.connected {
                VStack(spacing: 10) {
                    if !composerFocused {
                        header
                        filters
                    }
                    if let card = model.focused { deck(card) }
                    else { emptyState.frame(maxHeight: .infinity) }
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
                .padding(.horizontal, 16).padding(.top, 4).padding(.bottom, 4)
                .frame(maxWidth: 620)
                .safeAreaInset(edge: .bottom, spacing: 0) { if model.focused != nil { composer.frame(maxWidth: 620) } }
            } else { ConnectView(model: model) }
        }
        .preferredColorScheme(.light)
        .foregroundStyle(Ink.text)
        .tint(Ink.accent)
        .sheet(isPresented: $showThread, onDismiss: { model.closeThread() }) { ConversationView(model: model).tint(Ink.accent) }
        .sheet(item: $voiceTarget) { card in
            VoiceInputView(agentTitle: card.title, demo: model.isDemo) { text in
                model.appendVoiceDraft(text, agentID: card.id)
            }
        }
        .sheet(isPresented: $showAgents) { agentList }
        .sheet(isPresented: $showSettings) { settings }
        .confirmationDialog("Stop this turn?", isPresented: $showStop, titleVisibility: .visible) {
            Button("Stop turn", role: .destructive) { if let target = stopTarget { Task { await model.stop(agentID: target.agent, turnID: target.turn) } } }
        } message: { Text("The selected turn in \(stopTarget?.title ?? "this agent") will stop. You can send a new follow-up afterward.") }
        .onChange(of: model.draft) { old, value in
            if !old.isEmpty && value.isEmpty { composerFocused = false }
        }
        .onChange(of: model.focused?.activeTurns ?? []) { _, turns in
            if !turns.contains(model.selectedTurn) { model.selectedTurn = turns.first ?? "" }
        }
        .onChange(of: model.deck.focusedID) { _, _ in drag = 0; composerFocused = false; showStop = false }
    }
    private var header: some View {
        HStack(spacing: 8) {
            Button { showAgents = true } label: {
                Image(systemName: "line.3.horizontal.decrease").font(.system(size: 21)).frame(width: 40, height: 44)
            }.accessibilityLabel("Browse agents")
            Button { showSettings = true } label: {
                HStack(spacing: 6) {
                    Text("nanocodex").font(.system(size: 21, weight: .semibold)).lineLimit(1)
                    Image(systemName: "chevron.down").font(.system(size: 11, weight: .semibold)).foregroundStyle(Ink.muted)
                }
            }.accessibilityLabel("Account settings")
            Spacer(minLength: 4)
            Text(model.connection).font(.system(size: 12)).foregroundStyle(Ink.muted).lineLimit(1)
                .accessibilityIdentifier("connection")
            Button { Task { await model.newAgent() } } label: {
                Image(systemName: "square.and.pencil").font(.system(size: 21)).frame(width: 44, height: 44)
            }.disabled(model.isCreating).accessibilityLabel("New agent")
        }.foregroundStyle(Ink.text).buttonStyle(.plain)
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
                        .padding(.horizontal, 14).padding(.vertical, 8)
                        .frame(minHeight: 44)
                        .foregroundStyle(model.filter == filter ? Ink.background : Ink.muted)
                        .background(model.filter == filter ? Ink.accent : Ink.surface, in: Capsule())
                }.buttonStyle(.plain).accessibilityIdentifier("filter-" + filter.rawValue)
            }
            Spacer(minLength: 0)
            Button { Task { await model.refresh() } } label: {
                Image(systemName: "arrow.clockwise").frame(width: 44, height: 44)
            }.disabled(model.refreshing || model.isDemo).accessibilityLabel("Refresh agents")
        }
    }
    private func deck(_ card: AgentCard) -> some View {
        GeometryReader { _ in
            ZStack {
                RoundedRectangle(cornerRadius: 28).fill(Ink.surface.opacity(0.6)).padding(.horizontal, 18).offset(y: 16)
                RoundedRectangle(cornerRadius: 28).fill(Ink.surface).padding(.horizontal, 9).offset(y: 8)
                cardFace(card).id(card.id).transition(.opacity)
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
                    .simultaneousGesture(DragGesture(minimumDistance: 18)
                        .onChanged { value in
                            if abs(value.translation.width) > abs(value.translation.height) * 1.3 { drag = value.translation.width }
                        }
                        .onEnded { value in
                            if abs(drag) > 65 || (abs(drag) > 24 && abs(value.predictedEndTranslation.width) > 130) { advance(reviewed: drag > 0) }
                            withAnimation(reduceMotion ? nil : .snappy(duration: 0.16)) { drag = 0 }
                        })
            }
        }.frame(minHeight: composerFocused ? 0 : 220, maxHeight: .infinity).padding(.bottom, 12)
    }
    private func cardFace(_ card: AgentCard) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Label(card.error == nil ? card.status : "Update unavailable", systemImage: card.isRunning ? "waveform" : card.status == "Failed" ? "exclamationmark.circle" : "checkmark.circle")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(card.isRunning || card.error != nil ? Ink.amber : Ink.accent)
                Text(card.model).font(.system(size: 11)).foregroundStyle(Ink.muted).lineLimit(1)
                Spacer(minLength: 4)
                Text("\(model.deck.order.firstIndex(of: card.id).map { $0 + 1 } ?? 1) / \(model.deck.order.count)")
                    .font(.system(size: 13).monospacedDigit()).foregroundStyle(Ink.muted)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(card.title)
                        .font(.system(size: 22, weight: .semibold))
                        .fixedSize(horizontal: false, vertical: true).accessibilityIdentifier("agent-title")
                    Text(card.error ?? card.preview)
                        .font(.system(size: 17)).foregroundStyle(Ink.text)
                        .lineSpacing(3).fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("agent-preview")
                }.frame(maxWidth: .infinity, alignment: .leading)
            }.scrollIndicators(.hidden)
        }
        .padding(18).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Ink.card, in: RoundedRectangle(cornerRadius: 28))
        .overlay(RoundedRectangle(cornerRadius: 28).stroke(Ink.border, lineWidth: 0.75))
        .accessibilityElement(children: .contain)
        .onTapGesture { model.openThread(); showThread = true }
        .accessibilityAction(named: "Open thread") { model.openThread(); showThread = true }
        .accessibilityAction(named: "Previous agent") { model.back() }
        .contextMenu { Button("Previous agent") { model.back() }.disabled(!model.canGoBack) }
        .accessibilityAction(named: "Next agent, revisit later") { advance(reviewed: false) }
        .accessibilityAction(named: "Mark update seen") { advance(reviewed: true) }
        .accessibilityIdentifier("agent-card")
    }
    private var composer: some View {
        VStack(spacing: 10) {
            if let card = model.focused, card.activeTurns.count > 1 {
                Picker("Active turn", selection: $model.selectedTurn) {
                    ForEach(card.activeTurns, id: \.self) { id in Text(String(id.prefix(12))).tag(id) }
                }.pickerStyle(.menu)
            }
            HStack(alignment: .bottom, spacing: 8) {
                if model.focused?.isRunning == true {
                    Button {
                        if let card = model.focused { stopTarget = (card.id, model.focusedTurn, card.title); showStop = true }
                    } label: {
                        Image(systemName: "stop.circle").frame(width: 44, height: 44).contentShape(Rectangle())
                    }.buttonStyle(.plain).accessibilityLabel("Stop turn")
                        .disabled(model.busy.contains(model.focused?.id ?? ""))
                }
                TextField("Message this agent…", text: $model.draft, axis: .vertical)
                    .lineLimit(1...4).textFieldStyle(.plain).font(.body).focused($composerFocused)
                    .padding(.vertical, 12).accessibilityIdentifier("composer")
                Button { composerFocused = false; voiceTarget = model.focused } label: {
                    Image(systemName: "mic").font(.system(size: 20)).frame(width: 44, height: 44)
                }.buttonStyle(.plain).accessibilityLabel("Voice input")
                Button { model.send() } label: {
                    Image(systemName: model.busy.contains(model.focused?.id ?? "") ? "ellipsis" : "arrow.up")
                        .font(.system(size: 18, weight: .semibold)).frame(width: 44, height: 44)
                        .background(Ink.accent, in: Circle()).foregroundStyle(Ink.background)
                }.buttonStyle(.plain).disabled(model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.busy.contains(model.focused?.id ?? ""))
                    .accessibilityLabel(model.focused?.isRunning == true ? "Queue message" : "Send message")
                    .accessibilityIdentifier("send").keyboardShortcut(.return, modifiers: .command)
            }.padding(10).padding(.leading, 6).background(Ink.surface, in: RoundedRectangle(cornerRadius: 30))
            if !model.focusedPending.isEmpty {
                ScrollView {
                    VStack(spacing: 4) {
                        ForEach(model.focusedPending) { message in
                            HStack(alignment: .center, spacing: 8) {
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(message.phase == .submitting ? "Sending…" : message.phase == .starting ? "Stopping current turn…" : message.phase == .cancelling ? "Cancelling…" : message.phase == .failed ? "Not confirmed" : "Queued")
                                        .font(.system(size: 11)).foregroundStyle(Ink.muted)
                                    Text(message.input).font(.system(size: 13)).lineLimit(2)
                                        .accessibilityIdentifier("pending-message")
                                    if let error = message.error { Text(error).font(.caption2).foregroundStyle(Ink.muted) }
                                }.frame(maxWidth: .infinity, alignment: .leading)
                                if message.phase == .failed {
                                    Button { model.retryPending(message.id) } label: { Text("Retry").frame(minHeight: 44) }.accessibilityIdentifier("retry-pending")
                                } else if message.id == model.focusedPending.first?.id, message.interruption != nil {
                                    Button { model.steerNow(message.id) } label: { Text("Steer now").frame(minHeight: 44) }.accessibilityIdentifier("steer-now")
                                }
                                Button { model.cancelPending(message.id) } label: {
                                    Image(systemName: "xmark").frame(width: 44, height: 44).contentShape(Rectangle())
                                }.accessibilityLabel("Cancel queued message")
                            }.font(.system(size: 13, weight: .medium)).buttonStyle(.plain)
                                .disabled(model.busy.contains(message.agentID) || message.phase == .starting || message.phase == .cancelling)
                                .padding(.leading, 12)
                        }
                    }
                }.frame(maxHeight: model.focusedPending.count > 1 ? 126 : model.focusedPending.first?.error == nil ? 58 : 92)
                    .accessibilityIdentifier("pending-messages")
            }
        }.padding(.horizontal, 16).padding(.top, 6).padding(.bottom, 8).background(Ink.background)
    }

    private var emptyState: some View {
        VStack(spacing: 18) {
            Image(systemName: "tray").font(.system(size: 46, weight: .ultraLight)).foregroundStyle(Ink.accent)
            Text(model.filter == .running ? "Nothing running" : "You're caught up").font(.title2.weight(.medium))
            Text("Start something new, or check all your agents.").font(.subheadline).foregroundStyle(Ink.muted).multilineTextAlignment(.center)
            Button("New agent") { Task { await model.newAgent() } }.buttonStyle(.borderedProminent).disabled(model.isCreating)
            Button("View all agents") { model.filter = .all }
        }
    }
    private var agentList: some View {
        NavigationStack {
            List(model.cards) { card in
                Button { model.select(card.id); showAgents = false } label: {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(card.title).foregroundStyle(Ink.text)
                        Text(card.status).font(.caption).foregroundStyle(card.isRunning ? Ink.amber : Ink.muted)
                    }.padding(.vertical, 6)
                }
            }.navigationTitle("All agents")
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { showAgents = false } }
                    ToolbarItem(placement: .primaryAction) { Button { showAgents = false; Task { await model.newAgent() } } label: { Image(systemName: "plus") }.accessibilityLabel("New agent") }
                }
        }.frame(minWidth: 340, minHeight: 440).presentationDetents([.medium, .large])
    }
    private var settings: some View {
        NavigationStack {
            Form {
                Section("Connection") {
                    Text(model.isDemo ? "Demo · sample agents" : "Nanocodex account connected")
                    Text("Agents keep running when you swipe away or close the app.").foregroundStyle(.secondary)
                    Button(model.isDemo ? "Connect account" : "Disconnect account") {
                        do { try model.disconnect(); showSettings = false } catch { model.error = error.localizedDescription }
                    }
                }
                Section("Controls") {
                    Text("Swipe left: revisit later\nSwipe right: mark this update seen\nTap card: read the conversation\nSend: queue a message\nSteer now: stop the current turn so the queued message can start")
                    Text("Long-press a card to go back. ⌘Return sends your message.").font(.caption)
                }
            }.navigationTitle("Inbox settings").toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showSettings = false } } }
        }.frame(minWidth: 340, minHeight: 440).presentationDetents([.medium, .large])
    }
    private func advance(reviewed: Bool) {
        composerFocused = false
        #if os(iOS)
        UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        #endif
        withAnimation(reduceMotion ? nil : .snappy(duration: 0.18)) { model.advance(reviewed: reviewed); drag = 0 }
    }
}

private struct ConnectView: View {
    @ObservedObject var model: InboxModel
    @State private var origin = "https://nanocodex.gakonst.workers.dev"
    @State private var key = ""
    @State private var connecting = false
    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            Image(systemName: "bubble.left.and.bubble.right").font(.system(size: 36, weight: .regular)).foregroundStyle(Ink.accent)
            Text("What are we working on?").font(.system(size: 34, weight: .semibold))
            Text("Your agents, one card at a time. Catch up, steer, and keep moving.").foregroundStyle(Ink.muted)
            VStack(spacing: 14) {
                TextField("Server", text: $origin).textFieldStyle(.roundedBorder).autocorrectionDisabled()
                SecureField("Account API key", text: $key).textFieldStyle(.roundedBorder)
                Button(connecting ? "Connecting…" : "Connect account") {
                    connecting = true
                    Task {
                        do { try await model.connect(origin: origin, key: key); key = "" }
                        catch { model.error = error.localizedDescription }
                        connecting = false
                    }
                }.buttonStyle(.borderedProminent).controlSize(.large).disabled(connecting || key.isEmpty)
            }
            Text("Use an account key from Nanocodex → Settings → API keys. It is saved in your device's Keychain.").font(.caption).foregroundStyle(Ink.muted)
            if let error = model.error { Text(error).font(.caption).foregroundStyle(Ink.amber) }
            Button("Explore the demo") { model.demo() }.disabled(connecting)
        }.padding(32).frame(maxWidth: 480)
    }
}

private struct ConversationView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 18) {
                    if model.hasOlder { Button(model.loadingOlder ? "Loading…" : "Load earlier messages") { Task { await model.loadOlder() } }.disabled(model.loadingOlder) }
                    ForEach(model.rows) { row in
                        HStack(alignment: .top, spacing: 0) {
                            if row.role == "You" { Spacer(minLength: 44) }
                            VStack(alignment: .leading, spacing: 10) {
                                if row.role != "You" {
                                    HStack(spacing: 8) {
                                        Text(row.role == "Agent" ? "nanocodex" : row.role)
                                            .font(.system(size: 13, weight: .medium))
                                        if row.running { ProgressView().controlSize(.mini) }
                                    }.foregroundStyle(Ink.muted)
                                }
                                Text(row.text).font(.system(size: 17)).lineSpacing(5).textSelection(.enabled)
                                if !row.detail.isEmpty {
                                    DisclosureGroup("Details") {
                                        Text(row.detail).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                                    }.font(.subheadline).foregroundStyle(Ink.muted)
                                }
                            }
                            .padding(row.role == "You" ? 16 : 0)
                            .background(row.role == "You" ? Ink.surface : Color.clear, in: RoundedRectangle(cornerRadius: 24))
                            if row.role != "You" { Spacer(minLength: 0) }
                        }.frame(maxWidth: .infinity, alignment: row.role == "You" ? .trailing : .leading)
                    }
                    Color.clear.frame(height: 1).id("latest")
                }.padding(18)
            }
            .background(Ink.background)
            .navigationTitle(model.focused?.title ?? "Conversation")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }

        }.foregroundStyle(Ink.text).frame(minWidth: 340, minHeight: 520).presentationDetents([.large]).presentationDragIndicator(.visible)
    }
}
