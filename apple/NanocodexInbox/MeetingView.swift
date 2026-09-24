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
                    Button("Finish meeting") { recorder.finish() }
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("meeting-finish")
                } else if !recorder.working {
                    HStack {
                        Button(recorder.reviewing ? "Record new meeting" : "Start listening") {
                            Task { await start() }
                        }
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("meeting-start")
                        if recorder.reviewing {
                            Button("Send in new conversation") { submit() }
                                .disabled(QuickVoiceInput.finalText(recorder.transcript) == nil || !model.connected || model.isDemo || account != model.quickVoiceGeneration || scenePhase != .active)
                                .accessibilityIdentifier("meeting-send")
                        }
                    }
                } else if !recorder.recording {
                    ProgressView("Finishing segments…")
                }
                Text("Recording can continue while the screen is locked. Speech is processed in short segments; it may require a network connection. Nothing is sent until you finish, review, and tap Send. If recording is interrupted, review the partial transcript for missing words.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding()
            .navigationTitle("Meeting listening")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { recorder.discard(); dismiss() }
                }
            }
        }
        .interactiveDismissDisabled(recorder.working)
        .onChange(of: model.connected) { _, connected in
            if !connected { recorder.interrupt("Account disconnected. Review your partial transcript; sign in before sending.") }
        }
        .onChange(of: model.quickVoiceGeneration) { _, generation in
            if let account, generation != account {
                recorder.interrupt("Account changed. This transcript cannot be sent from a different account.")
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification)) { notification in
            guard let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  type == AVAudioSession.InterruptionType.began.rawValue else { return }
            recorder.interrupt()
        }
        .onReceive(NotificationCenter.default.publisher(for: AVAudioSession.routeChangeNotification)) { notification in
            if let reason = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
               reason == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue { recorder.interrupt() }
        }
        .onDisappear { visible = false; recorder.discard() }
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
