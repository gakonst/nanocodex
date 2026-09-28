import Foundation
import CryptoKit

public struct TodoMailAttachment: Identifiable, Equatable, Sendable {
    public let id: String
    public let filename: String
    public let mimeType: String
    public let size: Int
    public init(_ json: JSON) throws {
        guard !json["id"].string.isEmpty else { throw APIError.invalidResponse }
        id = json["id"].string
        filename = json["filename"].string.isEmpty ? "Attachment" : json["filename"].string
        mimeType = json["mime_type"].string
        size = max(0, Int(exactly: json["size"].number) ?? 0)
    }
}

public struct TodoMailMessage: Identifiable, Equatable, Sendable {
    public let id: String
    public let threadID: String
    public let from: String
    public let to: String
    public let cc: String
    public let bcc: String
    public let replyTo: String
    public let subject: String
    public let date: String
    public let messageID: String
    public let bodyText: String
    public let bodyHTML: String
    public let bodyTruncated: Bool
    public let attachments: [TodoMailAttachment]
    public init(_ json: JSON) throws {
        guard !json["id"].string.isEmpty else { throw APIError.invalidResponse }
        id = json["id"].string; threadID = json["thread_id"].string
        from = json["from"].string; to = json["to"].string
        cc = json["cc"].string; bcc = json["bcc"].string
        replyTo = json["reply_to"].string
        subject = json["subject"].string; date = json["date"].string
        messageID = json["message_id"].string
        bodyText = json["body_text"].string; bodyHTML = json["body_html"].string
        bodyTruncated = json["body_truncated"].bool
        attachments = try json["attachments"].array.map(TodoMailAttachment.init)
    }
}

public struct TodoMailThread: Identifiable, Equatable, Sendable {
    public let id: String
    public let connectionID: String
    public let subject: String
    public let messages: [TodoMailMessage]
    public init(_ json: JSON) throws {
        guard !json["id"].string.isEmpty, case .array = json["messages"] else { throw APIError.invalidResponse }
        id = json["id"].string; connectionID = json["connection_id"].string
        messages = try json["messages"].array.map(TodoMailMessage.init)
        subject = json["subject"].string.isEmpty ? (messages.first?.subject ?? "") : json["subject"].string
    }
}

public enum TodoMailDraftMode: String, Codable, CaseIterable, Sendable {
    case compose, reply, replyAll = "reply_all", forward
    public var title: String {
        switch self { case .compose: return "New message"; case .reply: return "Reply"; case .replyAll: return "Reply all"; case .forward: return "Forward" }
    }
}

/// Editable account-server draft. Version zero is a local draft not yet saved.
public struct TodoMailDraft: Identifiable, Codable, Equatable, Sendable {
    public var id: String
    public var connectionID: String
    public var threadID: String?
    public var replyMessageID: String?
    public var mode: TodoMailDraftMode
    public var version: Int
    public var to: [String]
    public var cc: [String]
    public var bcc: [String]
    public var subject: String
    public var bodyText: String
    public var status: String
    public init(connectionID: String, threadID: String? = nil, replyMessageID: String? = nil,
                mode: TodoMailDraftMode = .compose, to: [String] = [], cc: [String] = [],
                bcc: [String] = [], subject: String = "", bodyText: String = "") {
        id = UUID().uuidString.lowercased(); self.connectionID = connectionID
        self.threadID = threadID; self.replyMessageID = replyMessageID; self.mode = mode
        version = 0; self.to = to; self.cc = cc; self.bcc = bcc
        self.subject = subject; self.bodyText = bodyText; status = "draft"
    }
    public init(_ json: JSON) throws {
        guard !json["id"].string.isEmpty, !json["connection_id"].string.isEmpty,
              let version = Int(exactly: json["version"].number), version > 0 else { throw APIError.invalidResponse }
        id = json["id"].string; connectionID = json["connection_id"].string
        threadID = json["thread_id"].string.isEmpty ? nil : json["thread_id"].string
        replyMessageID = json["reply_message_id"].string.isEmpty ? nil : json["reply_message_id"].string
        mode = TodoMailDraftMode(rawValue: json["mode"].string) ?? .compose
        self.version = version
        to = json["to"].array.map(\.string); cc = json["cc"].array.map(\.string); bcc = json["bcc"].array.map(\.string)
        subject = json["subject"].string; bodyText = json["body_text"].string
        status = json["status"].string.isEmpty ? "draft" : json["status"].string
    }
    public var isLocked: Bool { ["sending", "sent", "unknown"].contains(status) }
    public var saveJSON: JSON {
        var fields: [String: JSON] = [
            "connection_id": .string(connectionID), "mode": .string(mode.rawValue),
            "thread_id": threadID.map(JSON.string) ?? .null,
            "reply_message_id": replyMessageID.map(JSON.string) ?? .null,
            "to": .array(to.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }.map(JSON.string)),
            "cc": .array(cc.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }.map(JSON.string)),
            "bcc": .array(bcc.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }.map(JSON.string)),
            "subject": .string(subject), "body_text": .string(bodyText),
        ]
        fields["id"] = .string(id); fields["version"] = .number(Double(version))
        return .object(fields)
    }
}

public struct TodoMailSendReceipt: Equatable, Sendable {
    public let operationID: String
    public let draftID: String
    public let status: String
    public init(_ json: JSON) throws {
        guard !json["operation_id"].string.isEmpty, !json["draft_id"].string.isEmpty,
              ["sent", "sending", "unknown"].contains(json["status"].string) else { throw APIError.invalidResponse }
        operationID = json["operation_id"].string; draftID = json["draft_id"].string; status = json["status"].string
    }
}

public extension ManagedClient {
    /// A one-way scope for protected on-device draft recovery; never exposes the account key.
    var todoMailStorageScope: String {
        SHA256.hash(data: Data((credential.origin + "\n" + credential.apiKey).utf8)).map { String(format: "%02x", $0) }.joined()
    }
    func todoMailThread(connectionID: String, threadID: String) async throws -> TodoMailThread {
        let response = try await json(path: "/v1/todo/mail/threads/" + Self.mailPathComponent(threadID) + Self.mailQuery(["connection_id": connectionID]))
        return try TodoMailThread(response["thread"])
    }
    func todoMailDraft(id: String) async throws -> TodoMailDraft {
        try TodoMailDraft(await json(path: "/v1/todo/mail/drafts/" + Self.mailPathComponent(id))["draft"])
    }
    func todoMailDrafts(connectionID: String, threadID: String? = nil) async throws -> [TodoMailDraft] {
        var query = ["connection_id": connectionID]
        if let threadID { query["thread_id"] = threadID }
        let response = try await json(path: "/v1/todo/mail/drafts" + Self.mailQuery(query))
        return try response["drafts"].array.map(TodoMailDraft.init).filter { threadID == nil || $0.threadID == threadID }
    }
    /// Generates editable text only. It neither creates a draft nor sends mail.
    func suggestTodoMailReply(connectionID: String, threadID: String, replyMessageID: String) async throws -> String {
        let response = try await json(path: "/v1/todo/mail/suggest", method: "POST", body: .object([
            "connection_id": .string(connectionID), "thread_id": .string(threadID),
            "reply_message_id": .string(replyMessageID),
        ]))
        guard case .string(let body) = response["body_text"], !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              body.utf8.count <= 200_000 else { throw APIError.invalidResponse }
        return body
    }
    func saveTodoMailDraft(_ draft: TodoMailDraft) async throws -> TodoMailDraft {
        try TodoMailDraft(await json(path: "/v1/todo/mail/drafts", method: "POST", body: draft.saveJSON)["draft"])
    }
    /// Call only from an explicit Send action, after freezing and persisting the reviewed draft version.
    func sendTodoMailDraft(_ draft: TodoMailDraft, operationID: UUID) async throws -> TodoMailSendReceipt {
        let response = try await json(path: "/v1/todo/mail/send", method: "POST", body: .object([
            "draft_id": .string(draft.id), "version": .number(Double(draft.version)),
            "operation_id": .string(operationID.uuidString.lowercased()),
        ]), idempotencyKey: operationID.uuidString.lowercased())
        return try TodoMailSendReceipt(response["receipt"])
    }
    private static func mailPathComponent(_ value: String) throws -> String {
        guard !value.isEmpty, value != ".", value != "..",
              let encoded = value.addingPercentEncoding(withAllowedCharacters: .alphanumerics) else { throw APIError.invalidResponse }
        return encoded
    }
    private static func mailQuery(_ values: [String: String]) -> String {
        var components = URLComponents()
        components.queryItems = values.sorted { $0.key < $1.key }.map { URLQueryItem(name: $0.key, value: $0.value) }
        return "?" + (components.percentEncodedQuery ?? "")
    }
}

#if DEBUG
public extension TodoMailThread {
    /// Synthetic content only; readers using this fixture never invoke live mail endpoints.
    static var fixture: TodoMailThread {
        try! TodoMailThread(.object([
            "id": .string("fixture-thread"), "connection_id": .string("fixture-mail"), "subject": .string("A quick look at the launch plan"),
            "messages": .array([
                .object(["id": .string("fixture-message-1"), "thread_id": .string("fixture-thread"), "from": .string("Maya Chen <maya@example.com>"),
                    "to": .string("Alex Morgan <alex@example.com>"), "cc": .string("Jordan <jordan@example.com>"), "subject": .string("A quick look at the launch plan"),
                    "date": .string("Mon, 28 Sep 2026 09:15:00 +0000"), "message_id": .string("<launch-1@example.com>"),
                    "body_text": .string("Hi Alex,\n\nCan you review the launch plan before Thursday? We’ve folded in the feedback from the team.\n\nThe two decisions are the launch date and who should join the first round. I’d love your thoughts.\n\nThanks,\nMaya"),
                    "attachments": .array([.object(["id": .string("fixture-attachment"), "filename": .string("Launch notes.txt"), "mime_type": .string("text/plain"), "size": .number(84)])])]),
                .object(["id": .string("fixture-message-2"), "thread_id": .string("fixture-thread"), "from": .string("Maya Chen <maya@example.com>"),
                    "to": .string("Alex Morgan <alex@example.com>"), "cc": .string("Jordan <jordan@example.com>"), "subject": .string("Re: A quick look at the launch plan"),
                    "date": .string("Mon, 28 Sep 2026 10:30:00 +0000"), "message_id": .string("<launch-2@example.com>"),
                    "body_text": .string("One more thing: Thursday at 10 works for Jordan too. Does that work for you?\n\nMaya"), "attachments": .array([])]),
            ]),
        ]))
    }
}
#endif

public extension ManagedClient {
    func todoMailAccountEmail(connectionID: String) async throws -> String {
        let response = try await json(path: "/v1/todo/mail/accounts")
        guard let account = response["accounts"].array.first(where: { $0["connection_id"].string == connectionID }),
              !account["email"].string.isEmpty else { throw APIError.invalidResponse }
        return account["email"].string
    }
    /// Download through the authenticated API; no provider URL enters the reader.
    func downloadTodoMailAttachment(connectionID: String, messageID: String, attachment: TodoMailAttachment) async throws -> URL {
        let response = try await json(path: "/v1/todo/mail/messages/" + Self.mailPathComponent(messageID) + "/attachments/" + Self.mailPathComponent(attachment.id) + Self.mailQuery(["connection_id": connectionID]))
        let encoded = response["data"].string.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        let padded = encoded + String(repeating: "=", count: (4 - encoded.count % 4) % 4)
        guard encoded.utf8.count <= 36 * 1024 * 1024, let data = Data(base64Encoded: padded),
              data.count == Int(exactly: response["size"].number), data.count <= 25 * 1024 * 1024 else { throw APIError.invalidResponse }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("TodoMail-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let filename = (attachment.filename as NSString).lastPathComponent
        let target = directory.appendingPathComponent(filename.isEmpty || filename == "." || filename == ".." ? "Attachment" : filename)
        try data.write(to: target, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        return target
    }
}
