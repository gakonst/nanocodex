import ActivityKit
import AVFoundation
import Combine
import InboxCore
import OSLog
import UIKit

/// Owns capture independently of a scene, sheet, or the short-lived intent instance.
@MainActor
final class LockedVoiceCoordinator {
    static let shared = LockedVoiceCoordinator()

    enum CaptureError: LocalizedError {
        case alreadyRecording, unavailable, permissions, account, staleCapture, completionFailed
        var errorDescription: String? {
            switch self {
            case .alreadyRecording: "A voice task is already in progress. Finish or cancel it first."
            case .unavailable: "Recording could not start. Try again from the Lock Screen."
            case .permissions: "Open the app and allow Microphone and Speech Recognition before recording from the Lock Screen."
            case .account: "Sign in in the app before recording from the Lock Screen."
            case .staleCapture: "This voice recording has already ended."
            case .completionFailed: "The voice task could not be sent. Any available transcript is saved in Nanocodex."
            }
        }
    }

    @MainActor
    private final class Capture {
        let id = UUID().uuidString
        let account: String
        let generation: UUID?
        let language: String
        // Continuous microphone stream with rotating speech segments. A pause
        // finalizes one segment, never the recording or a cloud turn.
        let recorder = MeetingRecorder()
        var stopRequested = false
        var startedAt: Date?
        var lastCheckpoint = Date.distantPast
        var sawSpeech = false
        let completion = LockedVoiceCompletion()
        var activity: Activity<LockedVoiceActivityAttributes>?
        var phase = "preparing"
        var failure: String?
        var restore: Task<Void, Error>?
        var delivery: Task<Void, Never>?
        var heartbeat: Task<Void, Never>?
        var completionDeadline: Task<Void, Never>?
        var update: Task<Void, Never>?
        var background: UIBackgroundTaskIdentifier = .invalid
        var observations: [AnyCancellable] = []
        init(account: String, generation: UUID?, language: String) {
            self.account = account; self.generation = generation; self.language = language
        }
    }
    private struct PartialSnapshot: Codable {
        let id: String
        let account: String
        let text: String
    }
    private static func snapshotKey(_ scope: String) -> String { "inbox.lockedVoice.capture." + scope }
    private var capture: Capture?
    private let model = InboxModel.shared
    private let log = Logger(subsystem: "xyz.paradigm.centaur", category: "LockedVoice")

    func start() async throws {
        VoiceDiagnostic.note("speak.coordinator.enter.state-\(UIApplication.shared.applicationState.rawValue).protected-\(UIApplication.shared.isProtectedDataAvailable)")
        log.info("Start requested: applicationState=\(UIApplication.shared.applicationState.rawValue) protectedDataAvailable=\(UIApplication.shared.isProtectedDataAvailable)")
        guard capture == nil else { VoiceDiagnostic.note("speak.coordinator.busy"); throw CaptureError.alreadyRecording }
        guard !model.voice.isEngaged else { VoiceDiagnostic.note("speak.coordinator.otherVoiceBusy"); throw CaptureError.alreadyRecording }
        guard QuickVoiceRecorder.permissionsGranted else { VoiceDiagnostic.note("speak.coordinator.permissionsDenied"); throw CaptureError.permissions }
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { VoiceDiagnostic.note("speak.coordinator.activityDisabled"); throw CaptureError.unavailable }
        // A prior app process may have died after publishing a terminal failure (or
        // before ending an activity). Those cards can outlive the process and obscure
        // a new successful capture on the Lock Screen.
        for orphan in Activity<LockedVoiceActivityAttributes>.activities {
            VoiceDiagnostic.note("speak.coordinator.retiringOrphan")
            await orphan.end(nil, dismissalPolicy: .immediate)
        }
        let account: String
        do { account = try model.lockedVoiceAccountScope() }
        catch { VoiceDiagnostic.note("speak.coordinator.accountUnavailable", error: error); throw CaptureError.account }
        // Recover a prior process's unsent partial as a draft, never as a turn.
        if let data = UserDefaults.standard.data(forKey: Self.snapshotKey(account)),
           let old = try? JSONDecoder().decode(PartialSnapshot.self, from: data), old.account == account {
            model.retainLockedVoiceRecovery(old.text, captureID: old.id, accountScope: account)
            UserDefaults.standard.removeObject(forKey: Self.snapshotKey(account))
        }
        let language = UserDefaults.standard.string(forKey: "quickVoice.locale") == "el-GR" ? "el-GR" : "en-US"
        let current = Capture(account: account, generation: model.connected ? model.quickVoiceGeneration : nil, language: language)
        capture = current
        do {
            // The combined recording/Live Activity intent is the intended locked path.
            // Physical-device validation is still required for OS launch eligibility.
            // The Live Activity must exist before activating the microphone.
            VoiceDiagnostic.note("speak.coordinator.activityRequest")
            current.activity = try Activity.request(attributes: LockedVoiceActivityAttributes(captureID: current.id),
                content: content(current, phase: "preparing"), pushType: nil)
            VoiceDiagnostic.note("speak.coordinator.activityStarted")
        } catch {
            VoiceDiagnostic.note("speak.coordinator.activityFailed", error: error)
            capture = nil
            throw CaptureError.unavailable
        }
        // Stream microphone buffers to bounded recognition segments continuously.
        // Only an explicit Stop Recording can admit the finished transcript.
        current.observations.append(current.recorder.$reviewing.dropFirst().sink { [weak self, weak current] ready in
            guard ready else { return }
            Task { @MainActor in
                guard let self, let current, self.capture === current else { return }
                self.beginCompletion(current)
                if current.stopRequested, let text = QuickVoiceInput.finalText(current.recorder.transcript) {
                    VoiceDiagnostic.note("speak.coordinator.finalTextReady")
                    self.deliver(text, capture: current)
                } else {
                    current.failure = current.stopRequested ? "No speech heard" : "Recording stopped"
                    self.end(current, phase: self.failurePhase(current), preserve: true)
                }
            }
        })
        current.observations.append(current.recorder.$transcript.dropFirst().sink { [weak self, weak current] text in
            guard let self, let current, self.capture === current else { return }
            self.checkpoint(current, text: text)
        })
        let captureID = current.id
        let center = NotificationCenter.default
        current.observations.append(center.publisher(for: AVAudioSession.interruptionNotification).sink { [weak self] notification in
            guard let value = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  value == AVAudioSession.InterruptionType.began.rawValue else { return }
            Task { @MainActor in
                self?.log.warning("Audio interruption began for capture \(captureID, privacy: .public)")
                self?.interrupt(captureID: captureID)
            }
        })
        current.observations.append(center.publisher(for: AVAudioSession.routeChangeNotification).sink { [weak self] notification in
            guard let value = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
                  value == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue else { return }
            Task { @MainActor in
                self?.log.warning("Audio input route lost for capture \(captureID, privacy: .public)")
                self?.interrupt(captureID: captureID)
            }
        })
        // Restore in parallel, never before or instead of microphone startup. An
        // unusually fast final callback still has a task to await for delivery.
        current.restore = Task { try await model.restoreLockedVoiceAccount(scope: account) }
        VoiceDiagnostic.note("speak.coordinator.recorderStarting")
        await current.recorder.start(locale: language, permissionsGranted: true)
        VoiceDiagnostic.note("speak.coordinator.recorderReturned.active-\(current.recorder.recording)")
        guard capture === current, current.recorder.recording else {
            if capture === current { end(current, phase: failurePhase(current), preserve: true) }
            throw CaptureError.unavailable
        }
        current.startedAt = Date()
        self.update(current, phase: "listening")
        current.heartbeat = Task { [weak self, weak current] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
                guard let self, let current, self.capture === current, current.phase == "listening" else { return }
                self.update(current, phase: "listening")
            }
        }
    }

    func finish(captureID: String) async throws {
        guard let current = capture, current.id == captureID else { throw CaptureError.staleCapture }
        // Stop recording exactly once. Segments continue finalizing after the mic
        // closes, then the completed transcript is admitted under this capture ID.
        // Duplicate taps join the same completion; they never create another turn.
        if current.recorder.recording {
            current.stopRequested = true
            checkpoint(current, text: current.recorder.transcript, force: true)
            beginCompletion(current)
            update(current, phase: "transcribing")
            VoiceDiagnostic.note("speak.coordinator.stopRequested")
            current.recorder.finish()
        }
        try await current.completion.wait()
    }

    func yieldToForegroundRecording() {
        guard let current = capture, current.delivery == nil else { return }
        beginCompletion(current)
        end(current, phase: "cancelled", preserve: true)
    }

    private func interrupt(captureID: String) {
        guard let current = capture, current.id == captureID else { return }
        beginCompletion(current)
        current.recorder.interrupt()
    }

    private func checkpoint(_ current: Capture, text: String, force: Bool = false) {
        guard capture === current, let text = QuickVoiceInput.finalText(text) else { return }
        if !current.sawSpeech {
            current.sawSpeech = true
            VoiceDiagnostic.note("speak.coordinator.firstTranscript")
        }
        guard force || Date().timeIntervalSince(current.lastCheckpoint) >= 5 else { return }
        current.lastCheckpoint = Date()
        // A crash retains only the latest partial, scoped to this account. It
        // becomes an editable draft on recovery; never auto-submits unfinished audio.
        if let data = try? JSONEncoder().encode(PartialSnapshot(id: current.id, account: current.account, text: text)) {
            UserDefaults.standard.set(data, forKey: Self.snapshotKey(current.account))
        }
    }

    private func beginCompletion(_ current: Capture) {
        guard capture === current, current.background == .invalid else { return }
        current.background = UIApplication.shared.beginBackgroundTask(withName: "Finish locked voice task") { [weak self, weak current] in
            Task { @MainActor in
                guard let self, let current else { return }
                self.releaseBackground(current)
                guard self.capture === current else { return }
                self.end(current, phase: self.failurePhase(current), preserve: true)
            }
        }
        current.completionDeadline = Task { [weak self, weak current] in
            do { try await Task.sleep(for: .seconds(25)) } catch { return }
            guard let self, let current, self.capture === current else { return }
            self.end(current, phase: self.failurePhase(current), preserve: true)
        }
    }

    private func deliver(_ text: String, capture current: Capture) {
        guard capture === current, current.delivery == nil else { return }
        beginCompletion(current)
        update(current, phase: "sending")
        current.delivery = Task { [weak self, weak current] in
            guard let self, let current else { return }
            do {
                try await current.restore?.value
                try Task.checkCancellation()
                guard self.capture === current else { return }
                let epoch = current.generation ?? self.model.quickVoiceGeneration
                try await self.model.submitLockedVoice(text, captureID: current.id, accountScope: current.account, generation: epoch)
                try Task.checkCancellation()
                guard self.capture === current else { return }
                VoiceDiagnostic.note("speak.coordinator.deliveryAdmitted")
                self.end(current, phase: "sent", preserve: false)
            } catch {
                guard self.capture === current else { return }
                VoiceDiagnostic.note("speak.coordinator.deliveryFailed", error: error)
                self.end(current, phase: self.failurePhase(current), preserve: true)
            }
        }
    }

    private func failurePhase(_ current: Capture) -> String {
        switch current.phase {
        case "sending": "deliveryFailed"
        case "transcribing": "transcriptionFailed"
        default: "recordingFailed"
        }
    }

    private func content(_ current: Capture, phase: String) -> ActivityContent<LockedVoiceActivityAttributes.ContentState> {
        ActivityContent(state: .init(phase: phase, language: current.language, failure: current.failure,
                                     startedAt: current.startedAt, waveform: phase == "listening" ? current.recorder.waveform : nil),
                        staleDate: ["preparing", "listening", "transcribing", "sending"].contains(phase) ? Date().addingTimeInterval(90) : nil)
    }

    private func update(_ current: Capture, phase: String) {
        guard capture === current else { return }
        current.phase = phase
        log.info("Capture state: \(phase, privacy: .public)")
        let previous = current.update, content = content(current, phase: phase)
        current.update = Task {
            await previous?.value
            guard !Task.isCancelled else { return }
            await current.activity?.update(content)
        }
    }

    private func releaseBackground(_ current: Capture) {
        if current.background != .invalid {
            UIApplication.shared.endBackgroundTask(current.background)
            current.background = .invalid
        }
    }

    private func end(_ current: Capture, phase: String, preserve: Bool) {
        guard capture === current else { return }
        // Fence all callbacks before stopping audio; a late segment cannot send.
        capture = nil
        VoiceDiagnostic.note("speak.coordinator.ended.\(phase)")
        log.info("Capture ended: \(phase, privacy: .public)")
        let retainedTranscript = current.recorder.transcript
        current.recorder.discard()
        current.heartbeat?.cancel()
        current.completionDeadline?.cancel()
        current.restore?.cancel()
        // Successful admission has already flushed its receipt. On failure, let
        // cancellation finish persisting the queue's retry state before returning.
        if phase != "sent" { current.delivery?.cancel() }
        current.observations.removeAll()
        if preserve {
            model.retainLockedVoiceRecovery(retainedTranscript, captureID: current.id, accountScope: current.account)
        }
        if let data = UserDefaults.standard.data(forKey: Self.snapshotKey(current.account)),
           let saved = try? JSONDecoder().decode(PartialSnapshot.self, from: data), saved.id == current.id {
            UserDefaults.standard.removeObject(forKey: Self.snapshotKey(current.account))
        }
        let content = content(current, phase: phase)
        Task {
            await current.delivery?.value
            await current.update?.value
            VoiceDiagnostic.note("speak.coordinator.activityEndStarting.\(phase)")
            await current.activity?.end(content, dismissalPolicy: .after(Date().addingTimeInterval(15)))
            VoiceDiagnostic.note("speak.coordinator.activityEndFinished.\(phase)")
            releaseBackground(current)
            current.completion.resolve(phase == "sent" ? .success(()) : .failure(
                phase == "cancelled" ? CancellationError() : CaptureError.completionFailed))
        }
    }
}
