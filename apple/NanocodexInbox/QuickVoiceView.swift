import SwiftUI
import Speech
import AVFoundation
import InboxCore

@MainActor
final class QuickVoiceRecorder: ObservableObject {
    @Published var transcript = ""
    @Published var status = "Ready"
    @Published var recording = false
    @Published var working = false
    var onFinal: ((String) -> Void)?
    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var recognition: SFSpeechRecognitionTask?
    private var deadline: Task<Void, Never>?
    private var gate = QuickVoiceCaptureGate()
    private var tapped = false
    private var finishing = false

    func start(locale: String) async {
        stop()
        let token = gate.begin()
        working = true
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
        guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: locale)), recognizer.isAvailable else {
            fail("Speech recognition is unavailable. Try again when connected."); return
        }
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement, options: [.duckOthers])
            try session.setActive(true)
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
                    if let error { self.fail(error.localizedDescription); return }
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
            armDeadline(seconds: 15, token: token) { self.fail("No speech was recognized. Try again.") }
        } catch { fail(error.localizedDescription) }
    }

    func finish() {
        guard recording, !finishing else { return }
        finishing = true
        recording = false
        status = "Finishing transcription…"
        releaseMicrophone()
        request?.endAudio()
        // Wait for the recognizer's final result; never send the last partial on timeout.
        armDeadline(seconds: 5, token: gate.token) {
            self.fail("Transcription did not finish. Your words are preserved; edit and send or try again.")
        }
    }

    func interrupt() {
        guard working else { return }
        fail("Recording interrupted. Your words are preserved; edit and send or try again.")
    }

    func fail(_ message: String) { stop(); status = message }

    func stop() {
        gate.cancel()
        deadline?.cancel(); deadline = nil
        releaseMicrophone()
        recognition?.cancel(); recognition = nil
        request = nil
        finishing = false; recording = false; working = false
    }

    private func releaseMicrophone() {
        engine.stop()
        if tapped { engine.inputNode.removeTap(onBus: 0); tapped = false }
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
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
