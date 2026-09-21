import SwiftUI

/// Shared schedule management form for the iPhone, iPad, and Mac apps.
public struct ScheduledJobEditor: View {
    public let job: ScheduledJob
    private let save: (String, String, String, Bool, Bool) async throws -> Void
    private let cancel: () async throws -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var cron: String
    @State private var timezone: String
    @State private var input: String
    @State private var enabled: Bool
    @State private var startsNew: Bool
    @State private var busy = false
    @State private var confirmCancellation = false
    @State private var error: String?

    public init(job: ScheduledJob,
                save: @escaping (String, String, String, Bool, Bool) async throws -> Void,
                cancel: @escaping () async throws -> Void) {
        self.job = job; self.save = save; self.cancel = cancel
        _cron = State(initialValue: job.cron); _timezone = State(initialValue: job.timezone)
        _input = State(initialValue: job.input); _enabled = State(initialValue: job.enabled)
        _startsNew = State(initialValue: job.startsNewConversation)
    }

    public var body: some View {
        NavigationStack {
            Form {
                Section("Prompt") {
                    TextEditor(text: $input).frame(minHeight: 100).accessibilityLabel("Scheduled prompt")
                }
                Section("Schedule") {
                    TextField("Cron (minute hour day month weekday)", text: $cron)
                    Text("Example: 0 9 * * * runs every day at 9 AM.").font(.caption).foregroundStyle(.secondary)
                    TextField("Time zone", text: $timezone)
                    Toggle("Active", isOn: $enabled)
                    Toggle("New conversation for each run", isOn: $startsNew)
                }
                Section {
                    Button("Cancel scheduled job", role: .destructive) { confirmCancellation = true }
                        .accessibilityIdentifier("scheduled-job-cancel")
                } footer: {
                    Text("Cancellation stops future scheduling. Runs already dispatched or running are not stopped. Existing conversations are kept.")
                }
                if let error { Section { Text(error).foregroundStyle(.red).accessibilityLabel("Schedule error: " + error) } }
                if busy { ProgressView("Saving changes") }
            }
            .navigationTitle("Edit scheduled job")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() }.disabled(busy) }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { perform { try await save(cron, timezone, input, enabled, startsNew) } }
                        .disabled(busy || input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || cron.split(whereSeparator: \.isWhitespace).count != 5 || TimeZone(identifier: timezone) == nil)
                        .accessibilityIdentifier("scheduled-job-save")
                }
            }
            .disabled(busy)
            .confirmationDialog("Cancel this scheduled job?", isPresented: $confirmCancellation, titleVisibility: .visible) {
                Button("Cancel scheduled job", role: .destructive) { perform(cancel) }
                Button("Keep job", role: .cancel) { }
            } message: { Text("Future scheduling will stop. Runs already dispatched or running are not stopped. This cannot be undone.") }
        }
        .interactiveDismissDisabled(busy)
        #if os(macOS)
        .frame(minWidth: 480, minHeight: 520)
        #endif
    }

    private func perform(_ operation: @escaping () async throws -> Void) {
        busy = true; error = nil
        Task { @MainActor in
            defer { busy = false }
            do { try await operation(); dismiss() }
            catch { self.error = "Couldn’t save the change. \(error.localizedDescription) Close and refresh to check the current job before retrying." }
        }
    }
}
