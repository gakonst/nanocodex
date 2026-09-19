import Foundation
#if canImport(ActivityKit) && os(iOS)
import ActivityKit

/// Only capture state is visible while locked; dictated text stays in the app.
public struct LockedVoiceActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable, Sendable {
        public var phase: String
        public var language: String
        public init(phase: String, language: String) {
            self.phase = phase
            self.language = language
        }
    }
    public var captureID: String
    public init(captureID: String) { self.captureID = captureID }
}
#endif
