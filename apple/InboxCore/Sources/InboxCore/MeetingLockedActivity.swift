import Foundation
#if canImport(ActivityKit) && os(iOS)
import ActivityKit

/// No dictated text or account identifier is exposed to the Lock Screen.
public struct MeetingLockedActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable, Sendable {
        public var phase: String
        public var seconds: Int
        public var warning: Bool
        public init(phase: String, seconds: Int = 0, warning: Bool = false) {
            self.phase = phase; self.seconds = seconds; self.warning = warning
        }
    }
    public let captureID: String
    public init(captureID: String) { self.captureID = captureID }
}
#endif
