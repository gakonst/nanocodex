import SwiftUI
import UniformTypeIdentifiers
import InboxCore
#if os(iOS)
import UIKit
#endif

struct ElevenLabsSettingsView: View {
    @ObservedObject var session: VoiceSession
    @Binding var sampleAudioBusy: Bool
    @Binding var settings: VoiceSettings
    let onUseVoice: () -> Void
    let configuration: @MainActor () async throws -> VoiceConfiguration
    var urlConfiguration: URLSessionConfiguration? = nil
    @State private var client: ElevenLabs?
    @State private var key = ""
    @State private var configured = false
    @State private var voices: [JSON] = []
    @State private var cursor: String?
    @State private var name = ""
    @FocusState private var editingName: Bool
    @State private var files: [URL] = []
    @State private var consent = false
    @State private var importing = false
    @State private var busy = false
    @State private var message: String?
    @State private var verification = false
    @State private var createdVoiceID: String?
    @State private var uploading = false
    @State private var refreshBeforeRetry = false
    @State private var operation: Task<Void, Never>?
    @State private var previewFile: URL?

    @State private var catalog: VoiceCatalog = .all
    #if os(iOS)
    @StateObject private var recording = VoiceCloneRecording()
    @Environment(\.scenePhase) private var scenePhase
    #endif
    private var displayedVoices: [JSON] { voices.filter { catalog.includes($0) } }
    private var cloneFiles: [URL] {
        #if os(iOS)
        if let sample = recording.sample { return [sample] }
        #endif
        return files
    }

    var body: some View {
        Section("ElevenLabs") {
            Group {
            if configured {
                Text("API key connected and stored encrypted on the server.")
                Button("Disconnect ElevenLabs") { perform {
                    _ = try await client?.request(method: "DELETE")
                    configured = false; voices = []; settings.outputProvider = .openai; settings.elevenLabsVoiceId = nil
                } }.disabled(sampleAudioBusy)
                Picker("Voice catalog", selection: $catalog) {
                    ForEach(VoiceCatalog.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                }.pickerStyle(.segmented)
                ForEach(displayedVoices.indices, id: \.self) { index in
                    let voice = displayedVoices[index]
                    Button {
                        settings.elevenLabsVoiceId = voice["voice_id"].string
                    } label: {
                        HStack {
                            Text(voice["name"].string)
                            Spacer()
                            if settings.elevenLabsVoiceId == voice["voice_id"].string { Image(systemName: "checkmark") }
                        }
                    }.accessibilityAddTraits(settings.elevenLabsVoiceId == voice["voice_id"].string ? .isSelected : [])
                }
                if displayedVoices.isEmpty { Text("No voices in this loaded catalog. Refresh or load more voices.").font(.caption) }
                if let selected = settings.elevenLabsVoiceId {
                    Text("Selected: " + (voices.first { $0["voice_id"].string == selected }?["name"].string ?? selected)).font(.caption)
                }
                Text(session.isEngaged ? "Apply and reconnect to use the selected voice." : "Save to use the selected voice on your next call.").font(.caption)
                #if os(iOS)
                if previewFile != nil, recording.playing {
                    Button("Stop voice preview") { recording.stopPlayback(); removePreview() }
                } else {
                    Button("Preview selected voice") { previewVoice() }
                        .disabled(sampleAudioBusy || session.isEngaged || settings.elevenLabsVoiceId == nil)
                        .accessibilityIdentifier("preview-selected-voice")
                    if session.isEngaged { Text("End the voice call to preview a different voice here.").font(.caption) }
                }
                #endif
                Button("Refresh voices") { perform { try await loadVoices(); refreshBeforeRetry = false } }.disabled(sampleAudioBusy)
                if cursor != nil { Button("Load more voices") { perform { try await loadVoices(more: true) } }.disabled(sampleAudioBusy) }
                Divider()
                cloneControls
            } else {
                SecureField("ElevenLabs API key", text: $key)
                Button("Connect ElevenLabs") { perform {
                    guard let client else { return }
                    _ = try await client.request(method: "PUT", body: .object(["api_key": .string(key)]))
                    key = ""; configured = true
                    try await loadVoices()
                } }.disabled(key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            }.disabled(busy)
            if busy { ProgressView(uploading ? "Uploading sample and creating voice…" : "Please wait…") }
            if uploading {
                Button("Cancel upload") {
                    refreshBeforeRetry = true
                    message = "Stopped waiting for the upload. It may already have reached ElevenLabs; refresh voices before creating another clone."
                    operation?.cancel()
                }.accessibilityIdentifier("clone-cancel-upload")
            }
            if let message { Text(message).font(.caption).textSelection(.enabled) }
        }
        #if os(iOS)
        .onDisappear { operation?.cancel(); operation = nil; recording.discard(); removePreview(); sampleAudioBusy = false; key = "" }
        .onChange(of: recording.recording) { _, _ in updateAudioBusy() }
        .onChange(of: recording.playing) { _, playing in updateAudioBusy(); if !playing { removePreview() } }
        .onChange(of: recording.preparing) { _, _ in updateAudioBusy() }
        .onChange(of: recording.saving) { _, _ in updateAudioBusy() }
        .onChange(of: recording.sample) { _, sample in consent = false; if sample != nil { files = [] } }
        .onChange(of: busy) { _, _ in updateAudioBusy() }
        .onChange(of: session.isEngaged) { _, engaged in if engaged { recording.discard(); updateAudioBusy() } }
        .onChange(of: scenePhase) { _, phase in
            // A microphone permission sheet temporarily makes the scene inactive.
            if phase == .background || (phase == .inactive && !recording.preparing) { recording.suspend() }
        }
        #endif
        .fileImporter(isPresented: $importing, allowedContentTypes: [.audio], allowsMultipleSelection: true) { result in
            switch result {
            case .success(let urls):
                guard !urls.isEmpty else { return }
                #if os(iOS)
                recording.discard()
                #endif
                files = urls; consent = false; createdVoiceID = nil
            case .failure(let error): message = error.localizedDescription
            }
        }
        .task {
            busy = true
            defer { busy = false }
            do {
                client = try await ElevenLabs(configuration: configuration(), urlConfiguration: urlConfiguration)
                configured = try await client?.request()["configured"].bool ?? false
                if configured { try await loadVoices() }
            } catch { message = error.localizedDescription }
        }
    }
    @ViewBuilder private var cloneControls: some View {
        Text("Clone a voice").font(.headline).accessibilityIdentifier("clone-voice-heading")
        Text("1. Name your voice").font(.subheadline.weight(.medium))
        TextField("For example, My natural voice", text: $name)
            .focused($editingName).submitLabel(.done).onSubmit { editingName = false }
            .accessibilityLabel("Clone name").accessibilityIdentifier("clone-name")
        if name.count > 100 { Text("Use 100 characters or fewer.").font(.caption).foregroundStyle(.red) }
        Text("2. Record or choose a sample").font(.subheadline.weight(.medium))
        #if os(iOS)
        recordingControls
        #endif
        DisclosureGroup("What should I say?") {
            Text("Read this at your normal pace, or talk about your day. Use the tone you want the clone to have; keep your distance from the microphone steady and avoid whispering or exaggerating your delivery.").font(.caption)
            Text(VoiceCloneGuidance.script).font(.body).textSelection(.enabled)
        }
        Button("Choose audio samples") {
            editingName = false; importing = true
        }.disabled(sampleAudioBusy).accessibilityIdentifier("clone-import-samples")
        ForEach(Array(files.enumerated()), id: \.offset) { index, file in
            HStack {
                Text(file.lastPathComponent).lineLimit(2)
                Spacer()
                #if os(iOS)
                Button(recording.playingURL == file ? "Stop" : "Review") {
                    if recording.playingURL == file { recording.stopPlayback() } else { recording.play(url: file) }
                }.disabled(session.isEngaged || recording.preparing || recording.recording || recording.saving || (recording.playing && recording.playingURL != file))
                .buttonStyle(.borderless)
                #endif
                Button("Remove", role: .destructive) {
                    #if os(iOS)
                    recording.stopPlayback()
                    #endif
                    files.remove(at: index); consent = false
                }.buttonStyle(.borderless)
            }
        }
        Text("MP3, WAV, M4A, AAC, OGG, WebM or FLAC. One to five samples, up to 10 MB each and below 20 MB total.").font(.caption)
        if files.count > 5 { Text("Remove samples until five or fewer remain.").font(.caption).foregroundStyle(.red) }
        Text("3. Review and create").font(.subheadline.weight(.medium))
        Toggle("I own this voice or have explicit permission to clone and use it.", isOn: $consent)
            .accessibilityIdentifier("clone-consent")
        Text("Creating a clone uploads these samples to ElevenLabs. Recording and review stay on this device.").font(.caption)
        Button("Create voice clone") { createClone() }
            .disabled(sampleAudioBusy || refreshBeforeRetry || !VoiceCloneGuidance.canCreate(name: name, count: cloneFiles.count, consent: consent))
            .accessibilityIdentifier("clone-create")
        if refreshBeforeRetry {
            Text("Refresh voices before uploading again: the previous request may already have created a clone. Your sample is still available.").font(.caption)
        }
        if let createdVoiceID, !verification {
            Button("Use this voice") { settings.elevenLabsVoiceId = createdVoiceID; settings.outputProvider = .elevenlabs; onUseVoice() }
                .disabled(sampleAudioBusy).accessibilityIdentifier("clone-use-voice")
        }
        if verification { Link("Open ElevenLabs verification", destination: URL(string: "https://elevenlabs.io/app/voice-lab")!) }
    }
    @MainActor private func createClone() {
        guard let client else { return }
        editingName = false; uploading = true
        perform {
            defer { uploading = false }
            let result: JSON
            do { result = try await client.clone(name: name, files: cloneFiles, consent: consent) }
            catch {
                let code = (error as? ManagedError)?.code
                refreshBeforeRetry = !["invalid_clone", "invalid_audio", "upload_size"].contains(code ?? "")
                throw error
            }
            verification = result["requires_verification"].bool
            createdVoiceID = result["voice_id"].string
            if !verification { settings.elevenLabsVoiceId = createdVoiceID }
            #if os(iOS)
            recording.discard()
            #endif
            files = []; consent = false
            message = verification ? "Voice created. Complete verification in ElevenLabs, then refresh voices." : "Voice created. Tap Use this voice to save your selection."
            do { try await loadVoices() }
            catch { message = "Voice created, but the catalog could not refresh. " + (verification ? "Complete verification in ElevenLabs." : "You can still use this voice.") }
            if !verification, let id = createdVoiceID, !voices.contains(where: { $0["voice_id"].string == id }) {
                voices.insert(.object(["voice_id": .string(id), "name": .string(name), "category": .string("cloned")]), at: 0)
            }
            if !verification { catalog = .cloned }
        }
    }
    #if os(iOS)
    private func removePreview() {
        if let previewFile { try? FileManager.default.removeItem(at: previewFile) }
        previewFile = nil
    }
    private func previewVoice() {
        guard let client, let id = settings.elevenLabsVoiceId else { return }
        editingName = false
        perform {
            let data = try await client.speech(text: "Hello! This is a preview of my voice. How does it sound?", voiceID: id)
            try Task.checkCancellation()
            let file = FileManager.default.temporaryDirectory.appendingPathComponent("voice-preview-\(UUID().uuidString).mp3")
            try data.write(to: file, options: .atomic)
            removePreview(); previewFile = file
            recording.play(url: file)
            if !recording.playing { removePreview() }
        }
    }
    private func updateAudioBusy() {
        sampleAudioBusy = busy || recording.preparing || recording.recording || recording.saving || recording.playing
    }
    @ViewBuilder private var recordingControls: some View {
        if session.isEngaged {
            Button("End voice call to record a sample") { session.stop() }
            Text("Recording and review use the microphone and speaker after your call ends.").font(.caption)
        } else if recording.preparing {
            Text("Waiting for microphone permission…")
            Button("Cancel recording") { recording.cancelPreparation() }
        } else if recording.saving {
            ProgressView("Saving your recording…")
        } else if recording.recording {
            Label("Recording \(Int(recording.elapsed)) / 120 seconds", systemImage: "record.circle.fill")
                .foregroundStyle(.red)
            ProgressView(value: Double(recording.level)).accessibilityLabel("Microphone level")
            Text("Aim for 60–90 seconds. Speak naturally in your usual tone in a quiet room; avoid music and other voices.").font(.caption)
            Button("Stop recording") { recording.stop() }.accessibilityIdentifier("clone-stop-recording")
            Button("Cancel recording", role: .destructive) { recording.cancelRecording(); consent = false }
        } else {
            Text("Record 60–90 seconds in a quiet room using your usual tone. Recording stops after two minutes.").font(.caption)
            Button(recording.sample == nil ? "Record voice sample" : "Record again") {
                editingName = false; consent = false; createdVoiceID = nil
                sampleAudioBusy = true
                Task { await recording.start(); updateAudioBusy() }
            }
            if recording.sample != nil {
                Button(recording.playingURL == recording.sample ? "Stop playback" : "Review recording") {
                    if recording.playingURL == recording.sample { recording.stopPlayback() } else { recording.play() }
                }.disabled(recording.playing && recording.playingURL != recording.sample)
                Button("Remove recording", role: .destructive) { recording.discard(); consent = false }
                Text("Recorded \(Int(recording.duration.rounded())) seconds.").font(.caption)
                if recording.playingURL == recording.sample {
                    ProgressView(value: recording.playbackElapsed, total: max(1, recording.duration))
                        .accessibilityLabel("Recording playback")
                    Text("Playing \(Int(recording.playbackElapsed)) of \(Int(recording.duration.rounded())) seconds").font(.caption)
                }
                if recording.duration < 30 { Text("This sample is short. Aim for 60–90 seconds for a more representative voice clone.").font(.caption) }
                Text("Review your recording before consenting to upload it. Audio stays on this device until you create the clone.").font(.caption)
            }
        }
        if let error = recording.error { Text(error).font(.caption).foregroundStyle(.red) }
        if recording.permissionDenied {
            Link("Open microphone settings", destination: URL(string: UIApplication.openSettingsURLString)!)
        }
    }
    #endif
    @MainActor private func perform(_ action: @escaping @MainActor () async throws -> Void) {
        busy = true; sampleAudioBusy = true; message = nil
        operation = Task {
            defer {
                busy = false
                #if os(iOS)
                updateAudioBusy()
                #else
                sampleAudioBusy = false
                #endif
            }
            do { try await action() }
            catch is CancellationError { }
            catch { message = error.localizedDescription }
        }
    }
    @MainActor private func loadVoices(more: Bool = false) async throws {
        guard let client else { return }
        var suffix = "/voices"
        if more, let cursor {
            var parts = URLComponents(); parts.queryItems = [URLQueryItem(name: "next_page_token", value: cursor)]
            suffix += "?" + (parts.percentEncodedQuery ?? "")
        }
        let result = try await client.request(suffix)
        let fetched = result["voices"].array
        voices = more ? voices + fetched.filter { item in !voices.contains(where: { $0["voice_id"] == item["voice_id"] }) } : fetched
        cursor = result["has_more"].bool && !result["next_page_token"].string.isEmpty ? result["next_page_token"].string : nil
    }
}

/// ElevenLabs returns instant clones as cloned and verified professional clones as professional.
enum VoiceCatalog: String, CaseIterable {
    case all = "All", existing = "Existing", cloned = "Cloned"
    func includes(_ voice: JSON) -> Bool {
        let clone = ["cloned", "professional"].contains(voice["category"].string)
        return self == .all || (self == .cloned ? clone : !clone)
    }
}
