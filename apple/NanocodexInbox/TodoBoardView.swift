import SwiftUI
import InboxCore
import NanocodexUI

/// A decision inbox, not a second prompt screen. Capture lives in the fixed bottom dock.
struct TodoBoardView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.scenePhase) private var scenePhase
    @State private var selectedDecision: TodoDecision?
    @State private var filter: Filter = .needsYou
    private enum Filter: String, CaseIterable { case needsYou = "Needs you", all = "All" }

    var body: some View {
        List {
            HStack {
                Text("Decisions").font(.system(size: 34, weight: .bold, design: .rounded))
                Spacer()
                if model.todoLoading { ProgressView() }
                else {
                    Button { Task { await model.refreshTodo() } } label: {
                        Image(systemName: "arrow.clockwise").frame(width: 44, height: 44)
                    }.accessibilityLabel("Refresh decisions").accessibilityIdentifier("todo-refresh")
                }
            }
            .padding(.top, 8)
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
            Section {
                let visible = model.todoDecisions.filter { filter == .all || $0.status == "needs_you" }
                if visible.isEmpty && !model.todoLoaded {
                    Text(model.todoError == nil ? "Loading decisions…" : "Decisions couldn't be loaded. Tap refresh.")
                        .font(.subheadline).foregroundStyle(.secondary)
                        .listRowSeparator(.hidden)
                } else if visible.isEmpty {
                    Label("Nothing needs your decision right now", systemImage: "checkmark.circle")
                        .foregroundStyle(.secondary)
                        .listRowSeparator(.hidden)
                        .accessibilityIdentifier("todo-no-decisions")
                } else {
                    ForEach(visible) { decision in
                        decisionCard(decision)
                            .listRowInsets(EdgeInsets(top: 6, leading: 18, bottom: 6, trailing: 18))
                            .listRowSeparator(.hidden)
                            .listRowBackground(Color.clear)
                            .swipeActions(edge: .leading, allowsFullSwipe: false) {
                                if decision.status == "needs_you", let choice = decision.choices.first {
                                    Button {
                                        Task { _ = await model.respondTodo(to: decision, choiceID: choice.id, text: nil) }
                                    } label: { Label(choice.title, systemImage: "checkmark") }
                                        .tint(.green).disabled(model.todoResponding)
                                        .accessibilityIdentifier("decision-swipe-primary:\(decision.id)")
                                }
                            }
                            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                                if decision.status == "needs_you", decision.choices.count > 1 {
                                    let choice = decision.choices[1]
                                    Button {
                                        Task { _ = await model.respondTodo(to: decision, choiceID: choice.id, text: nil) }
                                    } label: { Label(choice.title, systemImage: "arrow.uturn.backward") }
                                        .tint(.orange).disabled(model.todoResponding)
                                        .accessibilityIdentifier("decision-swipe-secondary:\(decision.id)")
                                }
                            }
                    }
                }
            } header: {
                HStack {
                    Text("Needs you")
                    if model.pendingTodoDecisionCount > 0 {
                        Text("\(model.pendingTodoDecisionCount)")
                            .padding(.horizontal, 7).padding(.vertical, 2)
                            .background(.primary.opacity(0.08), in: Capsule())
                    }
                    Spacer()
                    Menu {
                        ForEach(Filter.allCases, id: \.self) { option in
                            Button {
                                filter = option
                            } label: {
                                if filter == option { Label(option.rawValue, systemImage: "checkmark") }
                                else { Text(option.rawValue) }
                            }
                        }
                    } label: {
                        Image(systemName: "line.3.horizontal.decrease")
                            .frame(width: 44, height: 36)
                    }
                    .accessibilityLabel("Filter decisions")
                    .accessibilityValue(filter.rawValue)
                    .accessibilityIdentifier("todo-filter")
                }
                .font(.subheadline.weight(.semibold)).textCase(nil)
            } footer: {
                if model.pendingTodoDecisionCount > 0 {
                    Text("Swipe for quick choices · Tap to review or edit")
                        .font(.caption).textCase(nil)
                }
            }
            if !model.todoItems.isEmpty {
                Section("Captured") {
                    ForEach(model.todoItems) { item in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(item.body).font(.body)
                            if !item.watchHint.isEmpty {
                                Text("Watch for: " + item.watchHint).font(.caption).foregroundStyle(.secondary)
                            }
                            Text(item.status == "watching" ? "Watching" : item.status.capitalized)
                                .font(.caption2).foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 3)
                    }
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(Color(uiColor: .systemBackground))
        .refreshable { await model.refreshTodo() }
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                await model.refreshTodo()
                do { try await Task.sleep(for: .seconds(15)) }
                catch { return }
            }
        }
        .sheet(item: $selectedDecision) { decision in
            DecisionDetailView(decision: decision, model: model)
                .presentationDragIndicator(.visible)
                .presentationCornerRadius(28)
        }
    }

    private func decisionCard(_ decision: TodoDecision) -> some View {
        Button { selectedDecision = decision } label: {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text(decision.sourceLabel.isEmpty ? "Decision" : decision.sourceLabel)
                        .font(.caption.weight(.medium)).foregroundStyle(.secondary)
                    Spacer()
                    if decision.status == "needs_you" {
                        Circle().fill(.orange).frame(width: 7, height: 7)
                    } else { Text(decision.status.capitalized).font(.caption).foregroundStyle(.secondary) }
                }
                Text(decision.title).font(.headline).multilineTextAlignment(.leading)
                Text(decision.context).font(.subheadline).foregroundStyle(.secondary)
                    .multilineTextAlignment(.leading).lineLimit(2)
            }
            .padding(15).frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 18))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("decision-card:" + decision.id)
    }
}

/// Composer and tab switch share one bottom safe-area dock, so neither overlays the other.
struct TodoCaptureComposer: View {
    @ObservedObject var model: InboxModel
    @Binding var inputFocused: Bool
    @State private var captureFocused = false
    @State private var overflowing = false
    @FocusState private var hintFocused: Bool
    @State private var hintExpanded = false

    private var canSave: Bool {
        !model.todoDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && model.todoDraft.utf8.count <= 4096 && !model.todoSaving
    }

    var body: some View {
        VStack(spacing: 0) {
            if let error = model.todoError {
                Text(error).font(.caption).foregroundStyle(.orange)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16).padding(.top, 10)
            }
            HStack(spacing: 10) {
                if hintExpanded || !model.todoWatchHint.isEmpty {
                    TextField("Watch for…", text: $model.todoWatchHint)
                        .focused($hintFocused).font(.subheadline)
                        .accessibilityIdentifier("todo-watch-hint")
                } else { Spacer(minLength: 0) }
                Button { hintExpanded.toggle(); if hintExpanded { hintFocused = true } } label: {
                    Label("Watch for", systemImage: hintExpanded ? "eye.fill" : "eye")
                        .font(.caption).frame(minHeight: 44)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("todo-watch-toggle")
            }
            .padding(.horizontal, 16).frame(minHeight: 44)
            HStack(alignment: .bottom, spacing: 2) {
                Image(systemName: "square.and.pencil")
                    .frame(width: 44, height: 44).accessibilityHidden(true)
                ChatComposerEditor(text: $model.todoDraft, focused: $captureFocused,
                                   overflowing: $overflowing, accessibilityLabel: "On your mind")
                    .accessibilityIdentifier("todo-capture")
                    .overlay(alignment: .topLeading) {
                        if model.todoDraft.isEmpty {
                            Text("On your mind…").font(.body).foregroundStyle(.tertiary)
                                .padding(.top, 8).allowsHitTesting(false).accessibilityHidden(true)
                        }
                    }
                Button {
                    captureFocused = false; hintFocused = false
                    Task { await model.saveTodo() }
                } label: {
                    Group {
                        if model.todoSaving { ProgressView().tint(Color(uiColor: .systemBackground)) }
                        else { Image(systemName: "arrow.up") }
                    }
                    .font(.system(size: 16, weight: .semibold))
                    .frame(width: 32, height: 32)
                    .background(Color.primary.opacity(canSave ? 1 : 0.22), in: Circle())
                    .foregroundStyle(Color(uiColor: .systemBackground))
                    .frame(width: 44, height: 44).contentShape(Rectangle())
                }
                .buttonStyle(.plain).disabled(!canSave)
                .accessibilityLabel("Save thought").accessibilityIdentifier("todo-capture-save")
            }
            .padding(.horizontal, 4).padding(.bottom, 4).padding(.top, 4)
        }
        .modifier(InboxComposerShell(focused: inputFocused))
        .frame(maxWidth: 620)
        .onChange(of: captureFocused) { _, _ in inputFocused = captureFocused || hintFocused }
        .onChange(of: hintFocused) { _, _ in inputFocused = captureFocused || hintFocused }
        .onDisappear { inputFocused = false }
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
                        Text("Edit or give instructions").font(.headline).padding(.top, 10)
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
