import SwiftUI
import InboxCore

struct ScheduledJobsView: View {
    @ObservedObject var model: InboxModel
    var openChat: () -> Void
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        List {
            Section {
                Text("Create new scheduled jobs by asking an agent in chat.")
                    .font(.subheadline).foregroundStyle(.secondary)
                if model.isDemo { Text("Sample jobs · Demo").font(.caption).foregroundStyle(.secondary) }
            }
            if let error = model.schedulesError {
                Section {
                    Label("Some jobs may be missing or out of date", systemImage: "exclamationmark.circle")
                    Text(error).font(.caption).foregroundStyle(.secondary)
                    Button("Retry") { Task { await model.refreshScheduledJobs() } }
                        .disabled(model.schedulesLoading)
                }
            }
            if !model.schedulesLoaded && model.scheduledJobs.isEmpty && model.schedulesError == nil {
                ProgressView().accessibilityLabel("Loading scheduled jobs")
            } else if model.schedulesLoaded && model.scheduledJobs.isEmpty && model.schedulesError == nil {
                ContentUnavailableView("No scheduled jobs yet", systemImage: "clock",
                    description: Text("Ask an agent to run a task on a schedule. It will appear here."))
            }
            jobSection("Active", enabled: true)
            jobSection("Paused", enabled: false)
        }
        .navigationTitle("Scheduled jobs")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { Task { await model.refreshScheduledJobs() } } label: {
                    if model.schedulesLoading { ProgressView() }
                    else { Image(systemName: "arrow.clockwise") }
                }.disabled(model.schedulesLoading).accessibilityLabel("Refresh scheduled jobs")
            }
        }
        .refreshable { await model.refreshScheduledJobs() }
        .task { await model.refreshScheduledJobs() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await model.refreshScheduledJobs() } }
        }
        .accessibilityIdentifier("scheduled-jobs")
    }

    @ViewBuilder private func jobSection(_ title: String, enabled: Bool) -> some View {
        let jobs = model.scheduledJobs.filter { $0.enabled == enabled }
        if !jobs.isEmpty {
            Section(title) {
                ForEach(jobs) { job in
                    NavigationLink {
                        ScheduledJobDetailView(model: model, jobID: job.id, openChat: openChat)
                    } label: {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(job.triggerID).font(.headline)
                            Text(job.input).font(.subheadline).lineLimit(2)
                            Text(model.scheduledJobAgents[job.agentID] ?? "Agent").font(.caption).foregroundStyle(.secondary)
                            if job.enabled, let next = job.nextRun {
                                Text("Next: " + job.formatted(next)).font(.caption).foregroundStyle(.secondary)
                            } else if !job.enabled {
                                Label("Paused", systemImage: "pause.circle").font(.caption).foregroundStyle(.secondary)
                            }
                        }.padding(.vertical, 4)
                    }.accessibilityIdentifier("scheduled-job-" + job.id)
                }
            }
        }
    }
}

private struct ScheduledJobDetailView: View {
    @ObservedObject var model: InboxModel
    let jobID: String
    var openChat: () -> Void
    @State private var opening = false
    @State private var openingTask: Task<Void, Never>?
    @State private var error: String?

    var body: some View {
        Group {
            if let job = model.scheduledJobs.first(where: { $0.id == jobID }) {
                List {
                    Section("Prompt") {
                        Text(job.input).textSelection(.enabled)
                    }
                    Section("Schedule") {
                        LabeledContent("Status", value: job.enabled ? "Active" : "Paused")
                        LabeledContent("Cron") { Text(job.cron).monospaced().textSelection(.enabled) }
                        LabeledContent("Time zone", value: job.timezone)
                        LabeledContent("Next run", value: job.enabled ? job.nextRun.map(job.formatted) ?? "Not scheduled" : "Paused")
                        LabeledContent("Conversation", value: job.startsNewConversation ? "New for each run" : "Continue source chat")
                    }
                    Section {
                        LabeledContent("Last dispatched", value: job.lastRun.map(job.formatted) ?? "Not yet")
                        if let skipped = job.lastSkipped {
                            LabeledContent("Last skipped", value: job.formatted(skipped))
                        }
                        if let id = job.lastRunAgentID {
                            Button("Open latest run", systemImage: "bubble.left.and.bubble.right") { navigate(id) }
                                .accessibilityIdentifier("scheduled-job-latest-run")
                        }
                    } header: { Text("Activity") } footer: {
                        Text(job.startsNewConversation
                            ? "Each run starts a new conversation. Open the latest run to see its progress and result."
                            : "Runs continue the source chat. An occurrence is skipped if that conversation is busy.")
                    }
                    Section("Source") {
                        Text(model.scheduledJobAgents[job.agentID] ?? "Agent").foregroundStyle(.secondary)
                        Button("Open source chat", systemImage: "bubble.left") { navigate(job.agentID) }
                            .accessibilityIdentifier("scheduled-job-source-chat")
                    }
                    if let error { Section { Text(error).foregroundStyle(.secondary) } }
                    if opening { ProgressView().accessibilityLabel("Opening conversation") }
                }
                .navigationTitle(job.triggerID)
                .disabled(opening)
                .accessibilityIdentifier("scheduled-job-detail")
            } else {
                ContentUnavailableView("Job unavailable", systemImage: "clock.badge.exclamationmark",
                    description: Text("Return to Scheduled jobs and refresh to check again."))
            }
        }
        .onDisappear { openingTask?.cancel(); openingTask = nil; opening = false }
    }

    private func navigate(_ id: String) {
        opening = true; error = nil
        openingTask = Task {
            defer { opening = false }
            do { try await model.selectScheduledChat(id); try Task.checkCancellation(); openChat() }
            catch is CancellationError { }
            catch { self.error = error.localizedDescription }
        }
    }
}

private extension ScheduledJob {
    func formatted(_ date: Date) -> String {
        var style: Date.FormatStyle = .dateTime.month(.abbreviated).day().hour().minute().timeZone(.specificName(.short))
        style.timeZone = TimeZone(identifier: timezone) ?? .gmt
        return date.formatted(style)
    }
}
