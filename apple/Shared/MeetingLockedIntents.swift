import AppIntents
import Foundation

/// Start, then explicitly finish and send without opening the scene. System policy
/// ultimately decides whether an AudioRecordingIntent can launch while locked.
struct StartMeetingLockedIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Listen to a meeting"
    static var description = IntentDescription("Record a meeting in short segments until Finish & Send. Grant permissions in the app first.")
    static var openAppWhenRun = false
    static var authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { .background }
    @MainActor func perform() async throws -> some IntentResult {
        #if NANOCODEX_WIDGET_EXTENSION
        throw MeetingIntentError.appProcessRequired
        #else
        try await MeetingLockedCoordinator.shared.start()
        #endif
        return .result()
    }
}

struct FinishMeetingLockedIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Finish and send meeting"
    static var openAppWhenRun = false
    static var authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { .background }
    @Parameter(title: "Recording") var captureID: String
    init() {}
    init(captureID: String) { self.captureID = captureID }
    @MainActor func perform() async throws -> some IntentResult {
        #if NANOCODEX_WIDGET_EXTENSION
        throw MeetingIntentError.appProcessRequired
        #else
        try await MeetingLockedCoordinator.shared.finishAndSend(captureID: captureID)
        #endif
        return .result()
    }
}

struct SendMeetingLockedIntent: LiveActivityIntent {
    static var title: LocalizedStringResource = "Retry sending meeting transcript"
    static var openAppWhenRun = false
    static var authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed
    @available(iOS 26.0, *)
    static var supportedModes: IntentModes { .background }
    @Parameter(title: "Recording") var captureID: String
    init() {}
    init(captureID: String) { self.captureID = captureID }
    @MainActor func perform() async throws -> some IntentResult {
        #if NANOCODEX_WIDGET_EXTENSION
        throw MeetingIntentError.appProcessRequired
        #else
        try await MeetingLockedCoordinator.shared.send(captureID: captureID)
        #endif
        return .result()
    }
}

private enum MeetingIntentError: LocalizedError {
    case appProcessRequired
    var errorDescription: String? { "Meeting capture requires Nanocodex's background process. Open the app once to complete setup." }
}
