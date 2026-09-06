import SwiftUI
import Speech
import AVFoundation

@MainActor
final class VoiceInput: ObservableObject {
    @Published var text = ""
    @Published var recording = false
    @Published var level: CGFloat = 0
    @Published var error: String?
    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var recognition: SFSpeechRecognitionTask?
    private var tapped = false
    private var epoch = UUID()

    func start(demo: Bool) async {
        let token = epoch
        if demo {
            if ProcessInfo.processInfo.environment["NANOCODEX_DEMO_VOICE_ERROR"] == "1" {
                error = "Microphone unavailable. You can keep typing."
            } else {
                recording = true; level = 0.45
                text = "Check reconnect first, then simplify the inbox"
            }
            return
        }
        let speech = await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0) }
        }
        guard epoch == token, !Task.isCancelled else { return }
        guard speech == .authorized else { error = "Allow Speech Recognition in Settings to dictate. You can keep typing."; return }
        let microphone = await AVCaptureDevice.requestAccess(for: .audio)
        guard epoch == token, !Task.isCancelled else { return }
        guard microphone else { error = "Allow Microphone in Settings to dictate. You can keep typing."; return }
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else {
            error = "Speech recognition is unavailable. Try again later or keep typing."; return
        }
        do {
            #if os(iOS)
            try AVAudioSession.sharedInstance().setCategory(.record, mode: .measurement, options: .duckOthers)
            try AVAudioSession.sharedInstance().setActive(true)
            #endif
            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0 else { throw CocoaError(.fileReadUnknown) }
            let request = SFSpeechAudioBufferRecognitionRequest()
            request.shouldReportPartialResults = true
            self.request = request
            recognition = recognizer.recognitionTask(with: request) { [weak self] result, failure in
                Task { @MainActor [weak self] in
                    guard let self, self.epoch == token else { return }
                    if let result { self.text = result.bestTranscription.formattedString }
                    if failure != nil || result?.isFinal == true {
                        if failure != nil { self.error = "Dictation stopped. Your captured text is kept." }
                        self.stop()
                    }
                }
            }
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
                request.append(buffer)
                guard let samples = buffer.floatChannelData?[0], buffer.frameLength > 0 else { return }
                let count = Int(buffer.frameLength)
                var energy: Float = 0
                for index in 0..<count { energy += samples[index] * samples[index] }
                let level = CGFloat(min(1, sqrt(energy / Float(count)) * 12))
                Task { @MainActor [weak self] in
                    guard let self, self.epoch == token else { return }
                    self.level = level
                }
            }
            tapped = true
            engine.prepare(); try engine.start(); recording = true
        } catch {
            stop(); self.error = "Could not start the microphone. You can keep typing."
        }
    }
    func stop() {
        epoch = UUID()
        engine.stop()
        if tapped { engine.inputNode.removeTap(onBus: 0); tapped = false }
        request?.endAudio(); recognition?.cancel()
        recognition = nil; request = nil; recording = false; level = 0
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }
}

struct VoiceInputView: View {
    @StateObject private var voice = VoiceInput()
    @State private var committed = false
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let agentTitle: String
    let demo: Bool
    let finish: (String) -> Void

    var body: some View {
        VStack(spacing: 22) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(demo ? "Voice · Demo" : "Voice").font(.system(size: 21, weight: .semibold))
                    Text(agentTitle).font(.system(size: 13)).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer()
                Button { dismiss() } label: {
                    Image(systemName: "checkmark").font(.system(size: 18, weight: .semibold))
                        .frame(width: 44, height: 44)
                }.accessibilityLabel("Use voice draft")
            }
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 12).onEnded { value in
                if value.translation.height > 45 && value.translation.height > abs(value.translation.width) { dismiss() }
            })
            HStack(spacing: 5) {
                ForEach(0..<23) { index in
                    Capsule().fill(Color.primary)
                        .frame(width: 5, height: 7 + voice.level * CGFloat(16 + (index * 17) % 65))
                }
            }.frame(height: 80).animation(reduceMotion ? nil : .linear(duration: 0.08), value: voice.level)
                .accessibilityHidden(true)
            ScrollView {
                Text(voice.text.isEmpty ? (voice.error != nil ? "Voice unavailable" : voice.recording ? "Listening…" : "Getting ready…") : voice.text)
                    .font(.system(size: 22)).frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier("voice-transcript")
            }.frame(maxHeight: 130)
            if let error = voice.error {
                Text(error).font(.system(size: 13)).foregroundStyle(.secondary).accessibilityIdentifier("voice-error")
            }
            HStack {
                Text(voice.recording ? "Slide down to keep your draft" : "Review in the composer")
                    .font(.system(size: 13)).foregroundStyle(.secondary)
                Spacer()
                if voice.recording {
                    Button { voice.stop() } label: {
                        Image(systemName: "stop.fill").frame(width: 52, height: 52)
                            .background(Color.primary, in: Circle()).foregroundStyle(.background)
                    }.accessibilityLabel("Stop recording")
                }
            }
        }.padding(24).frame(minWidth: 320).background(.background).tint(.primary)
            .presentationDetents([.height(420), .large]).presentationDragIndicator(.visible)
            .presentationCornerRadius(30)
            .task { await voice.start(demo: demo) }
            .onChange(of: scenePhase) { _, phase in
                if phase != .active && (voice.recording || !voice.text.isEmpty) { voice.stop(); keepDraft() }
            }
            .onDisappear { voice.stop(); keepDraft() }
    }
    private func keepDraft() {
        guard !committed else { return }
        committed = true; finish(voice.text)
    }
}
