import AVFoundation
import InboxCore
import NanocodexUI
import SwiftUI

/// A first-class account surface, not a list of incidental chat threads.
struct MeetingsHomeView: View {
    @ObservedObject var model: InboxModel
    let openCapture: () -> Void
    var body: some View {
        if let library = model.meetingLibrary {
            MeetingsLibraryList(model: model, library: library, openCapture: openCapture)
        } else {
            ContentUnavailableView("Meeting storage unavailable", systemImage: "externaldrive.badge.exclamationmark",
                description: Text("The app could not open its local meeting journal. Restart the app before recording."))
        }
    }
}

private struct MeetingsLibraryList: View {
    @ObservedObject var model: InboxModel
    @ObservedObject var library: MeetingLibrary
    @ObservedObject private var recorder = MeetingRecorder.shared
    @Environment(\.scenePhase) private var scenePhase
    let openCapture: () -> Void
    @State private var query = ""
    private var rows: [MeetingRecordingStore.Entry] {
        library.entries.filter { $0.state != .capturing && (query.isEmpty || $0.record.title.localizedCaseInsensitiveContains(query)) }
    }
    private var activeCapture: Bool {
        recorder.working && recorder.accountScope == library.scope
    }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 18) {
                HStack(alignment: .center) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text("Meetings").font(.system(.largeTitle, design: .rounded, weight: .bold))
                        Text("Your conversations, worth keeping.").font(.subheadline).foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 8)
                    Button(action: openCapture) {
                        Image(systemName: activeCapture ? "waveform" : "plus").font(.title3.weight(.semibold)).frame(width: 44, height: 44)
                    }.buttonStyle(.borderedProminent).buttonBorderShape(.circle)
                        .foregroundStyle(Color(uiColor: .systemBackground))
                        .accessibilityLabel(activeCapture ? "Open recording" : "New meeting").accessibilityIdentifier("meeting-new")
                }.padding(.top, 12)
                if activeCapture {
                    Button(action: openCapture) {
                        HStack(spacing: 12) {
                            Image(systemName: "waveform").foregroundStyle(.red)
                            VStack(alignment: .leading, spacing: 4) {
                                Text(recorder.recording ? "Meeting in progress" : "Finishing transcript…").font(.headline)
                                Text("\(Duration.seconds(recorder.seconds).formatted()) · Tap to return").font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        }.padding(16).background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 20))
                    }.buttonStyle(.plain).accessibilityIdentifier("meeting-active")
                }
                HStack(spacing: 10) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("Search meeting titles", text: $query).textInputAutocapitalization(.never)
                        .autocorrectionDisabled().accessibilityIdentifier("meetings-search")
                    if !query.isEmpty { Button { query = "" } label: { Image(systemName: "xmark.circle.fill") }.accessibilityLabel("Clear search") }
                }.padding(14).background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 16))
                if let error = library.error {
                    VStack(alignment: .leading, spacing: 10) {
                        Label("Couldn’t sync meetings", systemImage: "wifi.exclamationmark").font(.headline)
                        Text(error).font(.caption).foregroundStyle(.secondary)
                        Text(library.entries.isEmpty ? "Try again when your connection is available." : "Saved on this device. Pending meetings retry with the same recording ID.").font(.caption).foregroundStyle(.secondary)
                        Button("Retry") { Task { await library.refresh(); await library.retry() } }.buttonStyle(.bordered)
                            .accessibilityIdentifier("meetings-retry")
                    }.padding(16).frame(maxWidth: .infinity, alignment: .leading)
                        .background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 16))
                }
                if let audioError = library.audioError {
                    VStack(alignment: .leading, spacing: 10) {
                        Label("Recording audio has not synced", systemImage: "exclamationmark.icloud").font(.headline)
                        Text(audioError).font(.caption).foregroundStyle(.secondary)
                        Button("Retry audio sync") { Task { await library.retry() } }.buttonStyle(.bordered)
                            .accessibilityIdentifier("meetings-audio-retry")
                    }.padding(16).frame(maxWidth: .infinity, alignment: .leading)
                        .background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 16))
                        .accessibilityIdentifier("meetings-audio-error")
                }
                if library.loading && rows.isEmpty {
                    ProgressView("Loading meetings…").frame(maxWidth: .infinity).padding(40)
                } else if rows.isEmpty {
                    ContentUnavailableView(query.isEmpty ? "Room for your next conversation" : "No matching meetings",
                        systemImage: query.isEmpty ? "text.bubble" : "magnifyingglass",
                        description: Text(query.isEmpty ? "Record a meeting or jot down notes. The transcript and notes will be here on your iPhone and Mac." : "Try another meeting title."))
                        .accessibilityIdentifier("meetings-empty")
                } else {
                    Text("\(rows.count) saved \(rows.count == 1 ? "meeting" : "meetings")").font(.caption.weight(.medium)).foregroundStyle(.secondary)
                    VStack(spacing: 0) {
                        ForEach(rows) { entry in
                            NavigationLink {
                                MeetingDocumentView(model: model, library: library, id: entry.id)
                            } label: { row(entry) }
                            .buttonStyle(.plain).accessibilityIdentifier("meeting-row-" + entry.id.uuidString.lowercased())
                            if entry.id != rows.last?.id { Divider().padding(.leading, 62) }
                        }
                    }.background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 20))
                }
                if library.nextCursor != nil {
                    Button(library.loading ? "Loading…" : "Load more meetings") { Task { await library.refresh(loadMore: true) } }
                        .buttonStyle(.bordered).disabled(library.loading).frame(maxWidth: .infinity)
                }
                Text("Original recordings are retained for playback and transcription recovery. Audio syncs separately from transcripts and notes.")
                    .font(.footnote).foregroundStyle(.secondary).frame(maxWidth: .infinity).padding(.top, 10)
            }.padding(.horizontal, 18).padding(.bottom, 24).frame(maxWidth: 620).frame(maxWidth: .infinity)
        }
        .background(ChatPalette.background).scrollDismissesKeyboard(.interactively)
        .task(id: model.screenScope) { await library.refresh() }
        .refreshable { await library.refresh() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { library.reloadLocal(); Task { await library.refresh() } }
        }
        .accessibilityIdentifier("meetings-library")
    }
    private func row(_ entry: MeetingRecordingStore.Entry) -> some View {
        HStack(spacing: 14) {
            Image(systemName: "text.bubble").font(.system(size: 19, weight: .medium))
                .frame(width: 34, height: 42).foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 6) {
                Text(entry.record.title.isEmpty ? "Untitled meeting" : entry.record.title).font(.body.weight(.semibold)).foregroundStyle(.primary).lineLimit(2)
                Text(entry.record.startedAt.formatted(date: .abbreviated, time: .shortened)).font(.caption).foregroundStyle(.secondary)
                HStack(spacing: 8) {
                    if entry.record.durationSeconds > 0 { Text(Duration.seconds(entry.record.durationSeconds).formatted()) }
                    if entry.record.partial { Label("Partial transcript", systemImage: "exclamationmark.circle") }
                    if entry.state == .pending { Label("Saved on device · sync pending", systemImage: "arrow.triangle.2.circlepath") }
                    if entry.state == .conflicted { Label("Changed on another device · needs review", systemImage: "exclamationmark.circle") }
                }.font(.caption2).foregroundStyle(.secondary)
            }
            Spacer(minLength: 6)
            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
        }.padding(14).frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
    }
}

private struct MeetingDocumentView: View {
    @ObservedObject var model: InboxModel
    @ObservedObject var library: MeetingLibrary
    let id: UUID
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var record: MeetingRecord?
    @State private var title = ""
    @State private var notes = ""
    @State private var transcript = ""
    @State private var selectedTab = "Notes"
    @State private var busy = false
    @State private var error: String?
    @State private var confirmDelete = false
    @State private var confirmConflict = false
    @State private var pinnedScope: String?
    @State private var targetID: String?
    @State private var question = ""
    @State private var accountGeneration: UUID?
    @State private var replacementTranscript: String?
    @State private var confirmTranscriptReplacement = false
    @FocusState private var focusedField: String?
    private var hasChanges: Bool { record.map { title != $0.title || notes != $0.notes || transcript != $0.transcript } ?? false }
    private var shareText: String {
        guard let record else { return "" }
        return "# \(title)\n\n\(record.summary)\n\n## My notes\n\(notes)\n\n## Transcript\n\(transcript)"
    }
    private var documentContent: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if let record, scenePhase == .active {
                    TextField("Meeting title", text: $title, axis: .vertical).font(.system(.title, design: .rounded, weight: .bold))
                        .accessibilityIdentifier("meeting-document-title").focused($focusedField, equals: "title").disabled(busy)
                    Text(record.startedAt.formatted(date: .abbreviated, time: .shortened) + (record.durationSeconds > 0 ? " · " + Duration.seconds(record.durationSeconds).formatted() : ""))
                        .font(.caption).foregroundStyle(.secondary)
                    if record.partial {
                        Label("Partial transcript — review for missing words.", systemImage: "exclamationmark.circle")
                            .font(.subheadline).foregroundStyle(.secondary).accessibilityIdentifier("meeting-partial-warning")
                    }
                    if library.entries.first(where: { $0.id == id })?.state == .pending {
                        Label("Saved on this device · sync pending", systemImage: "arrow.triangle.2.circlepath").font(.caption).foregroundStyle(.secondary)
                    }
                    if library.entries.first(where: { $0.id == id })?.state == .conflicted {
                        VStack(alignment: .leading, spacing: 10) {
                            Label("Changed on another device", systemImage: "exclamationmark.arrow.triangle.2.circlepath").font(.headline)
                            Text("Your local edits are preserved. Choose which version to keep; nothing will overwrite the other version automatically.")
                                .font(.subheadline).foregroundStyle(.secondary)
                            Button("Resolve conflict") { confirmConflict = true }.buttonStyle(.bordered)
                                .accessibilityIdentifier("meeting-resolve-conflict")
                        }.padding(16).background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 18))
                    }
                    Picker("Meeting content", selection: $selectedTab) { Text("Notes").tag("Notes"); Text("Transcript").tag("Transcript") }
                        .pickerStyle(.segmented).accessibilityIdentifier("meeting-document-tabs")
                    if selectedTab == "Notes" {
                        VStack(alignment: .leading, spacing: 12) {
                            HStack {
                                Label("Enhanced notes", systemImage: "sparkles").font(.headline)
                                Spacer()
                                if busy { ProgressView().controlSize(.small) }
                            }
                            if record.summary.isEmpty {
                                Text("Turn your transcript and notes into key points, decisions, and next steps.").font(.subheadline).foregroundStyle(.secondary)
                            } else {
                                ChatMarkdown(text: record.summary).textSelection(.enabled).accessibilityIdentifier("meeting-enhanced-notes")
                                Text("AI-generated · check against the transcript").font(.caption).foregroundStyle(.secondary)
                            }
                            Button(record.summaryStatus == .ready ? "Refresh enhanced notes" : "Enhance notes") { Task { await enhance() } }
                                .buttonStyle(.bordered).disabled(busy || hasChanges || (record.transcript.isEmpty && record.notes.isEmpty))
                                .accessibilityIdentifier("meeting-enhance")
                            if record.summaryStatus == .unavailable { Text("Enhancement unavailable. Your transcript and notes are saved; try again.").font(.caption).foregroundStyle(.secondary) }
                        }.padding(16).background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 18))
                        Text("My notes").font(.headline)
                        TextEditor(text: $notes).frame(minHeight: 180).scrollContentBackground(.hidden)
                            .padding(8).background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 16))
                            .accessibilityIdentifier("meeting-document-notes").focused($focusedField, equals: "notes").disabled(busy)
                    } else {
                        Text("Transcript").font(.headline)
                        TextEditor(text: $transcript).frame(minHeight: 350).scrollContentBackground(.hidden)
                            .padding(8).background(ChatPalette.composer, in: RoundedRectangle(cornerRadius: 16))
                            .accessibilityIdentifier("meeting-document-transcript").focused($focusedField, equals: "transcript").disabled(busy)
                        Text("Speech recognition may contain errors. Edits are preserved when you leave. Save changes syncs them immediately.").font(.caption).foregroundStyle(.secondary)
                    }
                    if hasChanges {
                        Button("Save changes") { Task { await save() } }.buttonStyle(.borderedProminent)
                            .foregroundStyle(Color(uiColor: .systemBackground)).disabled(busy)
                            .accessibilityIdentifier("meeting-document-save")
                    }
                    if let audioError = library.audioError {
                        Text(audioError).font(.caption).foregroundStyle(.red)
                            .accessibilityIdentifier("meeting-audio-sync-error")
                        Button("Retry audio sync") { Task { await library.retry() } }.buttonStyle(.bordered)
                    }
                    if let pinnedScope {
                        MeetingAudioControls(id: id, scope: pinnedScope,
                            loadAudio: { try await library.audioURL(id: id) },
                            onTranscript: { text in replacementTranscript = text; confirmTranscriptReplacement = true })
                            .disabled(busy)
                    }
                    TextField("Ask a question about this meeting", text: $question, axis: .vertical)
                        .textFieldStyle(.roundedBorder).focused($focusedField, equals: "question")
                        .accessibilityIdentifier("meeting-question")
                    Text("Uses the current transcript and notes, including unsaved edits.").font(.caption).foregroundStyle(.secondary)
                    Button { askAgent() } label: { Label("Ask Nanocodex about this meeting", systemImage: "bubble.left.and.text.bubble.right") }
                        .buttonStyle(.bordered).disabled(busy || question.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        .accessibilityIdentifier("meeting-ask-agent")
                } else if scenePhase != .active { Text("Meeting content hidden while inactive").foregroundStyle(.secondary) }
                else if error == nil { ProgressView("Loading meeting…").frame(maxWidth: .infinity).padding(40) }
                if let error {
                    Text(error).font(.subheadline).foregroundStyle(.red).accessibilityIdentifier("meeting-document-error")
                    if record == nil { Button("Retry") { Task { await load() } }.buttonStyle(.bordered) }
                }
            }.padding(20).frame(maxWidth: 620).frame(maxWidth: .infinity).privacySensitive()
        }
    }
    var body: some View {
        documentContent
        .background(ChatPalette.background).navigationTitle("Meeting").navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .navigationBarBackButtonHidden(hasChanges)
        .toolbar {
            ToolbarItemGroup(placement: .keyboard) {
                Spacer()
                Button("Done typing") { focusedField = nil }.accessibilityIdentifier("meeting-keyboard-done")
            }
            ToolbarItem(placement: .topBarLeading) {
                if hasChanges {
                    Button("Back", systemImage: "chevron.left") {
                        Task { await save(); if !hasChanges { dismiss() } }
                    }.disabled(busy).accessibilityIdentifier("meeting-back-save")
                }
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
                if record != nil {
                    ShareLink(item: shareText) { Image(systemName: "square.and.arrow.up") }.disabled(scenePhase != .active)
                    Menu {
                        Button("Delete meeting", role: .destructive) { confirmDelete = true }.disabled(busy)
                    } label: { Image(systemName: "ellipsis") }.accessibilityIdentifier("meeting-document-menu")
                }
            }
        }
        .confirmationDialog("Delete this meeting?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete meeting", role: .destructive) {
                Task {
                    guard library.scope == pinnedScope else { return }
                    do { try await library.delete(id: id); dismiss() } catch { self.error = error.localizedDescription }
                }
            }
        } message: { Text("The transcript, your notes, and enhanced notes will be removed from your account. This cannot be undone.") }
        .confirmationDialog("Which meeting version should be kept?", isPresented: $confirmConflict, titleVisibility: .visible) {
            Button("Keep my version — replace server version", role: .destructive) { Task { await resolveConflict(keepLocal: true) } }
            Button("Use server version — discard my edits", role: .destructive) { Task { await resolveConflict(keepLocal: false) } }
        } message: { Text("Copy your local notes first if you want to combine both versions. Choosing a version is explicit and cannot be undone.") }
        .confirmationDialog("Replace the transcript?", isPresented: $confirmTranscriptReplacement, titleVisibility: .visible) {
            Button("Replace transcript", role: .destructive) {
                guard !busy, library.scope == pinnedScope, model.quickVoiceGeneration == accountGeneration,
                      let replacementTranscript else { return }
                transcript = replacementTranscript
                self.replacementTranscript = nil
            }.disabled(busy)
            Button("Cancel", role: .cancel) { replacementTranscript = nil }
        } message: { Text("This replaces the current transcript, including your edits. Review the result and save changes to sync it. Your notes and recording are kept.") }
        .task { pinnedScope = library.scope; accountGeneration = model.quickVoiceGeneration; await load() }
        .onChange(of: model.quickVoiceGeneration) { _, _ in
            replacementTranscript = nil; confirmTranscriptReplacement = false; question = ""; targetID = nil
        }
        .onChange(of: library.entries) { _, entries in
            guard library.scope == pinnedScope, !hasChanges, !busy,
                  let entry = entries.first(where: { $0.id == id }), entry.detailsLoaded else { return }
            // Background capture delivery/enhancement should become visible
            // without reopening the native document, but never replace typing.
            adopt(entry.record)
        }
        .onDisappear { retainEdits() }
    }
    private func adopt(_ value: MeetingRecord) { record = value; title = value.title; notes = value.notes; transcript = value.transcript }
    @MainActor private func load() async {
        busy = true; error = nil
        defer { busy = false }
        do {
            let value = try await library.detail(id: id)
            guard library.scope == pinnedScope, !Task.isCancelled else { return }
            adopt(value)
        } catch { if library.scope == pinnedScope { self.error = error.localizedDescription } }
    }
    @MainActor private func save() async {
        guard var record, library.scope == pinnedScope, !busy else { return }
        record.title = title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Untitled meeting" : title
        record.notes = notes; record.transcript = transcript
        busy = true; error = nil
        defer { busy = false }
        do {
            try await library.save(record)
            guard library.scope == pinnedScope else { return }
            adopt(try await library.detail(id: id)); error = library.error
        } catch { if library.scope == pinnedScope { self.error = error.localizedDescription } }
    }
    @MainActor private func enhance() async {
        guard library.scope == pinnedScope, !busy else { return }
        busy = true; error = nil
        defer { busy = false }
        do {
            try await library.summarize(id: id)
            guard library.scope == pinnedScope else { return }
            adopt(try await library.detail(id: id))
        } catch { if library.scope == pinnedScope { self.error = error.localizedDescription } }
    }
    @MainActor private func resolveConflict(keepLocal: Bool) async {
        guard library.scope == pinnedScope, !busy else { return }
        busy = true; error = nil; defer { busy = false }
        do {
            if keepLocal {
                if hasChanges, var draft = record {
                    draft.title = title.isEmpty ? "Untitled meeting" : title; draft.notes = notes; draft.transcript = transcript
                    try await library.save(draft)
                }
                try await library.chooseKeepLocal(id: id)
            }
            else { try await library.reloadServer(id: id) }
            guard library.scope == pinnedScope else { return }
            adopt(try await library.detail(id: id)); error = library.error
        } catch { if library.scope == pinnedScope { self.error = error.localizedDescription } }
    }
    /// Navigation preserves edited text in the original account's local outbox,
    /// even if a sign-out has already retired the active library client.
    @discardableResult private func retainEdits() -> Bool {
        guard hasChanges else { return true }
        guard var record, let pinnedScope, let store = model.meetingRecordingStore else {
            error = "Your edits could not be saved on this device."; return false
        }
        record.title = title.isEmpty ? "Untitled meeting" : title; record.notes = notes; record.transcript = transcript
        do { try store.put(record, scope: pinnedScope, state: .pending) }
        catch { self.error = "Your edits could not be saved: " + error.localizedDescription; return false }
        if library.scope == pinnedScope { library.reloadLocal(); Task { await library.retry() } }
        return true
    }
    private func askAgent() {
        guard let record, library.scope == pinnedScope, let accountGeneration,
              model.quickVoiceGeneration == accountGeneration else { return }
        let query = question.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return }
        let text = MeetingQuestionPrompt.make(question: query, title: title, notes: notes,
            transcript: transcript, capturedAt: Date(), duration: record.durationSeconds, partial: record.partial)
        guard retainEdits() else { return }
        guard model.sendQuickVoice(text, generation: accountGeneration, targetID: &targetID) else {
            error = model.error ?? "The question could not be sent. Your question is still here."; return
        }
        question = ""
        dismiss()
    }
}

/// Only finalized recordings are exposed by the audio store/transport. File work
/// is cancelled on navigation, and results are fenced to this account generation.
struct MeetingAudioControls: View {
    let id: UUID
    let scope: String
    let loadAudio: () async throws -> URL
    let onTranscript: (String) -> Void
    @ObservedObject private var recorder = MeetingRecorder.shared
    @ObservedObject private var model = InboxModel.shared
    @AppStorage("quickVoice.locale") private var locale = "en-US"
    @State private var url: URL?
    @State private var player: AVAudioPlayer?
    @State private var operation: Task<Void, Never>?
    @State private var busy = false
    @State private var error: String?
    @State private var epoch = UUID()
    private var audioInUse: Bool { recorder.working || QuickVoiceRecorder.audioOwner != nil || model.voice.isEngaged }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Original recording").font(.headline)
            if busy { ProgressView("Preparing recording…") }
            HStack {
                Button(url == nil ? "Load recording" : "Reload recording") { fetchAudio() }.disabled(busy || audioInUse)
                    .accessibilityIdentifier("meeting-load-audio")
                if let url {
                    TimelineView(.periodic(from: .now, by: 0.5)) { _ in
                        Button(player?.isPlaying == true ? "Stop playback" : "Play recording") { play(url) }
                            .disabled(busy || audioInUse).accessibilityIdentifier("meeting-play-audio")
                    }
                    ShareLink(item: url) { Label("Export audio", systemImage: "square.and.arrow.up") }
                        .accessibilityIdentifier("meeting-export-audio")
                }
            }.buttonStyle(.bordered)
            if url != nil {
                Picker("Transcription language", selection: $locale) { Text("English").tag("en-US"); Text("Ελληνικά").tag("el-GR") }
                    .disabled(busy)
                Button("Re-transcribe recording") { transcribe() }.buttonStyle(.bordered)
                    .disabled(busy || audioInUse).accessibilityIdentifier("meeting-retranscribe")
            }
            if audioInUse { Text("Finish recording before playback or re-transcription.").font(.caption).foregroundStyle(.secondary) }
            if let error { Text(error).font(.caption).foregroundStyle(.red) }
        }
        .onDisappear { cancel() }
        .onChange(of: model.quickVoiceGeneration) { _, _ in cancel(); url = nil }
        .onChange(of: recorder.working) { _, working in if working { cancel() } }
        .task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(300))
                if audioInUse, player != nil || busy { cancel() }
                else if let player, !player.isPlaying { stopPlayback() }
            }
        }
    }
    private func valid(_ token: UUID, generation: UUID) -> Bool {
        token == epoch && model.quickVoiceGeneration == generation && (try? model.lockedVoiceAccountScope()) == scope
    }
    private func fetchAudio() {
        cancel(); error = nil; busy = true
        let token = epoch, generation = model.quickVoiceGeneration
        operation = Task { @MainActor in
            defer { if epoch == token { busy = false; operation = nil } }
            do {
                let loaded = try await loadAudio()
                guard !Task.isCancelled, valid(token, generation: generation) else { return }
                url = loaded
            } catch {
                if valid(token, generation: generation), !Task.isCancelled {
                    self.error = "Recording unavailable on this device or account. Try again after audio sync. " + error.localizedDescription
                }
            }
        }
    }
    private func play(_ url: URL) {
        guard !audioInUse, (try? model.lockedVoiceAccountScope()) == scope else { return }
        if player?.isPlaying == true { stopPlayback(); return }
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, mode: .default)
            try session.setActive(true)
            player = try AVAudioPlayer(contentsOf: url)
            guard player?.play() == true else { throw CocoaError(.fileReadCorruptFile) }
            error = nil
        } catch {
            stopPlayback()
            if !audioInUse { try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
            self.error = error.localizedDescription
        }
    }
    private func transcribe() {
        guard let url, !audioInUse else { return }
        cancel(); busy = true; error = nil
        let token = epoch, generation = model.quickVoiceGeneration, language = locale
        operation = Task { @MainActor in
            defer { if epoch == token { busy = false; operation = nil } }
            do {
                let text = try await MeetingAudioTranscriber.transcribe(url: url, locale: language)
                guard !Task.isCancelled, valid(token, generation: generation), !audioInUse else { return }
                onTranscript(text)
            } catch {
                if valid(token, generation: generation), !Task.isCancelled { self.error = error.localizedDescription }
            }
        }
    }
    private func stopPlayback() {
        let hadPlayer = player != nil
        player?.stop(); player = nil
        if hadPlayer && !audioInUse { try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
    }
    private func cancel() {
        epoch = UUID(); operation?.cancel(); operation = nil; busy = false; stopPlayback()
    }
}
