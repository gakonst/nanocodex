import SwiftUI
import InboxCore
#if os(iOS)
import UIKit
#endif

private enum Ink {
    static let background = Color(red: 0.065, green: 0.07, blue: 0.068)
    static let card = Color(red: 0.11, green: 0.12, blue: 0.115)
    static let muted = Color(red: 0.58, green: 0.62, blue: 0.59)
    static let accent = Color(red: 0.77, green: 0.92, blue: 0.55)
    static let amber = Color(red: 0.98, green: 0.71, blue: 0.40)
}

struct InboxView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drag: CGFloat = 0
    @State private var showThread = false
    @State private var showAgents = false
    @State private var showSettings = false
    @State private var showStop = false
    @State private var stopTarget: (agent: String, turn: String, title: String)?
    @State private var sendFollowUp = false
    @FocusState private var composerFocused: Bool

    var body: some View {
        ZStack {
            Ink.background.ignoresSafeArea()
            if model.connected {
                VStack(spacing: 20) {
                    header
                    filters
                    if let card = model.focused { deck(card) }
                    else { emptyState.frame(maxHeight: .infinity) }
                    if model.focused != nil { actions }
                    if let error = model.error {
                        HStack(alignment: .top) {
                            Text(error).font(.caption).foregroundStyle(Ink.amber)
                            Button { model.error = nil } label: { Image(systemName: "xmark") }.accessibilityLabel("Dismiss error")
                        }
                        .padding(12).background(Ink.card, in: RoundedRectangle(cornerRadius: 12))
                    } else if let notice = model.notice {
                        Text(notice).font(.caption).foregroundStyle(Ink.muted).accessibilityIdentifier("notice")
                    }
                }
                .padding(.horizontal, 24).padding(.top, 18).padding(.bottom, 12)
                .frame(maxWidth: 620)
                .safeAreaInset(edge: .bottom, spacing: 0) { if model.focused != nil { composer.frame(maxWidth: 620) } }
            } else { ConnectView(model: model) }
        }
        .preferredColorScheme(.dark)
        .tint(Ink.accent)
        .sheet(isPresented: $showThread) { ConversationView(model: model) }
        .sheet(isPresented: $showAgents) { agentList }
        .sheet(isPresented: $showSettings) { settings }
        .confirmationDialog("Stop this turn?", isPresented: $showStop, titleVisibility: .visible) {
            Button("Stop turn", role: .destructive) { if let target = stopTarget { Task { await model.stop(agentID: target.agent, turnID: target.turn) } } }
        } message: { Text("The selected turn in \(stopTarget?.title ?? "this agent") will stop. You can send a new follow-up afterward.") }
        .onChange(of: model.focused?.activeTurns ?? []) { _, turns in
            if !turns.contains(model.selectedTurn) { model.selectedTurn = turns.first ?? "" }
        }
        .onChange(of: model.deck.focusedID) { _, _ in drag = 0; composerFocused = false; showStop = false }
    }
    private var header: some View {
        VStack(alignment: .leading, spacing: 22) {
            HStack {
                HStack(spacing: 8) {
                    Image(systemName: "terminal").foregroundStyle(Ink.accent)
                    Text("nanocodex").font(.system(.body, design: .monospaced).weight(.semibold))
                }
                Spacer()
                Button { showAgents = true } label: { Image(systemName: "rectangle.stack").frame(width: 44, height: 44) }
                    .accessibilityLabel("Browse agents")
                Button { showSettings = true } label: { Image(systemName: "slider.horizontal.3").frame(width: 44, height: 44) }
                    .accessibilityLabel("Account settings")
            }.foregroundStyle(.white)
            HStack(alignment: .bottom) {
                VStack(alignment: .leading, spacing: 7) {
                    Text("Agent inbox").font(.system(size: 32, weight: .semibold, design: .rounded))
                    Text("\(model.attentionCount) to review · \(model.runningCount) running").font(.subheadline).foregroundStyle(Ink.muted)
                }
                Spacer()
                HStack(spacing: 5) {
                    Circle().fill(model.connection == "Live" ? Ink.accent : Ink.amber).frame(width: 5, height: 5)
                    Text(model.connection).font(.system(size: 10, weight: .medium, design: .monospaced))
                }.foregroundStyle(Ink.muted).padding(.bottom, 4).accessibilityIdentifier("connection")
            }
        }
    }
    private var filters: some View {
        HStack(spacing: 6) {
            ForEach(InboxModel.Filter.allCases, id: \.self) { filter in
                Button { model.filter = filter } label: {
                    Text(filter.rawValue).font(.subheadline.weight(.medium))
                        .padding(.horizontal, 18).padding(.vertical, 10)
                        .foregroundStyle(model.filter == filter ? Ink.background : Ink.muted)
                        .background(model.filter == filter ? Color.white : Color.clear, in: Capsule())
                }.buttonStyle(.plain).accessibilityIdentifier("filter-" + filter.rawValue)
            }
            Spacer(minLength: 0)
            Button { Task { await model.refresh() } } label: {
                Image(systemName: "arrow.clockwise").frame(width: 44, height: 44)
            }.disabled(model.refreshing || model.isDemo).accessibilityLabel("Refresh agents")
        }
    }
    private func deck(_ card: AgentCard) -> some View {
        GeometryReader { geometry in
            ZStack {
                RoundedRectangle(cornerRadius: 28).fill(Ink.card.opacity(0.4)).padding(.horizontal, 18).offset(y: 16)
                RoundedRectangle(cornerRadius: 28).fill(Ink.card.opacity(0.7)).padding(.horizontal, 9).offset(y: 8)
                cardFace(card, compact: geometry.size.height < 330)
                    .overlay(alignment: drag > 0 ? .topLeading : .topTrailing) {
                        if abs(drag) > 24 {
                            Text(drag > 0 ? "SEEN" : "LATER")
                                .font(.system(.title2, design: .monospaced).bold())
                                .padding(12).background(Ink.background, in: RoundedRectangle(cornerRadius: 10))
                                .foregroundStyle(drag > 0 ? Ink.accent : Ink.amber)
                                .overlay(RoundedRectangle(cornerRadius: 10).stroke(drag > 0 ? Ink.accent : Ink.amber, lineWidth: 2))
                                .rotationEffect(.degrees(drag > 0 ? -9 : 9)).padding(24)
                        }
                    }
                    .rotationEffect(.degrees(reduceMotion ? 0 : Double(drag / 28)))
                    .offset(x: drag)
                    .gesture(DragGesture(minimumDistance: 18)
                        .onChanged { value in
                            if abs(value.translation.width) > abs(value.translation.height) * 1.3 { drag = value.translation.width }
                        }
                        .onEnded { value in
                            if abs(drag) > 85 || (abs(drag) > 30 && abs(value.predictedEndTranslation.width) > 160) { advance(reviewed: drag > 0) }
                            withAnimation(reduceMotion ? nil : .spring(response: 0.3, dampingFraction: 0.8)) { drag = 0 }
                        })
            }
        }.frame(minHeight: 230, maxHeight: .infinity).padding(.bottom, 12)
    }
    private func cardFace(_ card: AgentCard, compact: Bool) -> some View {
        VStack(alignment: .leading, spacing: compact ? 14 : 22) {
            HStack {
                Label(card.error == nil ? card.status : "Update unavailable", systemImage: card.isRunning ? "waveform" : card.status == "Failed" ? "exclamationmark.circle" : "checkmark.circle")
                    .font(.system(.caption, design: .monospaced).weight(.medium))
                    .foregroundStyle(card.isRunning || card.error != nil ? Ink.amber : Ink.accent)
                Spacer()
                Text("\(model.deck.order.firstIndex(of: card.id).map { $0 + 1 } ?? 1) / \(model.deck.order.count)")
                    .font(.system(.caption, design: .monospaced)).foregroundStyle(Ink.muted)
            }
            Text(card.title).font(.system(size: compact ? 24 : 29, weight: .semibold)).lineLimit(3).accessibilityIdentifier("agent-title")
            VStack(alignment: .leading, spacing: 10) {
                Text(card.isRunning ? "WORKING ON IT" : "LATEST UPDATE")
                    .font(.system(size: 10, weight: .medium, design: .monospaced)).tracking(1.5).foregroundStyle(Ink.muted)
                Text(card.error ?? card.preview).font(.system(size: 16)).foregroundStyle(.white.opacity(0.83))
                    .lineLimit(compact ? 3 : 7).lineSpacing(4)
            }
            Spacer(minLength: 0)
            HStack {
                Text(card.model.isEmpty ? "Managed agent" : card.model)
                    .font(.system(size: 11, design: .monospaced)).foregroundStyle(Ink.muted)
                Spacer()
                Button { showThread = true } label: {
                    Label("Open thread", systemImage: "arrow.up.right").font(.caption.weight(.medium)).padding(.vertical, 8)
                }.accessibilityIdentifier("open-thread")
            }
        }
        .padding(24).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Ink.card, in: RoundedRectangle(cornerRadius: 28))
        .overlay(RoundedRectangle(cornerRadius: 28).stroke(.white.opacity(0.08), lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityAction(named: "Next agent, revisit later") { advance(reviewed: false) }
        .accessibilityAction(named: "Mark update seen") { advance(reviewed: true) }
        .accessibilityIdentifier("agent-card")
    }
    private var actions: some View {
        HStack(spacing: 22) {
            Button { model.back() } label: { Image(systemName: "arrow.uturn.backward").frame(width: 44, height: 44) }
                .disabled(!model.canGoBack).accessibilityLabel("Previous agent").keyboardShortcut("[", modifiers: .command)
            actionButton("Later", icon: "arrow.right", color: Ink.amber) { advance(reviewed: false) }
                .accessibilityIdentifier("later").keyboardShortcut(.rightArrow, modifiers: .command)
            actionButton("Seen", icon: "checkmark", color: Ink.accent) { advance(reviewed: true) }
                .accessibilityIdentifier("seen").keyboardShortcut("d", modifiers: .command)
            Button { showThread = true } label: { Image(systemName: "text.bubble").frame(width: 44, height: 44) }
                .accessibilityLabel("Read conversation").keyboardShortcut("o", modifiers: .command)
        }.foregroundStyle(Ink.muted)
    }
    private func actionButton(_ label: String, icon: String, color: Color, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 7) {
                Image(systemName: icon).font(.system(size: 21, weight: .medium)).frame(width: 62, height: 52)
                    .background(color.opacity(0.1), in: RoundedRectangle(cornerRadius: 20))
                Text(label).font(.caption)
            }.foregroundStyle(color)
        }.buttonStyle(.plain)
    }
    private var composer: some View {
        VStack(spacing: 10) {
            if let card = model.focused, card.activeTurns.count > 1 {
                Picker("Active turn", selection: $model.selectedTurn) {
                    ForEach(card.activeTurns, id: \.self) { id in Text(String(id.prefix(12))).tag(id) }
                }.pickerStyle(.menu)
            }
            HStack(alignment: .bottom, spacing: 10) {
                TextField(model.focused?.isRunning == true && !sendFollowUp ? "Steer this agent…" : "Send a follow-up…", text: $model.draft, axis: .vertical)
                    .lineLimit(1...4).textFieldStyle(.plain).font(.body).focused($composerFocused)
                    .padding(.vertical, 12).accessibilityIdentifier("composer")
                Button { Task { await model.send(steer: model.focused?.isRunning == true && !sendFollowUp) } } label: {
                    Image(systemName: model.busy.contains(model.focused?.id ?? "") ? "ellipsis" : "arrow.up")
                        .font(.system(size: 18, weight: .semibold)).frame(width: 44, height: 44)
                        .background(Ink.accent, in: RoundedRectangle(cornerRadius: 14)).foregroundStyle(Ink.background)
                }.buttonStyle(.plain).disabled(model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.busy.contains(model.focused?.id ?? ""))
                    .accessibilityLabel(model.focused?.isRunning == true && !sendFollowUp ? "Send steering" : "Send follow-up")
                    .accessibilityIdentifier("send").keyboardShortcut(.return, modifiers: .command)
            }.padding(10).padding(.leading, 6).background(Ink.card, in: RoundedRectangle(cornerRadius: 23))
            HStack {
                if model.focused?.isRunning == true {
                    Menu {
                        Button("Steer current turn") { sendFollowUp = false }
                        Button("Queue a follow-up") { sendFollowUp = true }
                    } label: { Label(sendFollowUp ? "Follow-up" : "Steer live", systemImage: "slider.horizontal.3") }
                    Spacer()
                    Button("Stop turn") { if let card = model.focused { stopTarget = (card.id, model.focusedTurn, card.title); showStop = true } }.foregroundStyle(Ink.muted)
                        .disabled(model.busy.contains(model.focused?.id ?? ""))
                } else { Text("Give it a direction. Keep moving.").foregroundStyle(Ink.muted); Spacer() }
                if model.canRetry { Button("Retry follow-up") { Task { await model.retry() } } }
            }.font(.caption).padding(.horizontal, 8)
        }.padding(.horizontal, 24).padding(.top, 12).padding(.bottom, 16).background(Ink.background)
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
                        Text(card.title).foregroundStyle(.white)
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
                    Text("Swipe left: revisit later\nSwipe right: mark this update seen\nOpen thread: read the conversation\nSteer live: change the current direction")
                    Text("On Mac: ⌘→ next · ⌘[ back · ⌘D seen · ⌘O thread · ⌘Return send").font(.caption)
                }
            }.navigationTitle("Inbox settings").toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showSettings = false } } }
        }.frame(minWidth: 340, minHeight: 440).presentationDetents([.medium, .large])
    }
    private func advance(reviewed: Bool) {
        composerFocused = false
        #if os(iOS)
        UIImpactFeedbackGenerator(style: .soft).impactOccurred()
        #endif
        withAnimation(reduceMotion ? nil : .spring(response: 0.32, dampingFraction: 0.86)) { model.advance(reviewed: reviewed); drag = 0 }
    }
}

private struct ConnectView: View {
    @ObservedObject var model: InboxModel
    @State private var origin = "https://nanocodex.gakonst.workers.dev"
    @State private var key = ""
    @State private var connecting = false
    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            Image(systemName: "rectangle.stack").font(.system(size: 40, weight: .ultraLight)).foregroundStyle(Ink.accent)
            Text("A little direction.\nA lot in motion.").font(.system(size: 37, weight: .semibold, design: .rounded))
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
                LazyVStack(alignment: .leading, spacing: 24) {
                    if model.hasOlder { Button(model.loadingOlder ? "Loading…" : "Load earlier messages") { Task { await model.loadOlder() } }.disabled(model.loadingOlder) }
                    ForEach(model.rows) { row in
                        VStack(alignment: .leading, spacing: 8) {
                            HStack {
                                Text(row.role.uppercased()).font(.system(size: 10, weight: .semibold, design: .monospaced)).tracking(1)
                                if row.running { ProgressView().controlSize(.mini) }
                            }.foregroundStyle(row.role == "You" ? Ink.accent : Ink.muted)
                            Text(row.text).font(.body).textSelection(.enabled)
                            if !row.detail.isEmpty {
                                DisclosureGroup("Details") { Text(row.detail).font(.system(.caption, design: .monospaced)).textSelection(.enabled) }
                            }
                        }.frame(maxWidth: .infinity, alignment: .leading)
                    }
                    Color.clear.frame(height: 1).id("latest")
                }.padding(24)
            }
            .navigationTitle(model.focused?.title ?? "Conversation")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .safeAreaInset(edge: .bottom) {
                Button("Steer this agent") { dismiss() }.buttonStyle(.borderedProminent).padding()
            }
        }.frame(minWidth: 340, minHeight: 520).presentationDetents([.large])
    }
}
