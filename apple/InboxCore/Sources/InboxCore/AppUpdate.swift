import Foundation

public struct AppUpdate: Decodable, Sendable {
    public static let feedURL = URL(string: "https://nanocodex-ios-updates.gakonst.workers.dev/latest.json")!
    public let version: String
    public let build: String
    public let bundleID: String
    public let manifestURL: String
    public let publishedAt: String
    public let notes: String?

    enum CodingKeys: String, CodingKey {
        case version, build, notes
        case bundleID = "bundle_id", manifestURL = "manifest_url", publishedAt = "published_at"
    }

    public enum ValidationError: LocalizedError {
        case invalidFeed, invalidBuild
        public var errorDescription: String? {
            switch self {
            case .invalidFeed: return "The update information could not be verified. Please try again."
            case .invalidBuild: return "The app build number could not be verified."
            }
        }
    }

    private static func normalizedBuild(_ value: String) throws -> String {
        guard !value.isEmpty, value.utf8.allSatisfy({ $0 >= 48 && $0 <= 57 }) else {
            throw ValidationError.invalidBuild
        }
        let digits = value.drop(while: { $0 == "0" })
        return digits.isEmpty ? "0" : String(digits)
    }

    public static func isNewer(_ candidate: String, than installed: String) throws -> Bool {
        let left = try normalizedBuild(candidate)
        let right = try normalizedBuild(installed)
        return left.count == right.count ? left > right : left.count > right.count
    }

    public func validate() throws {
        _ = try Self.normalizedBuild(build)
        guard bundleID == "xyz.paradigm.centaur", !version.isEmpty,
              manifestURL == "https://nanocodex-ios-updates.gakonst.workers.dev/builds/\(build)/manifest.plist",
              let components = URLComponents(string: manifestURL),
              components.scheme == "https", components.host == Self.feedURL.host,
              components.user == nil, components.password == nil, components.port == nil,
              components.query == nil, components.fragment == nil else {
            throw ValidationError.invalidFeed
        }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard formatter.date(from: publishedAt) != nil || ISO8601DateFormatter().date(from: publishedAt) != nil else {
            throw ValidationError.invalidFeed
        }
    }

    public func installationURL(installedBuild: String) throws -> URL? {
        try validate()
        guard try Self.isNewer(build, than: installedBuild) else { return nil }
        var components = URLComponents()
        components.scheme = "itms-services"
        components.host = ""
        components.queryItems = [URLQueryItem(name: "action", value: "download-manifest"), URLQueryItem(name: "url", value: manifestURL)]
        guard let url = components.url else { throw ValidationError.invalidFeed }
        return url
    }
}
