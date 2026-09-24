import Foundation
#if canImport(ActivityKit) && os(iOS)
import ActivityKit

/// Only capture state is visible while locked; dictated text stays in the app.
public struct LockedVoiceActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable, Sendable {
        public var phase: String
        public var language: String
        /// Privacy-safe failure reason; never contains dictated speech or account data.
        public var failure: String?
        public init(phase: String, language: String, failure: String? = nil) {
            self.phase = phase
            self.language = language
            self.failure = failure
        }
    }
    public var captureID: String
    public init(captureID: String) { self.captureID = captureID }
}
#endif
