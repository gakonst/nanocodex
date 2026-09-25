import ActivityKit
import AVFoundation
import Combine
import InboxCore
import OSLog
import UIKit

/// App-process owner for a recording launched by a Control Widget while the phone
/// is locked. The scene and the intent can both disappear without ending capture.
@MainActor
final class MeetingLockedCoordinator {
    static let shared = MeetingLockedCoordinator()

    enum CaptureError: LocalizedError {
        case busy, permissions, account, unavailable, stale, notReady, empty
        var errorDescription: String? {
            switch self {
            case .busy: "Finish or discard the current recording first."
            case .permissions: "Open Nanocodex once to grant Microphone and Speech Recognition access."
            case .account: "Sign in to Nanocodex before recording from the Lock Screen."
            case .unavailable: "The microphone or Live Activity is unavailable."
            case .stale: "That recording is no longer available."
            case .notReady: "Stop recording and wait for transcription before sending."
            case .empty: "No speech was recognized. Discard and try recording again."
            }
        }
    }

    private struct ReadySnapshot: Codable {
        let id: String
        let account: String
        let transcript: String
        let warning: Bool
        let ready: Bool
    }
    private static let scopePointerKey = "inbox.lockedMeeting.activeScope"
    private static func snapshotKey(_ scope: String) -> String { "inbox.lockedMeeting.ready." + scope }

    @MainActor private final class Capture {
        let id = UUID().uuidString
        let account: String
        let recorder = MeetingRecorder()
        var activity: Activity<MeetingLockedActivityAttributes>?
        var observers: [AnyCancellable] = []
        var ticker: Task<Void, Never>?
        var pendingUpdate: Task<Void, Never>?
        var background: UIBackgroundTaskIdentifier = .invalid
        var phase = "preparing"
        var stopping = false
        var lastCheckpoint = Date.distantPast
        init(account: String) { self.account = account }
    }
    private var capture: Capture?
    private var sending: (id: String, task: Task<Void, Error>)?
    private let model = InboxModel.shared
    private let log = Logger(subsystem: "xyz.paradigm.centaur", category: "MeetingLocked")

    func start() async throws {
        // A killed app may leave a partial checkpoint but no active microphone.
        // Preserve it as an account-scoped draft and allow a fresh locked capture.
        if savedSnapshot?.ready == false { recoverOutstanding() }
        guard capture == nil, sending == nil, savedSnapshot == nil,
              QuickVoiceRecorder.audioOwner == nil, !model.voice.isEngaged else { throw CaptureError.busy }
        guard QuickVoiceRecorder.permissionsGranted else { throw CaptureError.permissions }
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { throw CaptureError.unavailable }
        let account: String
        do { account = try model.lockedVoiceAccountScope() }
        catch { throw CaptureError.account }
        // An OS termination may have left a stale recording activity with no
        // owning audio process. Do not display a second apparently live mic.
        for orphan in Activity<MeetingLockedActivityAttributes>.activities {
            await orphan.end(content(phase: "failed", seconds: 0), dismissalPolicy: .immediate)
        }
        let current = Capture(account: account)
        capture = current
        do {
            current.activity = try Activity.request(
                attributes: MeetingLockedActivityAttributes(captureID: current.id),
                content: content(phase: "preparing", seconds: 0), pushType: nil)
        } catch {
            capture = nil
            throw CaptureError.unavailable
        }
        let id = current.id
        current.observers.append(current.recorder.$reviewing.dropFirst().sink { [weak self] ready in
            guard ready else { return }
            Task { @MainActor in self?.becameReady(id: id) }
        })
        current.observers.append(current.recorder.$transcript.dropFirst().sink { [weak self] text in
            Task { @MainActor in self?.checkpoint(id: id, text: text) }
        })
        current.observers.append(NotificationCenter.default.publisher(for: AVAudioSession.interruptionNotification).sink { [weak self] notification in
            guard let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  type == AVAudioSession.InterruptionType.began.rawValue else { return }
            Task { @MainActor in self?.interrupt(id: id) }
        })
        current.observers.append(NotificationCenter.default.publisher(for: AVAudioSession.routeChangeNotification).sink { [weak self] notification in
            guard let reason = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
                  reason == AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue else { return }
            Task { @MainActor in self?.interrupt(id: id) }
        })
        let locale = UserDefaults.standard.string(forKey: "quickVoice.locale") == "el-GR" ? "el-GR" : "en-US"
        await current.recorder.start(locale: locale, permissionsGranted: true)
        guard capture === current, current.recorder.recording else {
            // A failed startup cannot leave an apparently active Lock Screen control.
            finishCapture(current, phase: "failed")
            throw CaptureError.unavailable
        }
        update(current, phase: "listening")
        current.ticker = Task { [weak self, weak current] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(10)) } catch { return }
                guard let self, let current, self.capture === current else { return }
                if current.recorder.recording { self.update(current, phase: "listening") }
                else if current.recorder.reviewing { self.becameReady(id: current.id); return }
                else if !current.stopping {
                    self.beginFinishing(current)
                    current.recorder.interrupt("Recording stopped. Review the partial transcript before sending.")
                    return
                }
            }
        }
    }

    func finishAndSend(captureID: String) async throws {
        try await stop(captureID: captureID)
        try await send(captureID: captureID)
    }

    private func stop(captureID: String) async throws {
        guard let current = capture, current.id == captureID else {
            if let snapshot = savedSnapshot, snapshot.id == captureID, snapshot.ready { return }
            throw CaptureError.stale
        }
        if current.recorder.recording {
            beginFinishing(current)
            current.recorder.finish()
            update(current, phase: "transcribing")
        }
        // Keep this AudioRecordingIntent alive for the bounded final Speech results.
        for _ in 0..<120 {
            if current.recorder.reviewing { becameReady(id: captureID); return }
            guard capture === current else { throw CaptureError.stale }
            try await Task.sleep(for: .milliseconds(100))
        }
        current.recorder.interrupt("Transcription timed out. Review the partial transcript before sending.")
        throw CaptureError.notReady
    }

    func send(captureID: String) async throws {
        if let sending, sending.id == captureID { return try await sending.task.value }
        guard let snapshot = savedSnapshot, snapshot.id == captureID, snapshot.ready else {
            if capture?.id == captureID { throw CaptureError.notReady }
            throw CaptureError.stale
        }
        guard let text = QuickVoiceInput.finalText(snapshot.transcript) else { throw CaptureError.empty }
        let task = Task { [self] in
            let activeCapture = capture?.id == captureID ? capture : nil
            if let activeCapture { beginDelivery(activeCapture) }
            defer { if let activeCapture { releaseBackground(activeCapture) } }
            do {
                let scope = try model.lockedVoiceAccountScope()
                guard scope == snapshot.account else { throw CaptureError.account }
                await activity(for: captureID)?.update(content(phase: "sending", seconds: 0))
                try await model.restoreLockedVoiceAccount(scope: scope)
                try await model.submitLockedVoice(text, captureID: captureID,
                                                  accountScope: scope, generation: model.quickVoiceGeneration)
                // The cloud admission is persisted before any success is shown.
                if savedSnapshot?.id == captureID { clearSnapshot(id: captureID) }
                if let current = capture, current.id == captureID { finishCapture(current, phase: "sent") }
                else { await activity(for: captureID)?.end(content(phase: "sent", seconds: 0), dismissalPolicy: .after(Date().addingTimeInterval(15))) }
            } catch {
                // Never retry a possibly admitted write under a different UUID.
                // InboxModel retains the account-scoped draft/queue with this ID.
                model.retainLockedVoiceRecovery(snapshot.transcript, captureID: captureID,
                                                accountScope: snapshot.account)
                if let current = capture, current.id == captureID { update(current, phase: "ready", warning: true) }
                else { await activity(for: captureID)?.update(content(phase: "ready", seconds: 0, warning: true)) }
                throw error
            }
        }
        sending = (captureID, task)
        defer { if sending?.id == captureID { sending = nil } }
        try await task.value
    }

    /// The scene's foreground entry hands an orphaned or failed Lock Screen
    /// capture to the ordinary account-scoped recovery draft. Never display it
    /// to a different account; the Live Activity itself contains no text.
    func recoverOutstanding() {
        guard let snapshot = savedSnapshot, capture?.id != snapshot.id,
              sending?.id != snapshot.id,
              (try? model.lockedVoiceAccountScope()) == snapshot.account else { return }
        model.retainLockedVoiceRecovery(snapshot.transcript, captureID: snapshot.id,
                                        accountScope: snapshot.account)
        clearSnapshot(id: snapshot.id)
        Task { await activity(for: snapshot.id)?.end(content(phase: "failed", seconds: 0, warning: true),
                                                     dismissalPolicy: .immediate) }
    }

    private func checkpoint(id: String, text: String) {
        guard let current = capture, current.id == id, current.recorder.recording,
              Date().timeIntervalSince(current.lastCheckpoint) >= 5,
              let text = QuickVoiceInput.finalText(text) else { return }
        current.lastCheckpoint = Date()
        // One overwritten checkpoint, never a growing collection of audio files.
        // A process eviction can still lose the most recent uncheckpointed words.
        let snapshot = ReadySnapshot(id: id, account: current.account, transcript: text,
                                     warning: true, ready: false)
        storeSnapshot(snapshot)
    }

    private var savedSnapshot: ReadySnapshot? {
        guard let scope = UserDefaults.standard.string(forKey: Self.scopePointerKey),
              let data = UserDefaults.standard.data(forKey: Self.snapshotKey(scope)),
              let snapshot = try? JSONDecoder().decode(ReadySnapshot.self, from: data),
              snapshot.account == scope else { return nil }
        return snapshot
    }

    private func storeSnapshot(_ snapshot: ReadySnapshot) {
        guard let data = try? JSONEncoder().encode(snapshot) else { return }
        UserDefaults.standard.set(data, forKey: Self.snapshotKey(snapshot.account))
        UserDefaults.standard.set(snapshot.account, forKey: Self.scopePointerKey)
    }

    private func clearSnapshot(id: String) {
        guard let snapshot = savedSnapshot, snapshot.id == id else { return }
        UserDefaults.standard.removeObject(forKey: Self.snapshotKey(snapshot.account))
        UserDefaults.standard.removeObject(forKey: Self.scopePointerKey)
    }

    private func activity(for id: String) -> Activity<MeetingLockedActivityAttributes>? {
        if capture?.id == id { return capture?.activity }
        return Activity<MeetingLockedActivityAttributes>.activities.first { $0.attributes.captureID == id }
    }

    private func interrupt(id: String) {
        guard let current = capture, current.id == id, current.recorder.recording else { return }
        beginFinishing(current)
        current.recorder.interrupt()
        update(current, phase: "transcribing", warning: true)
    }

    private func beginFinishing(_ current: Capture) {
        guard !current.stopping else { return }
        current.stopping = true
        current.background = UIApplication.shared.beginBackgroundTask(withName: "Finish meeting transcription") { [weak self, weak current] in
            Task { @MainActor in
                guard let self, let current, self.capture === current else { return }
                self.becameReady(id: current.id, force: true)
            }
        }
    }

    private func beginDelivery(_ current: Capture) {
        guard current.background == .invalid else { return }
        current.background = UIApplication.shared.beginBackgroundTask(withName: "Send meeting transcript") { [weak self, weak current] in
            Task { @MainActor in
                guard let self, let current, self.capture === current else { return }
                self.model.retainLockedVoiceRecovery(current.recorder.transcript, captureID: current.id,
                                                     accountScope: current.account)
                self.releaseBackground(current)
            }
        }
    }

    private func becameReady(id: String, force: Bool = false) {
        guard let current = capture, current.id == id,
              current.recorder.reviewing || force else { return }
        current.ticker?.cancel(); current.ticker = nil
        let warning = current.recorder.status.contains("partial") || current.recorder.status.contains("timed out") || force
        if let text = QuickVoiceInput.finalText(current.recorder.transcript) {
            let snapshot = ReadySnapshot(id: id, account: current.account, transcript: text,
                                         warning: warning, ready: true)
            storeSnapshot(snapshot)
            update(current, phase: "ready", warning: warning)
        } else {
            if savedSnapshot?.id == id { clearSnapshot(id: id) }
            finishCapture(current, phase: "failed")
        }
        releaseBackground(current)
    }

    private func releaseBackground(_ current: Capture) {
        if current.background != .invalid {
            UIApplication.shared.endBackgroundTask(current.background)
            current.background = .invalid
        }
    }

    private func content(phase: String, seconds: Int, warning: Bool = false) -> ActivityContent<MeetingLockedActivityAttributes.ContentState> {
        ActivityContent(state: .init(phase: phase, seconds: seconds, warning: warning),
                        staleDate: ["preparing", "listening", "transcribing", "sending"].contains(phase) ? Date().addingTimeInterval(90) : nil)
    }

    private func update(_ current: Capture, phase: String, warning: Bool = false) {
        guard capture === current else { return }
        current.phase = phase
        let previous = current.pendingUpdate
        let next = content(phase: phase, seconds: current.recorder.seconds, warning: warning)
        current.pendingUpdate = Task { [weak current] in
            await previous?.value
            await current?.activity?.update(next)
        }
    }

    private func finishCapture(_ current: Capture, phase: String) {
        guard capture === current else { return }
        capture = nil // Fence all recognizer callbacks before tearing down audio.
        current.ticker?.cancel()
        current.observers.removeAll()
        let elapsed = current.recorder.seconds
        current.recorder.discard()
        let previous = current.pendingUpdate
        let activity = current.activity
        let final = content(phase: phase, seconds: elapsed)
        Task { await previous?.value; await activity?.end(final, dismissalPolicy: .after(Date().addingTimeInterval(15))) }
        releaseBackground(current)
        log.info("Meeting capture ended: \(phase, privacy: .public)")
    }
}
