import SwiftUI
import InboxCore

/// Human decisions first; conversational prompting remains in the adjacent Chat tab.
struct TodoBoardView: View {
    @ObservedObject var model: InboxModel
    @Binding var inputFocused: Bool
    @Environment(\.scenePhase) private var scenePhase
    @FocusState private var captureFocused: Bool
    @FocusState private var hintFocused: Bool
    @State private var selectedDecision: TodoDecision?
    @State private var filter: Filter = .needsYou
    private enum Filter: String, CaseIterable { case needsYou = "Needs you", all = "All" }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text("YOUR WORKSPACE").font(.caption2.weight(.semibold)).tracking(1.6)
                            .foregroundStyle(.secondary)
                        Text("Decisions").font(.system(size: 35, weight: .bold, design: .rounded))
                        Text("Capture what matters. Decide when it matters.")
                            .font(.subheadline).foregroundStyle(.secondary)
                    }
                    Spacer()
                    if model.todoLoading { ProgressView().padding(.top, 10) }
                    else {
                        Button { Task { await model.refreshTodo() } } label: {
                            Image(systemName: "arrow.clockwise").frame(width: 44, height: 44)
                        }.accessibilityLabel("Refresh decisions").accessibilityIdentifier("todo-refresh")
                    }
                }
                captureCard
                if let error = model.todoError {
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: "exclamationmark.circle")
                        Text(error).font(.subheadline)
                        Spacer()
                        Button("Retry") { Task { await model.refreshTodo() } }
                    }
                    .foregroundStyle(.orange)
                    .padding(14).background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 16))
                    .accessibilityIdentifier("todo-error")
                }
                HStack {
                    Text("Needs you").font(.title3.weight(.bold))
                    if model.pendingTodoDecisionCount > 0 {
                        Text("\(model.pendingTodoDecisionCount)").font(.caption.weight(.bold))
                            .padding(.horizontal, 8).padding(.vertical, 4)
                            .background(.primary.opacity(0.09), in: Capsule())
                    }
                    Spacer()
                    Picker("View", selection: $filter) {
                        ForEach(Filter.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                    }.pickerStyle(.menu).accessibilityIdentifier("todo-filter")
                }
                let visible = model.todoDecisions.filter { filter == .all || $0.status == "needs_you" }
                if visible.isEmpty && !model.todoLoaded {
                    Text(model.todoError == nil ? "Loading decisions…" : "Decisions couldn't be loaded. Retry above.")
                        .font(.subheadline).foregroundStyle(.secondary)
                        .padding(18).frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 22))
                } else if visible.isEmpty {
                    VStack(alignment: .leading, spacing: 5) {
                        Image(systemName: "checkmark.circle").font(.title2)
                        Text("Nothing needs your decision right now").font(.headline)
                        Text("When a workflow needs a choice, it will appear here with its context.")
                            .font(.subheadline).foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(18).background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 22))
                    .accessibilityIdentifier("todo-no-decisions")
                } else {
                    LazyVStack(spacing: 12) {
                        ForEach(visible) { decision in
                            decisionCard(decision)
                        }
                    }
                }
                if !model.todoItems.isEmpty {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("On my mind").font(.title3.weight(.bold))
                        ForEach(model.todoItems) { item in
                            HStack(alignment: .top, spacing: 12) {
                                Image(systemName: item.status == "watching" ? "waveform.path" : "square.and.pencil")
                                    .foregroundStyle(.secondary).frame(width: 22)
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(item.body).font(.body)
                                    if !item.watchHint.isEmpty { Text("Watch for: " + item.watchHint).font(.caption).foregroundStyle(.secondary) }
                                    Text(item.status == "watching" ? "Watching for signals" : item.status.capitalized)
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer(minLength: 0)
                            }
                            .padding(15).frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 17))
                        }
                    }
                }
            }
            .frame(maxWidth: 620)
            .padding(.horizontal, 18).padding(.top, 20).padding(.bottom, 26)
            .frame(maxWidth: .infinity)
        }
        .background(Color(uiColor: .systemBackground))
        .scrollDismissesKeyboard(.interactively)
        .refreshable { await model.refreshTodo() }
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                await model.refreshTodo()
                do { try await Task.sleep(for: .seconds(15)) }
                catch { return }
            }
        }
        .onChange(of: captureFocused) { _, _ in inputFocused = captureFocused || hintFocused }
        .onChange(of: hintFocused) { _, _ in inputFocused = captureFocused || hintFocused }
        .sheet(item: $selectedDecision) { decision in
            DecisionDetailView(decision: decision, model: model)
                .presentationDragIndicator(.visible)
                .presentationCornerRadius(28)
        }
        .accessibilityIdentifier("todo-board")
    }

    private var captureCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("On your mind?", systemImage: "square.and.pencil")
                .font(.headline)
            TextEditor(text: $model.todoDraft)
                .focused($captureFocused)
                .frame(minHeight: 58, maxHeight: 115)
                .scrollContentBackground(.hidden)
                .accessibilityLabel("Capture a thought")
                .accessibilityIdentifier("todo-capture")
                .overlay(alignment: .topLeading) {
                    if model.todoDraft.isEmpty {
                        Text("An idea, open loop, or something to watch…")
                            .foregroundStyle(.tertiary).padding(.top, 8).padding(.leading, 5)
                            .allowsHitTesting(false)
                    }
                }
            if captureFocused || !model.todoDraft.isEmpty {
                TextField("What should we watch for? (optional)", text: $model.todoWatchHint)
                    .focused($hintFocused)
                    .font(.subheadline).textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("todo-watch-hint")
            }
            HStack {
                Text("Saved thoughts can become decisions; they don't authorize actions.")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer(minLength: 8)
                Button {
                    captureFocused = false
                    hintFocused = false
                    Task { await model.saveTodo() }
                } label: {
                    if model.todoSaving { ProgressView().tint(.white) }
                    else { Text("Capture") }
                }
                .buttonStyle(.borderedProminent)
                .disabled(model.todoDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    || model.todoDraft.utf8.count > 4096 || model.todoSaving)
                .accessibilityIdentifier("todo-capture-save")
            }
        }
        .padding(16)
        .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 22))
    }

    private func decisionCard(_ decision: TodoDecision) -> some View {
        Button { selectedDecision = decision } label: {
            VStack(alignment: .leading, spacing: 11) {
                HStack {
                    Label(decision.sourceLabel.isEmpty ? "Decision" : decision.sourceLabel, systemImage: "sparkle")
                        .font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    Spacer()
                    Text(decision.status == "answered" ? "Answer recorded" : decision.status == "needs_you" ? "Needs you" : decision.status.capitalized)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(decision.status == "needs_you" ? Color.orange : Color.secondary)
                }
                Text(decision.title).font(.headline).multilineTextAlignment(.leading)
                Text(decision.context).font(.subheadline).foregroundStyle(.secondary)
                    .multilineTextAlignment(.leading).lineLimit(3)
                HStack {
                    Text(decision.choices.first?.title ?? "Review")
                        .font(.subheadline.weight(.semibold))
                    Image(systemName: "arrow.up.right").font(.caption.weight(.bold))
                    Spacer()
                    Text("Review context").font(.caption).foregroundStyle(.secondary)
                }.padding(.top, 2)
            }
            .padding(16).frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 22))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("decision-card:" + decision.id)
    }
}

private struct DecisionDetailView: View {
    let decision: TodoDecision
    @ObservedObject var model: InboxModel
    @Environment(\.dismiss) private var dismiss
    @State private var instructions = ""

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    Label(decision.sourceLabel.isEmpty ? "Decision" : decision.sourceLabel, systemImage: "sparkle")
                        .font(.subheadline).foregroundStyle(.secondary)
                    Text(decision.title).font(.largeTitle.weight(.bold))
                    Text(decision.context).font(.body)
                    if let todoID = decision.todoID,
                       let thought = model.todoItems.first(where: { $0.id == todoID }) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("From your capture").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                            Text(thought.body).font(.subheadline)
                        }
                        .padding(14)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 15))
                    }
                    if let url = decision.sourceURL {
                        Link(destination: url) {
                            Label("Open source", systemImage: "arrow.up.right.square")
                        }.accessibilityIdentifier("decision-source")
                    }
                    if decision.status == "needs_you" {
                        Text("Choose what happens next").font(.headline).padding(.top, 10)
                        ForEach(decision.choices) { choice in
                            Button {
                                Task {
                                    if await model.respondTodo(to: decision, choiceID: choice.id, text: nil) { dismiss() }
                                }
                            } label: {
                                HStack {
                                    Text(choice.title)
                                    Spacer()
                                    Image(systemName: "arrow.right")
                                }
                                .padding(16)
                                .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 15))
                            }
                            .buttonStyle(.plain).disabled(model.todoResponding)
                            .accessibilityIdentifier("decision-choice:\(decision.id):\(choice.id)")
                        }
                        Text("Or tell the agent what you want").font(.headline).padding(.top, 10)
                        TextEditor(text: $instructions)
                            .frame(minHeight: 100).padding(8)
                            .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 15))
                            .accessibilityIdentifier("decision-instructions")
                        Button("Submit instructions") {
                            Task {
                                if await model.respondTodo(to: decision, choiceID: nil,
                                                           text: instructions.trimmingCharacters(in: .whitespacesAndNewlines)) { dismiss() }
                            }
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(instructions.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.todoResponding)
                        if let error = model.todoError { Text(error).font(.caption).foregroundStyle(.orange) }
                    } else {
                        Label(decision.status == "answered" ? "Answer recorded; waiting for the workflow" : "This decision is no longer open",
                              systemImage: "checkmark.circle")
                            .font(.subheadline).foregroundStyle(.secondary)
                    }
                    Text("A choice applies only to this decision; it never grants blanket permission for future actions.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                .padding(22).frame(maxWidth: 620, alignment: .leading).frame(maxWidth: .infinity)
            }
            .navigationTitle("Decision").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) {
                Button("Done") { dismiss() }.accessibilityIdentifier("decision-detail-close")
            } }
        }
    }
}
