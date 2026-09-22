import SwiftUI
import Speech
import AVFoundation
import InboxCore
import OSLog

@MainActor
final class QuickVoiceRecorder: ObservableObject {
    @Published var transcript = ""
    @Published var status = "Ready"
    @Published var recording = false
    @Published var working = false
    var onFinal: ((String) -> Void)?
    var onStatus: ((String) -> Void)?
    var onError: ((String) -> Void)?
    var onAudioEnded: (() -> Void)?
    enum PermissionMode { case request, alreadyGranted }
    private let engine = AVAudioEngine()
    private let log = Logger(subsystem: "xyz.paradigm.centaur", category: "QuickVoice")
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var recognition: SFSpeechRecognitionTask?
    private var deadline: Task<Void, Never>?
    private var gate = QuickVoiceCaptureGate()
    fileprivate static weak var audioOwner: AnyObject?
    private var sessionActive = false
    private var tapped = false
    private var finishing = false

    static var permissionsGranted: Bool {
        SFSpeechRecognizer.authorizationStatus() == .authorized && AVAudioApplication.shared.recordPermission == .granted
    }

    func start(locale: String, permissions: PermissionMode = .request) async {
        stop()
        guard Self.audioOwner == nil else {
            fail("Another voice recording is in progress. Finish it first."); return
        }
        Self.audioOwner = self
        let token = gate.begin()
        working = true
        if permissions == .alreadyGranted {
            guard Self.permissionsGranted else {
                fail("Allow Microphone and Speech Recognition in the app before recording from the Lock Screen."); return
            }
        } else {
            status = "Requesting microphone and speech access…"
            let speech = await withCheckedContinuation { continuation in
                SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0) }
            }
            guard gate.accepts(token) else { return }
            let microphone = await AVAudioApplication.requestRecordPermission()
            guard gate.accepts(token) else { return }
            guard speech == .authorized, microphone else {
                fail("Allow Microphone and Speech Recognition in Settings to record."); return
            }
        }
        guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: locale)), recognizer.isAvailable else {
            fail("Speech recognition is unavailable. Try again when connected."); return
        }
        do {
            let session = AVAudioSession.sharedInstance()
            // duckOthers is unsupported by the record-only category.
            try session.setCategory(.record, mode: .measurement)
            try session.setActive(true)
            sessionActive = true
            let request = SFSpeechAudioBufferRecognitionRequest()
            request.shouldReportPartialResults = true
            request.taskHint = .dictation
            self.request = request
            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0 else {
                fail("No microphone is available."); return
            }
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
                request.append(buffer)
            }
            tapped = true
            recognition = recognizer.recognitionTask(with: request) { [weak self] result, error in
                let text = result?.bestTranscription.formattedString
                let final = result?.isFinal ?? false
                Task { @MainActor in
                    guard let self, self.gate.accepts(token) else { return }
                    if let text { self.transcript = text }
                    // Errors and interruptions never submit a partial transcript.
                    if let error {
                        let failure = error as NSError
                        self.log.error("Speech failed: domain=\(failure.domain, privacy: .public) code=\(failure.code)")
                        self.fail(error.localizedDescription); return
                    }
                    if let text, let input = self.gate.completed(text, token: token, isFinal: final) {
                        self.stop()
                        self.status = "Starting task…"
                        self.onFinal?(input)
                    } else if final {
                        self.fail("No speech was recognized. Try again.")
                    } else if !self.finishing {
                        self.armDeadline(seconds: 1.8, token: token) { self.finish() }
                    }
                }
            }
            engine.prepare()
            try engine.start()
            transcript = ""
            recording = true
            status = "Listening… Pause when finished to start your task."
            onStatus?("listening")
            armDeadline(seconds: 15, token: token) { self.fail("No speech was recognized. Try again.") }
        } catch {
            let failure = error as NSError
            log.error("Audio start failed: domain=\(failure.domain, privacy: .public) code=\(failure.code)")
            fail(error.localizedDescription)
        }
    }

    func finish() {
        guard recording, !finishing else { return }
        finishing = true
        recording = false
        status = "Finishing transcription…"
        onStatus?("transcribing")
        releaseMicrophone(endAudio: true)
        // Wait for the recognizer's final result; never send the last partial on timeout.
        armDeadline(seconds: 5, token: gate.token) {
            self.fail("Transcription did not finish. Your words are preserved; edit and send or try again.")
        }
    }

    func interrupt() {
        guard working else { return }
        fail("Recording interrupted. Your words are preserved; edit and send or try again.")
    }

    func fail(_ message: String) { stop(); status = message; onError?(message) }

    func stop() {
        gate.cancel()
        deadline?.cancel(); deadline = nil
        releaseMicrophone()
        recognition?.cancel(); recognition = nil
        request = nil
        finishing = false; recording = false; working = false
        if Self.audioOwner === self { Self.audioOwner = nil }
    }

    private func releaseMicrophone(endAudio: Bool = false) {
        let hadAudio = tapped
        // Reserve completion time before relinquishing background audio execution.
        if hadAudio { onAudioEnded?() }
        engine.stop()
        if tapped { engine.inputNode.removeTap(onBus: 0); tapped = false }
        if endAudio { request?.endAudio() }
        if sessionActive {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            sessionActive = false
        }
    }

    private func armDeadline(seconds: Double, token: UUID, action: @escaping @MainActor () -> Void) {
        deadline?.cancel()
        deadline = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(seconds)) } catch { return }
            guard let self, self.gate.accepts(token) else { return }
            action()
        }
    }
}

/// Lock Screen capture is explicitly committed by Send. Speech recognition starts
/// only after recording ends, so recognizer pauses cannot stop or submit capture.
@MainActor
final class LockedAudioRecorder: NSObject, AVAudioRecorderDelegate {
    var transcript = ""
    private(set) var recording = false
    var onFinal: ((String) -> Void)?
    var onStatus: ((String) -> Void)?
    var onError: ((String) -> Void)?
    var onAudioEnded: (() -> Void)?
    private var recorder: AVAudioRecorder?
    private var recognition: SFSpeechRecognitionTask?
    private var recognizer: SFSpeechRecognizer?
    private var file: URL?
    private var deadline: Task<Void, Never>?
    private var token = UUID()
    private var sessionActive = false
    private let log = Logger(subsystem: "xyz.paradigm.centaur", category: "QuickVoice")

    func start(locale: String, permissions: QuickVoiceRecorder.PermissionMode) async {
        stop()
        transcript = ""
        guard QuickVoiceRecorder.audioOwner == nil else {
            fail("Another recording is in progress."); return
        }
        QuickVoiceRecorder.audioOwner = self
        let storage = FileManager.default.temporaryDirectory.appendingPathComponent("locked-voice", isDirectory: true)
        let protection: [FileAttributeKey: Any] = [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication]
        // Protect the directory as well as the file: AVAudioRecorder prepares its own
        // output and may replace a precreated file. Its output must inherit a class
        // that can also be reopened for transcription after Send while locked.
        // Reap only this feature's orphan files after an interrupted process.
        var stage = "storageDirectory"
        do {
            try FileManager.default.createDirectory(at: storage, withIntermediateDirectories: true, attributes: protection)
            try FileManager.default.setAttributes(protection, ofItemAtPath: storage.path)
            stage = "cleanup"
            for url in try FileManager.default.contentsOfDirectory(at: storage, includingPropertiesForKeys: nil)
                where url.pathExtension == "m4a" {
                try FileManager.default.removeItem(at: url)
            }
            // Also remove recordings left by versions that used the temporary root.
            for url in try FileManager.default.contentsOfDirectory(at: FileManager.default.temporaryDirectory,
                includingPropertiesForKeys: nil) where url.lastPathComponent.hasPrefix("locked-voice-") && url.pathExtension == "m4a" {
                try FileManager.default.removeItem(at: url)
            }
        } catch {
            report(error, stage: stage)
            fail("Recording storage unavailable."); return
        }
        guard QuickVoiceRecorder.permissionsGranted else {
            fail("Microphone or speech permission unavailable."); return
        }
        recognizer = SFSpeechRecognizer(locale: Locale(identifier: locale))
        do {
            let session = AVAudioSession.sharedInstance()
            stage = "sessionCategory"
            try session.setCategory(.record, mode: .measurement)
            stage = "sessionActivation"
            try session.setActive(true)
            sessionActive = true
            let url = storage.appendingPathComponent("locked-voice-\(UUID().uuidString).m4a")
            file = url
            stage = "recorderInitialization"
            let audio = try AVAudioRecorder(url: url, settings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 44100,
                AVNumberOfChannelsKey: 1,
                AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue
            ])
            audio.delegate = self
            recorder = audio
            stage = "recorderPreparation"
            guard audio.prepareToRecord() else { throw CocoaError(.fileWriteUnknown) }
            // Apply to the actual prepared output, not a placeholder it can replace.
            stage = "outputProtection"
            try FileManager.default.setAttributes(protection, ofItemAtPath: url.path)
            stage = "record"
            guard audio.record() else { throw CocoaError(.fileWriteUnknown) }
            recording = true
            onStatus?("listening")
        } catch {
            report(error, stage: stage)
            fail("Microphone could not start.")
        }
    }

    func finish() {
        guard recording, let file else { return }
        // Acquire background completion time before relinquishing microphone access.
        onAudioEnded?()
        recording = false
        recorder?.delegate = nil
        recorder?.stop()
        recorder = nil
        deactivate()
        onStatus?("transcribing")
        guard let recognizer, recognizer.isAvailable else {
            fail("Speech recognition unavailable."); return
        }
        let current = token
        let request = SFSpeechURLRecognitionRequest(url: file)
        request.shouldReportPartialResults = false
        recognition = recognizer.recognitionTask(with: request) { [weak self] result, error in
            let text = result?.bestTranscription.formattedString
            let final = result?.isFinal == true
            Task { @MainActor in
                guard let self, self.token == current else { return }
                if let error {
                    self.report(error, stage: "transcribe")
                    self.fail("Transcription failed."); return
                }
                guard final else { return }
                guard let text, let input = QuickVoiceInput.finalText(text) else {
                    self.fail("No speech recognized."); return
                }
                self.transcript = input
                self.stop()
                self.onFinal?(input)
            }
        }
        deadline = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(20)) } catch { return }
            guard let self, self.token == current else { return }
            self.fail("Transcription timed out.")
        }
    }

    func interrupt() { fail("Recording interrupted.") }

    func stop() {
        token = UUID()
        deadline?.cancel(); deadline = nil
        if recording { onAudioEnded?() }
        recording = false
        recorder?.delegate = nil
        recorder?.stop(); recorder = nil
        recognition?.cancel(); recognition = nil
        recognizer = nil
        deactivate()
        if let file {
            do { try FileManager.default.removeItem(at: file); self.file = nil }
            catch { report(error, stage: "cleanup") }
        }
        if QuickVoiceRecorder.audioOwner === self { QuickVoiceRecorder.audioOwner = nil }
    }

    private func deactivate() {
        guard sessionActive else { return }
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        sessionActive = false
    }

    private func fail(_ message: String) {
        log.error("Locked audio failure: \(message, privacy: .public)")
        stop()
        onError?(message)
    }

    private func report(_ error: Error, stage: String) {
        let error = error as NSError
        log.error("Locked audio \(stage, privacy: .public): domain=\(error.domain, privacy: .public) code=\(error.code)")
    }

    nonisolated func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
        Task { @MainActor [weak self] in
            guard let self, self.recorder === recorder else { return }
            if let error { self.report(error, stage: "encode") }
            self.fail("Audio recording failed.")
        }
    }

    nonisolated func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully flag: Bool) {
        Task { @MainActor [weak self] in
            guard let self, self.recorder === recorder, self.recording else { return }
            self.fail("Audio recording ended unexpectedly.")
        }
    }
}

struct QuickVoiceView: View {
    @ObservedObject var model: InboxModel
    @StateObject private var recorder = QuickVoiceRecorder()
    @AppStorage("quickVoice.locale") private var locale = "en-US"
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var started = false
    @State private var submitted = false
    @State private var visible = true
    @State private var account: UUID?
    @State private var targetID: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Picker("Speech language", selection: $locale) {
                    Text("English").tag("en-US")
                    Text("Ελληνικά").tag("el-GR")
                }.pickerStyle(.segmented)
                    .onChange(of: locale) { _, _ in
                        recorder.stop()
                        Task { await start() }
                    }
                Text(recorder.status).accessibilityIdentifier("quickVoiceStatus")
                TextEditor(text: $recorder.transcript)
                    .disabled(recorder.working)
                    .accessibilityIdentifier("quickVoiceTranscript")
                if recorder.recording {
                    Button("Finish speaking") { recorder.finish() }
                } else if !recorder.working {
                    Button("Record again") { Task { await start() } }
                    Button("Send in new conversation") { submit(recorder.transcript) }
                        .disabled(QuickVoiceInput.finalText(recorder.transcript) == nil || !model.connected)
                }
                Text("Speech is transcribed by Apple in the selected language. A completed utterance starts a new conversation automatically.")
                    .font(.footnote).foregroundStyle(.secondary)
            }
            .padding()
            .navigationTitle("New voice task")
            .toolbar { ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { recorder.stop(); dismiss() }
            } }
        }
        .interactiveDismissDisabled(recorder.working)
        .task { await startIfReady() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await startIfReady() } }
            else if phase == .background { recorder.interrupt() }
        }
        .onChange(of: model.restoringAccount) { _, _ in Task { await startIfReady() } }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification)) { _ in recorder.interrupt() }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.routeChangeNotification)) { notification in
            if let reason = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
               reason == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue { recorder.interrupt() }
        }
        .onDisappear { visible = false; recorder.stop() }
    }

    private func startIfReady() async {
        guard !started, scenePhase == .active else { return }
        guard !model.restoringAccount else { recorder.status = "Connecting to your account…"; return }
        started = true
        await start()
    }

    private func start() async {
        guard visible, scenePhase == .active, !submitted else { return }
        guard model.connected, !model.isDemo else {
            recorder.fail("Sign in and connect first, then tap Record again."); return
        }
        LockedVoiceCoordinator.shared.yieldToForegroundRecording()
        account = model.quickVoiceGeneration
        model.voice.stop()
        recorder.onFinal = { submit($0) }
        await recorder.start(locale: locale == "el-GR" ? "el-GR" : "en-US")
    }

    private func submit(_ text: String) {
        guard visible, scenePhase == .active else {
            recorder.fail("Recording interrupted. Your words are preserved; edit and send or try again."); return
        }
        guard !submitted, let text = QuickVoiceInput.finalText(text) else { return }
        guard model.connected, !model.isDemo, account == model.quickVoiceGeneration else {
            recorder.fail("Your account changed or disconnected. Your words are preserved. Record again after signing in."); return
        }
        guard let account, model.sendQuickVoice(text, generation: account, targetID: &targetID) else {
            recorder.fail(model.error ?? "Could not queue the task. Your words are preserved."); return
        }
        submitted = true
        dismiss()
    }
}
