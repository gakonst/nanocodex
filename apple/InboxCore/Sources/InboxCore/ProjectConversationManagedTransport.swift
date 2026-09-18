#if canImport(Combine) && !os(Linux)
import Foundation

/// Adapter for the existing account ManagedClient contract. The host is
/// responsible for resolving the authorized roster for this account/project;
/// this adapter never discovers membership through the account-wide agent list.
/// This is not a Connect-grant adapter: Connect hosts must inject a transport
/// whose request construction preserves their grant's route/resource boundary.
@MainActor public final class ProjectConversationManagedTransport: ProjectConversationTransport {
    public let scope: ProjectConversationScope
    private let client: ManagedClient
    private let allowed: Set<String>
    public init(client: ManagedClient, authorizedScope: ProjectConversationScope) throws {
        guard !authorizedScope.projectID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw APIError.invalidResponse }
        for item in authorizedScope.conversations { _ = try ManagedClient.agentPath(item.id) }
        self.client = client; scope = authorizedScope
        allowed = Set(authorizedScope.conversations.map(\.id))
    }
    private func authorize(_ id: String) throws {
        guard allowed.contains(id) else { throw APIError.http(403) }
    }
    public func state(_ id: String) async throws -> JSON {
        try authorize(id)
        return try await client.state(id)
    }
    public func history(_ id: String, before: Cursor?, after: Cursor?) async throws -> EventPage {
        try authorize(id)
        return try await client.history(id, before: before, after: after)
    }
    public func stream(_ id: String, after: Cursor, receive: @escaping @Sendable (ProjectConversationFrame) async -> Void) async throws {
        try authorize(id)
        try await client.stream(id, after: after) { frame in
            await receive(.init(event: frame.event, cursor: frame.cursor))
        }
    }
    public func send(_ command: AgentCommand) async throws {
        try authorize(command.agentID)
        guard (command.kind == .followUp || command.kind == .stop), command.images.isEmpty, command.rawInput == nil else { throw APIError.invalidResponse }
        _ = try await client.command(command)
    }
}
#endif
