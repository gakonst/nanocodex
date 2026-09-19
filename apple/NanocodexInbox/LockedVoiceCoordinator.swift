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
        case alreadyRecording, unavailable, permissions, account, staleCapture
        var errorDescription: String? {
            switch self {
            case .alreadyRecording: "A voice task is already in progress. Finish or cancel it first."
            case .unavailable: "Recording could not start. Try again from the Lock Screen."
            case .permissions: "Open the app and allow Microphone and Speech Recognition before recording from the Lock Screen."
            case .account: "Sign in in the app before recording from the Lock Screen."
            case .staleCapture: "This voice recording has already ended."
            }
        }
    }

    @MainActor
    private final class Capture {
        let id = UUID().uuidString
        let account: String
        let generation: UUID?
        let language: String
        let recorder = LockedAudioRecorder()
        var activity: Activity<LockedVoiceActivityAttributes>?
        var phase = "preparing"
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
    private var capture: Capture?
    private let model = InboxModel.shared
    private let log = Logger(subsystem: "xyz.paradigm.centaur", category: "LockedVoice")

    func start() async throws {
        log.info("Start requested: applicationState=\(UIApplication.shared.applicationState.rawValue) protectedDataAvailable=\(UIApplication.shared.isProtectedDataAvailable)")
        guard capture == nil else { throw CaptureError.alreadyRecording }
        guard !model.voice.isEngaged else { throw CaptureError.alreadyRecording }
        guard QuickVoiceRecorder.permissionsGranted else { throw CaptureError.permissions }
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { throw CaptureError.unavailable }
        let account: String
        do { account = try model.lockedVoiceAccountScope() }
        catch { throw CaptureError.account }
        let language = UserDefaults.standard.string(forKey: "quickVoice.locale") == "el-GR" ? "el-GR" : "en-US"
        let current = Capture(account: account, generation: model.connected ? model.quickVoiceGeneration : nil, language: language)
        capture = current
        do {
            // The combined recording/Live Activity intent is the intended locked path.
            // Physical-device validation is still required for OS launch eligibility.
            // The Live Activity must exist before activating the microphone.
            current.activity = try Activity.request(attributes: LockedVoiceActivityAttributes(captureID: current.id),
                content: content(current, phase: "preparing"), pushType: nil)
        } catch {
            capture = nil
            throw CaptureError.unavailable
        }
        current.recorder.onStatus = { [weak self, weak current] phase in
            guard let self, let current, self.capture === current else { return }
            self.update(current, phase: phase)
        }
        current.recorder.onAudioEnded = { [weak self, weak current] in
            guard let self, let current, self.capture === current else { return }
            self.beginCompletion(current)
        }
        current.recorder.onError = { [weak self, weak current] _ in
            guard let self, let current, self.capture === current else { return }
            self.end(current, phase: self.failurePhase(current), preserve: true)
        }
        current.recorder.onFinal = { [weak self, weak current] text in
            guard let self, let current, self.capture === current else { return }
            self.deliver(text, capture: current)
        }
        let captureID = current.id
        let center = NotificationCenter.default
        current.observations.append(center.publisher(for: AVAudioSession.interruptionNotification).sink { [weak self] notification in
            guard let value = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  value == AVAudioSession.InterruptionType.began.rawValue else { return }
            Task { @MainActor in self?.interrupt(captureID: captureID) }
        })
        current.observations.append(center.publisher(for: AVAudioSession.routeChangeNotification).sink { [weak self] notification in
            guard let value = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
                  value == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue else { return }
            Task { @MainActor in self?.interrupt(captureID: captureID) }
        })
        await current.recorder.start(locale: language, permissions: .alreadyGranted)
        guard capture === current, current.recorder.recording else {
            if capture === current { end(current, phase: failurePhase(current), preserve: true) }
            throw CaptureError.unavailable
        }
        // Network restoration starts only after audio is running, and never opens UI.
        current.restore = Task { try await model.restoreLockedVoiceAccount(scope: account) }
        current.heartbeat = Task { [weak self, weak current] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(30)) } catch { return }
                guard let self, let current, self.capture === current, current.phase == "listening" else { return }
                self.update(current, phase: "listening")
            }
        }
    }

    func finish(captureID: String) async throws {
        guard let current = capture, current.id == captureID, current.recorder.recording else { return }
        current.recorder.finish()
    }

    func cancel(captureID: String) async throws {
        guard let current = capture, current.id == captureID, current.delivery == nil else { return }
        beginCompletion(current)
        end(current, phase: "cancelled", preserve: false)
    }

    func yieldToForegroundRecording() {
        guard let current = capture, current.delivery == nil else { return }
        beginCompletion(current)
        end(current, phase: "cancelled", preserve: true)
    }

    private func interrupt(captureID: String) {
        guard let current = capture, current.id == captureID else { return }
        current.recorder.interrupt()
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
                self.end(current, phase: "sent", preserve: false)
            } catch {
                guard self.capture === current else { return }
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
        ActivityContent(state: .init(phase: phase, language: current.language), staleDate: ["preparing", "listening", "transcribing", "sending"].contains(phase) ? Date().addingTimeInterval(90) : nil)
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
        // Fence all callbacks before stopping audio; a late final can never send.
        capture = nil
        log.info("Capture ended: \(phase, privacy: .public)")
        current.recorder.onAudioEnded = nil
        current.recorder.onFinal = nil
        current.recorder.onError = nil
        current.recorder.onStatus = nil
        current.recorder.stop()
        current.heartbeat?.cancel()
        current.completionDeadline?.cancel()
        current.restore?.cancel(); current.delivery?.cancel()
        current.observations.removeAll()
        if preserve {
            model.retainLockedVoiceRecovery(current.recorder.transcript, captureID: current.id, accountScope: current.account)
        }
        let content = content(current, phase: phase)
        Task {
            await current.update?.value
            await current.activity?.end(content, dismissalPolicy: .after(Date().addingTimeInterval(15)))
            releaseBackground(current)
        }
    }
}
