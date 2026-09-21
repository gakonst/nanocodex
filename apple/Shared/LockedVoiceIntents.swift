import AppIntents
import Foundation

/// System-dispatched recording runs in the app process without presenting its UI.
/// LiveActivityIntent permits the required recording activity to start in the background.
struct StartLockedVoiceIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Record a voice task"
    static var description = IntentDescription("Record in English or Greek and send a new agent task. Set up microphone and speech access in Nanocodex first.")
    static var openAppWhenRun = false
    static var authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { .background }

    @MainActor func perform() async throws -> some IntentResult {
        #if NANOCODEX_WIDGET_EXTENSION
        throw LockedVoiceIntentError.appProcessRequired
        #else
        try await LockedVoiceCoordinator.shared.start()
        #endif
        return .result()
    }
}

struct FinishLockedVoiceIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Finish voice task"
    static var openAppWhenRun = false
    static var authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed
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
