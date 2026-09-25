import Foundation
#if canImport(ActivityKit) && os(iOS)
import ActivityKit

/// The optional recap is explicitly privacy-sensitive in the widget; never put
/// raw dictated text or an account identifier in this ActivityKit state.
public struct MeetingLockedActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable, Sendable {
        public var phase: String
        public var seconds: Int
        public var warning: Bool
        /// Short, model-generated recap; WidgetKit redacts it on protected screens.
        public var recap: String?
        public init(phase: String, seconds: Int = 0, warning: Bool = false, recap: String? = nil) {
            self.phase = phase; self.seconds = seconds; self.warning = warning; self.recap = recap
        }
    }
    public let captureID: String
    public init(captureID: String) { self.captureID = captureID }
}
#endif
