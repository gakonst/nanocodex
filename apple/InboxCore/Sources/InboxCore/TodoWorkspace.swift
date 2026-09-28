import Foundation

public struct TodoMailAccount: Identifiable, Equatable, Sendable {
    public let id: String
    public let label: String
    public let email: String
    public init(_ json: JSON) throws {
        id = json["connection_id"].string
        guard !id.isEmpty else { throw APIError.invalidResponse }
        label = json["label"].string; email = json["email"].string
    }
}

public struct TodoMailThreadSummary: Identifiable, Codable, Equatable, Sendable {
    public let id: String
    public let connectionID: String
    public let subject: String
    public let sender: String
    public let snippet: String
    public let updatedAt: Date
    public let isUnread: Bool
    public let messageCount: Int
    public let inInbox: Bool?
    public init(_ json: JSON) throws {
        id = json["id"].string; connectionID = json["connection_id"].string
        guard !id.isEmpty, !connectionID.isEmpty else { throw APIError.invalidResponse }
        subject = json["subject"].string; sender = json["from"].string; snippet = json["snippet"].string
        updatedAt = todoDate(json["date"].string) ?? .distantPast
        if case .bool(let inbox) = json["in_inbox"] { inInbox = inbox } else { inInbox = nil }
        isUnread = json["unread"].bool; messageCount = Int(exactly: json["message_count"].number) ?? 1
    }
}
public struct TodoMailPage: Sendable {
    public let threads: [TodoMailThreadSummary]
    public let nextPageToken: String?
}

public struct TodoScheduleEvent: Identifiable, Equatable, Sendable {
    public let id: String
    public let connectionID: String
    public let calendarID: String
    public let title: String
    public let startAt: Date
    public let endAt: Date
    public let isAllDay: Bool
    public let location: String
    public let details: String
    public let url: URL?
    public init(_ json: JSON) throws {
        id = json["id"].string; connectionID = json["connection_id"].string; calendarID = json["calendar_id"].string
        guard !id.isEmpty, let start = todoDate(json["start"].string), let end = todoDate(json["end"].string) else { throw APIError.invalidResponse }
        title = json["title"].string; startAt = start; endAt = end; isAllDay = json["all_day"].bool
        location = json["location"].string; details = json["description"].string
        let link = URL(string: json["html_url"].string)
        url = link?.scheme == "https" && link?.host != nil && link?.user == nil && link?.password == nil ? link : nil
    }
}
public struct TodoSchedule: Sendable {
    public let events: [TodoScheduleEvent]
    public let partial: Bool
}
private func todoDate(_ text: String) -> Date? {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    if let date = formatter.date(from: text) { return date }
    formatter.formatOptions = [.withInternetDateTime]
    if let date = formatter.date(from: text) { return date }
    // Google all-day values have no timezone; display them on the local calendar day.
    let day = DateFormatter(); day.calendar = Calendar(identifier: .gregorian)
    day.locale = Locale(identifier: "en_US_POSIX"); day.dateFormat = "yyyy-MM-dd"; day.isLenient = false
    return day.date(from: text)
}
private func todoQuery(_ values: [String: String]) -> String {
    var components = URLComponents()
    components.queryItems = values.sorted { $0.key < $1.key }.map { URLQueryItem(name: $0.key, value: $0.value) }
    return "?" + (components.percentEncodedQuery ?? "")
}
public extension ManagedClient {
    func todoMailAccounts() async throws -> [TodoMailAccount] {
        let response = try await json(path: "/v1/todo/mail/accounts")
        guard case .array(let accounts) = response["accounts"] else { throw APIError.invalidResponse }
        return try accounts.map(TodoMailAccount.init)
    }
    func todoMailThreads(connectionID: String, query: String, pageToken: String? = nil) async throws -> TodoMailPage {
        var values = ["connection_id": connectionID, "q": query]
        values["page_token"] = pageToken
        let response = try await json(path: "/v1/todo/mail/threads" + todoQuery(values))
        guard case .array(let threads) = response["threads"] else { throw APIError.invalidResponse }
        return TodoMailPage(threads: try threads.map(TodoMailThreadSummary.init), nextPageToken: response["next_page_token"].string.isEmpty ? nil : response["next_page_token"].string)
    }
    func todoMailSummary(connectionID: String, threadID: String) async throws -> TodoMailThreadSummary {
        guard let id = threadID.addingPercentEncoding(withAllowedCharacters: .alphanumerics), !id.isEmpty else { throw APIError.invalidResponse }
        let response = try await json(path: "/v1/todo/mail/threads/" + id + todoQuery(["connection_id": connectionID, "format": "metadata"]))
        return try TodoMailThreadSummary(response["summary"])
    }
    func todoSchedule() async throws -> TodoSchedule {
        let response = try await json(path: "/v1/todo/schedule")
        guard case .array(let events) = response["events"] else { throw APIError.invalidResponse }
        return TodoSchedule(events: try events.map(TodoScheduleEvent.init), partial: response["partial"].bool)
    }
    func modifyTodoMailThread(connectionID: String, threadID: String, archive: Bool? = nil, unread: Bool? = nil) async throws {
        guard let id = threadID.addingPercentEncoding(withAllowedCharacters: .alphanumerics), !id.isEmpty else { throw APIError.invalidResponse }
        var fields: [String: JSON] = ["connection_id": .string(connectionID)]
        if let archive { fields["archive"] = .bool(archive) }
        if let unread { fields["unread"] = .bool(unread) }
        _ = try await json(path: "/v1/todo/mail/threads/" + id + "/modify", method: "POST", body: .object(fields))
    }
}
