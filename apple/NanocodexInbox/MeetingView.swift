import AVFoundation
import InboxCore
import SwiftUI

struct MeetingView: View {
    @ObservedObject var model: InboxModel
    @StateObject private var recorder = MeetingRecorder()
    @AppStorage("quickVoice.locale") private var locale = "en-US"
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var account: UUID?
    @State private var targetID: String?
    @State private var submitted = false
    @State private var stopRequested = false
    @State private var sendError: String?
    @State private var visible = true

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                Picker("Speech language", selection: $locale) {
                    Text("English").tag("en-US")
                    Text("Ελληνικά").tag("el-GR")
                }
                .pickerStyle(.segmented)
                .disabled(recorder.working)
                Text(recorder.status)
                    .font(.subheadline)
                    .accessibilityIdentifier("meeting-status")
                if recorder.recording {
                    Label("Recording · \(Duration.seconds(recorder.seconds).formatted())", systemImage: "mic.fill")
                        .foregroundStyle(.red)
                        .accessibilityIdentifier("meeting-recording-indicator")
                }
                if let sendError { Text(sendError).font(.caption).foregroundStyle(.red) }
                Text("Live transcript")
                    .font(.headline)
                TextEditor(text: Binding(get: { recorder.transcript }, set: { recorder.edit($0) }))
                    .disabled(recorder.working || !recorder.reviewing)
                    .frame(minHeight: 220)
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(.secondary.opacity(0.3)))
                    .accessibilityIdentifier("meeting-transcript")
                if recorder.recording {
                    Button("Stop Recording") { stopRequested = true; recorder.finish() }
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("meeting-finish")
                } else if !recorder.working {
                    if recorder.reviewing, QuickVoiceInput.finalText(recorder.transcript) != nil {
                        Button(sendError == nil ? "Start agent with transcript" : "Retry starting agent") { submit() }
                            .disabled(!model.connected || model.isDemo || account != model.quickVoiceGeneration || scenePhase != .active)
                            .accessibilityIdentifier("meeting-send")
                    } else {
                        Button("Start listening") { Task { await start() } }
                            .buttonStyle(.borderedProminent)
                            .accessibilityIdentifier("meeting-start")
                    }
                } else if !recorder.recording {
                    ProgressView("Finishing segments…")
                }
                Text("Recording continues through pauses. Stop Recording transcribes and starts one agent thread. An interrupted transcript stays available for review; nothing partial sends automatically.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding()
            .navigationTitle("Meeting listening")
        }
        .interactiveDismissDisabled(recorder.working)
        .onChange(of: recorder.reviewing) { _, ready in
            if ready && stopRequested {
                stopRequested = false
                if !recorder.completedWithWarning { submit() }
            }
        }
        .onChange(of: model.connected) { _, connected in
            if !connected { stopRequested = false; recorder.interrupt("Account disconnected. Review your partial transcript; sign in before sending.") }
        }
        .onChange(of: model.quickVoiceGeneration) { _, generation in
            if let account, generation != account {
                stopRequested = false
                recorder.interrupt("Account changed. This transcript cannot be sent from a different account.")
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification)) { notification in
            guard let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  type == AVAudioSession.InterruptionType.began.rawValue else { return }
            stopRequested = false; recorder.interrupt()
        }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.routeChangeNotification)) { notification in
            if let reason = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
               reason == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue {
                stopRequested = false; recorder.interrupt()
            }
        }
        .onDisappear {
            visible = false
            if !submitted, let text = QuickVoiceInput.finalText(recorder.transcript),
               let scope = try? model.lockedVoiceAccountScope() {
                model.retainLockedVoiceRecovery(text, captureID: UUID().uuidString, accountScope: scope)
            }
            recorder.discard()
        }
    }

    private func start() async {
        guard visible, scenePhase == .active, !submitted, !recorder.working else { return }
        guard model.connected, !model.isDemo, !model.restoringAccount else { return }
        guard QuickVoiceRecorder.audioOwner == nil else {
            // Recorder reports the competing capture as well; do not stop its mic.
            await recorder.start(locale: locale)
            return
        }
        LockedVoiceCoordinator.shared.yieldToForegroundRecording()
        model.voice.stop()
        account = model.quickVoiceGeneration
        targetID = nil
        sendError = nil
        stopRequested = false
        await recorder.start(locale: locale == "el-GR" ? "el-GR" : "en-US")
    }

    private func submit() {
        guard !submitted, visible, scenePhase == .active, recorder.reviewing, !recorder.working,
              let text = QuickVoiceInput.finalText(recorder.transcript), let account else { return }
        guard model.connected, !model.isDemo, account == model.quickVoiceGeneration else { return }
        guard model.sendQuickVoice(text, generation: account, targetID: &targetID) else {
            sendError = model.error ?? "Could not start the conversation. Your transcript is still here; try sending again."
            return
        }
        submitted = true
        dismiss()
    }
}
