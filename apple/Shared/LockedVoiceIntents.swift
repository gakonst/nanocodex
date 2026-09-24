import AppIntents
import Foundation

/// Bounded, transcript-free device breadcrumbs. The app and widget each write in
/// their own container, readable with devicectl even when Console disconnects.
@MainActor
enum VoiceDiagnostic {
    static func note(_ event: String, error: Error? = nil) {
        let code: String
        if let error {
            let ns = error as NSError
            code = " domain=\(ns.domain) code=\(ns.code)"
        } else { code = "" }
        let process: String
        #if NANOCODEX_WIDGET_EXTENSION
        process = "widget"
        #else
        process = "app"
        #endif
        let line = "\(Date().timeIntervalSince1970) \(process) \(event)\(code)\n"
        guard let root = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first?
            .appendingPathComponent("voice-diagnostics", isDirectory: true),
            let bytes = line.data(using: .utf8) else { return }
        let url = root.appendingPathComponent("breadcrumbs.txt")
        do {
            let protection: [FileAttributeKey: Any] = [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication]
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true, attributes: protection)
            try FileManager.default.setAttributes(protection, ofItemAtPath: root.path)
            let previous = (try? Data(contentsOf: url)) ?? Data()
            // Keep at most the last 16 KiB; never store speech or account data.
            let tail = previous.suffix(16_000)
            try (Data(tail) + bytes).write(to: url, options: .atomic)
            try FileManager.default.setAttributes(protection, ofItemAtPath: url.path)
        } catch { /* Device logs remain available if this diagnostic write fails. */ }
    }
}

/// System-dispatched recording runs in the app process without presenting its UI.
/// LiveActivityIntent permits the required recording activity to start in the background.
struct StartLockedVoiceIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Record a voice task"
    static var description = IntentDescription("Record in English or Greek and send a new agent task. Set up microphone and speech access in Nanocodex first.")
    static var openAppWhenRun = false
    static var authenticationPolicy: IntentAuthenticationPolicy = .requiresLocalDeviceAuthentication
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { .background }

    @MainActor func perform() async throws -> some IntentResult {
        VoiceDiagnostic.note("speak.intent.enter")
        #if NANOCODEX_WIDGET_EXTENSION
        VoiceDiagnostic.note("speak.intent.widgetRejected")
        throw LockedVoiceIntentError.appProcessRequired
        #else
        do { try await LockedVoiceCoordinator.shared.start() }
        catch { VoiceDiagnostic.note("speak.intent.failed", error: error); throw error }
        VoiceDiagnostic.note("speak.intent.started")
        #endif
        return .result()
    }
}

struct FinishLockedVoiceIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Finish voice task"
    static var openAppWhenRun = false
    static var authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { .background }
    @Parameter(title: "Recording") var captureID: String
    init() {}
    init(captureID: String) { self.captureID = captureID }
    @MainActor func perform() async throws -> some IntentResult {
        #if NANOCODEX_WIDGET_EXTENSION
        throw LockedVoiceIntentError.appProcessRequired
        #else
        try await LockedVoiceCoordinator.shared.finish(captureID: captureID)
        #endif
        return .result()
    }
}

struct CancelLockedVoiceIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Cancel voice task"
    static var openAppWhenRun = false
    static var authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { .background }
    @Parameter(title: "Recording") var captureID: String
    init() {}
    init(captureID: String) { self.captureID = captureID }
    @MainActor func perform() async throws -> some IntentResult {
        #if NANOCODEX_WIDGET_EXTENSION
        throw LockedVoiceIntentError.appProcessRequired
        #else
        try await LockedVoiceCoordinator.shared.cancel(captureID: captureID)
        #endif
        return .result()
    }
}

private enum LockedVoiceIntentError: LocalizedError {
    case appProcessRequired
    var errorDescription: String? { "Recording must run in Nanocodex's background process. Open Nanocodex once to finish setup." }
}
