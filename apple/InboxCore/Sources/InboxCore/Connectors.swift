import Foundation

public struct ConnectorCapabilityDefinition: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
}

public struct ConnectorProviderDefinition: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let description: String
    public let capabilities: [ConnectorCapabilityDefinition]
}

public struct ConnectorAccountConnection: Identifiable, Equatable, Sendable {
    public let id: String
    public let label: String
    public let accountID: String?
    public let capabilities: [String]
}

public struct ConnectorCapabilityStatus: Equatable, Sendable {
    public let connected: Bool
    public let connections: [ConnectorAccountConnection]
    public let legacyLabel: String?
}

public enum McpConnectionStatus: String, Equatable, Sendable {
    case authorizationRequired = "authorization_required"
    case connected
    case reauthorizationRequired = "reauthorization_required"
    case disabled
    case revoked
}

public struct McpConnection: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let status: McpConnectionStatus
}

public struct ConnectorOverview: Equatable, Sendable {
    public let providers: [ConnectorProviderDefinition]
    public let statuses: [String: ConnectorCapabilityStatus]
    public let mcpConnections: [McpConnection]

    public func isConnected(_ provider: ConnectorProviderDefinition) -> Bool {
        provider.capabilities.contains { statuses[$0.id]?.connected == true }
    }

    public func connections(for provider: ConnectorProviderDefinition) -> [ConnectorAccountConnection] {
        var connections: [String: ConnectorAccountConnection] = [:]
        for capability in provider.capabilities {
            for connection in statuses[capability.id]?.connections ?? [] {
                let prior = connections[connection.id]
                let capabilities = Set((prior?.capabilities ?? []) + connection.capabilities + [capability.id]).sorted()
                connections[connection.id] = ConnectorAccountConnection(
                    id: connection.id,
                    label: connection.label,
                    accountID: connection.accountID ?? prior?.accountID,
                    capabilities: capabilities
                )
            }
        }
        return connections.values.sorted {
            $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending
        }
    }

    public func capabilityNames(
        for connection: ConnectorAccountConnection,
        provider: ConnectorProviderDefinition
    ) -> [String] {
        let names = Dictionary(uniqueKeysWithValues: provider.capabilities.map { ($0.id, $0.name) })
        return connection.capabilities.compactMap { names[$0] }
    }
}

public enum ConnectorAuthorizationResult: String, Equatable, Sendable {
    case connected, cancelled, failed
}

public struct ConnectorAuthorization: Equatable, Sendable {
    public let provider: String
    public let authorizationURL: URL
    public let callbackURL: URL
    public let attemptID: String

    public func result(from url: URL) throws -> ConnectorAuthorizationResult {
        guard url.scheme == callbackURL.scheme,
              url.host == callbackURL.host,
              url.port == callbackURL.port,
              url.path == callbackURL.path,
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            throw APIError.invalidResponse
        }
        let items = components.queryItems ?? []
        guard items.count == 3,
              items.filter({ $0.name == "attempt" }).count == 1,
              items.first(where: { $0.name == "attempt" })?.value == attemptID,
              items.filter({ $0.name == "connector" }).count == 1,
              items.first(where: { $0.name == "connector" })?.value == provider,
              let raw = items.first(where: { $0.name == "connector_result" })?.value,
              items.filter({ $0.name == "connector_result" }).count == 1,
              let result = ConnectorAuthorizationResult(rawValue: raw) else {
            throw APIError.invalidResponse
        }
        return result
    }
}

public struct McpAuthorization: Equatable, Sendable {
    public let connectionID: String
    public let authorizationURL: URL
    public let callbackURL: URL
    public let attemptID: String

    public func result(from url: URL) throws -> ConnectorAuthorizationResult {
        guard url.scheme == callbackURL.scheme,
              url.host == callbackURL.host,
              url.port == callbackURL.port,
              url.path == callbackURL.path,
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            throw APIError.invalidResponse
        }
        let items = components.queryItems ?? []
        guard items.count == 3,
              items.filter({ $0.name == "attempt" }).count == 1,
              items.first(where: { $0.name == "attempt" })?.value == attemptID,
              items.filter({ $0.name == "mcp_connection" }).count == 1,
              items.first(where: { $0.name == "mcp_connection" })?.value == connectionID,
              let raw = items.first(where: { $0.name == "mcp_result" })?.value,
              items.filter({ $0.name == "mcp_result" }).count == 1,
              let result = ConnectorAuthorizationResult(rawValue: raw) else {
            throw APIError.invalidResponse
        }
        return result
    }
}

public enum McpConnectionStart: Equatable, Sendable {
    case connected(McpConnection)
    case authorization(McpAuthorization)
}

public extension ManagedClient {
    func connectorOverview() async throws -> ConnectorOverview {
        async let catalog = json(path: "/v1/connectors/catalog")
        async let statuses = json(path: "/v1/connectors")
        async let mcpConnections = json(path: "/v1/connectors/mcp-connections")
        let (catalogValue, statusValue, mcpValue) = try await (catalog, statuses, mcpConnections)
        return try ConnectorOverview(
            providers: Self.connectorProviders(from: catalogValue),
            statuses: Self.connectorStatuses(from: statusValue),
            mcpConnections: Self.mcpConnections(from: mcpValue)
        )
    }

    func beginConnectorAuthorization(provider: String) async throws -> ConnectorAuthorization {
        let provider = try Self.connectorPathComponent(provider)
        let attemptID = UUID().uuidString.lowercased()
        var callback = URLComponents(string: "nanocodex://connectors/complete")
        callback?.queryItems = [URLQueryItem(name: "attempt", value: attemptID)]
        guard let callbackURL = callback?.url else { throw APIError.invalidResponse }
        let returnTo = "/v1/connectors/mobile-complete?attempt=" + attemptID
        let response = try await json(
            path: "/v1/connectors/\(provider)",
            method: "POST",
            body: .object(["return_to": .string(returnTo)])
        )
        guard let authorizationURL = URL(string: response["authorization_url"].string),
              authorizationURL.scheme == "https",
              authorizationURL.user == nil,
              authorizationURL.password == nil,
              authorizationURL.fragment == nil else {
            throw APIError.invalidResponse
        }
        return ConnectorAuthorization(
            provider: provider,
            authorizationURL: authorizationURL,
            callbackURL: callbackURL,
            attemptID: attemptID
        )
    }

    func disconnectConnector(provider: String, connectionID: String) async throws {
        let provider = try Self.connectorPathComponent(provider)
        guard connectionID.range(of: #"^[A-Za-z0-9_-]{43}$"#, options: .regularExpression) != nil else {
            throw APIError.invalidResponse
        }
        _ = try await json(
            path: "/v1/connectors/\(provider)/connections/\(connectionID)",
            method: "DELETE"
        )
    }

    func addMcpConnection(target: String) async throws -> McpConnection {
        let response = try await json(
            path: "/v1/connectors/mcp-connections",
            method: "POST",
            body: .object(["target": .string(target)])
        )
        return try Self.mcpConnection(from: response["mcp_connection"])
    }

    func beginMcpAuthorization(connectionID: String) async throws -> McpConnectionStart {
        let connectionID = try Self.mcpConnectionID(connectionID)
        let attemptID = UUID().uuidString.lowercased()
        var callback = URLComponents(string: "nanocodex://connectors/mcp-complete")
        callback?.queryItems = [URLQueryItem(name: "attempt", value: attemptID)]
        guard let callbackURL = callback?.url else { throw APIError.invalidResponse }
        let returnTo = "/v1/connectors/mcp-mobile-complete?attempt=" + attemptID
        let response = try await json(
            path: "/v1/connectors/mcp-connections/\(connectionID)/start",
            method: "POST",
            body: .object(["return_to": .string(returnTo)])
        )
        let connection = try Self.mcpConnection(from: response["mcp_connection"])
        guard connection.id == connectionID else { throw APIError.invalidResponse }
        if connection.status == .connected, response["authorization_url"] == .null {
            return .connected(connection)
        }
        guard let authorizationURL = URL(string: response["authorization_url"].string),
              authorizationURL.scheme == "https",
              authorizationURL.user == nil,
              authorizationURL.password == nil,
              authorizationURL.fragment == nil else {
            throw APIError.invalidResponse
        }
        return .authorization(McpAuthorization(
            connectionID: connectionID,
            authorizationURL: authorizationURL,
            callbackURL: callbackURL,
            attemptID: attemptID
        ))
    }

    func disconnectMcpConnection(connectionID: String) async throws {
        let connectionID = try Self.mcpConnectionID(connectionID)
        _ = try await json(
            path: "/v1/connectors/mcp-connections/\(connectionID)",
            method: "DELETE"
        )
    }

    private static func connectorPathComponent(_ value: String) throws -> String {
        guard value.range(of: #"^[a-z][a-z0-9_-]{0,63}$"#, options: .regularExpression) != nil else {
            throw APIError.invalidResponse
        }
        return value
    }

    private static func connectorProviders(from value: JSON) throws -> [ConnectorProviderDefinition] {
        guard case .array(let rawProviders) = value["providers"],
              !rawProviders.isEmpty, rawProviders.count <= 64 else { throw APIError.invalidResponse }
        var ids = Set<String>()
        let providers = try rawProviders.map { raw -> ConnectorProviderDefinition in
            let id = try connectorIdentifier(raw["id"].string)
            let name = try displayString(raw["name"].string, maximum: 128)
            let description = try displayString(raw["description"].string, maximum: 512)
            guard ids.insert(id).inserted,
                  case .array(let rawCapabilities) = raw["capabilities"],
                  !rawCapabilities.isEmpty, rawCapabilities.count <= 32 else { throw APIError.invalidResponse }
            var capabilityIDs = Set<String>()
            let capabilities = try rawCapabilities.map { capability -> ConnectorCapabilityDefinition in
                let capabilityID = try connectorIdentifier(capability["id"].string)
                guard capabilityIDs.insert(capabilityID).inserted else { throw APIError.invalidResponse }
                return ConnectorCapabilityDefinition(
                    id: capabilityID,
                    name: try displayString(capability["name"].string, maximum: 128)
                )
            }
            return ConnectorProviderDefinition(
                id: id,
                name: name,
                description: description,
                capabilities: capabilities
            )
        }
        return providers
    }

    private static func connectorStatuses(from value: JSON) throws -> [String: ConnectorCapabilityStatus] {
        guard case .object(let rawStatuses) = value["connectors"], rawStatuses.count <= 128 else {
            throw APIError.invalidResponse
        }
        return try Dictionary(uniqueKeysWithValues: rawStatuses.map { id, raw in
            let id = try connectorIdentifier(id)
            let connected = raw["connected"].bool
            var connections: [ConnectorAccountConnection] = []
            if case .array(let rawConnections) = raw["connections"] {
                guard rawConnections.count <= 64 else { throw APIError.invalidResponse }
                var connectionIDs = Set<String>()
                connections = try rawConnections.map { connection in
                    let connectionID = connection["id"].string
                    guard connectionID.range(of: #"^[A-Za-z0-9_-]{43}$"#, options: .regularExpression) != nil,
                          connectionIDs.insert(connectionID).inserted else { throw APIError.invalidResponse }
                    let capabilities: [String]
                    if case .array(let rawCapabilities) = connection["capabilities"] {
                        guard rawCapabilities.count <= 32 else { throw APIError.invalidResponse }
                        capabilities = try rawCapabilities.map { try connectorIdentifier($0.string) }
                        guard Set(capabilities).count == capabilities.count else { throw APIError.invalidResponse }
                    } else { capabilities = [] }
                    let accountID = connection["account_id"].string
                    return ConnectorAccountConnection(
                        id: connectionID,
                        label: try displayString(connection["label"].string, maximum: 256),
                        accountID: accountID.isEmpty ? nil : try displayString(accountID, maximum: 256),
                        capabilities: capabilities
                    )
                }
            }
            let legacyLabel = raw["label"].string.isEmpty ? raw["account"].string : raw["label"].string
            return (id, ConnectorCapabilityStatus(
                connected: connected,
                connections: connections,
                legacyLabel: legacyLabel.isEmpty ? nil : try displayString(legacyLabel, maximum: 256)
            ))
        })
    }

    private static func mcpConnections(from value: JSON) throws -> [McpConnection] {
        guard case .array(let rawConnections) = value["mcp_connections"], rawConnections.count <= 64 else {
            throw APIError.invalidResponse
        }
        var ids = Set<String>()
        return try rawConnections.map { raw in
            let connection = try mcpConnection(from: raw)
            guard ids.insert(connection.id).inserted else { throw APIError.invalidResponse }
            return connection
        }
    }

    private static func mcpConnection(from value: JSON) throws -> McpConnection {
        let id = try mcpConnectionID(value["id"].string)
        let name = try displayString(value["name"].string, maximum: 256)
        guard let status = McpConnectionStatus(rawValue: value["status"].string) else {
            throw APIError.invalidResponse
        }
        return McpConnection(id: id, name: name, status: status)
    }

    private static func mcpConnectionID(_ value: String) throws -> String {
        guard value.range(of: #"^[A-Za-z0-9_-]{43}$"#, options: .regularExpression) != nil else {
            throw APIError.invalidResponse
        }
        return value
    }

    private static func connectorIdentifier(_ value: String) throws -> String {
        guard value.range(of: #"^[a-z][a-z0-9_-]{0,63}$"#, options: .regularExpression) != nil else {
            throw APIError.invalidResponse
        }
        return value
    }

    private static func displayString(_ value: String, maximum: Int) throws -> String {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, value.count <= maximum,
              value.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) else {
            throw APIError.invalidResponse
        }
        return value
    }
}
