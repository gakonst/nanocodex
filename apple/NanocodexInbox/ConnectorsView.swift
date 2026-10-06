import AuthenticationServices
import InboxCore
import NanocodexUI
import SwiftUI
import UIKit

struct ConnectorsView: View {
    @ObservedObject var model: InboxModel

    var body: some View {
        // Recreate all connector state, including presented sheets, when the
        // account changes, even when this screen is opened from Settings.
        ConnectorContentView(model: model)
            .id(model.vaultIntakeAccount)
    }
}

private struct ConnectorContentView: View {
    @ObservedObject var model: InboxModel
    @StateObject private var center = ConnectorCenter()
    @State private var query = ""
    @State private var showingAddMcp = false
    @State private var showingMusic: MusicLoopbackProvider?

    private var providers: [ConnectorProviderDefinition] {
        guard let overview = center.overview else { return [] }
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return overview.providers }
        return overview.providers.filter { provider in
            ([provider.name, provider.description]
                + provider.capabilities.map(\.name)
                + overview.connections(for: provider).map(\.label))
                .contains { $0.localizedCaseInsensitiveContains(query) }
        }
    }
    private var connected: [ConnectorProviderDefinition] {
        guard let overview = center.overview else { return [] }
        return providers.filter(overview.isConnected)
    }
    private var available: [ConnectorProviderDefinition] {
        guard let overview = center.overview else { return providers }
        return providers.filter { !overview.isConnected($0) }
    }
    private var mcpConnections: [McpConnection] {
        guard let overview = center.overview else { return [] }
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return overview.mcpConnections }
        return overview.mcpConnections.filter { $0.name.localizedCaseInsensitiveContains(query) }
    }
    private var connectedMcp: [McpConnection] {
        mcpConnections.filter { $0.status == .connected }
    }
    private var availableMcp: [McpConnection] {
        mcpConnections.filter { $0.status != .connected && $0.status != .revoked }
    }
    private var showsAddMcp: Bool {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return query.isEmpty || "Add MCP server".localizedCaseInsensitiveContains(query)
    }
    private var showsChatGpt: Bool {
        query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || "ChatGPT accounts model subscriptions".localizedCaseInsensitiveContains(query.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private var showsClaude: Bool {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return query.isEmpty || "Claude Anthropic model subscriptions".localizedCaseInsensitiveContains(query)
    }

    var body: some View {
        List {
            Section { NavigationLink("Vault") { NativeVaultView(model: model) } }
            if showsChatGpt || showsClaude {
                Section {
                    if showsChatGpt { NavigationLink("ChatGPT accounts") { NativeVaultView(model: model, chatOnly: true) } }
                    if showsClaude {
                        NavigationLink {
                            Form {
                                ClaudeConnectionSection(read: model.claudeConnectionStatus, start: model.startClaudeLogin,
                                    complete: model.completeClaudeLogin, disconnect: model.disconnectClaude,
                                    changed: model.refreshModelCatalog)
                            }
                            .navigationTitle("Claude")
                        } label: {
                            Label("Claude subscription", systemImage: "person.crop.circle.badge.plus")
                        }
                        .accessibilityIdentifier("claude-connection")
                    }
                } header: {
                    Text("Model access")
                }
            }
            if center.loading, center.overview == nil {
                HStack { Spacer(); ProgressView("Loading connectors"); Spacer() }
                    .listRowBackground(Color.clear)
            }
            if !connected.isEmpty || !connectedMcp.isEmpty {
                Section("Connected") {
                    ForEach(connected) { provider in
                        NavigationLink {
                            if let music = MusicLoopbackProvider(rawValue: provider.id) {
                                Form { MusicConnectionView(model: model, provider: music) }
                                    .navigationTitle(music.name)
                                    .onDisappear { Task { await center.load(using: model) } }
                            } else {
                                ConnectorProviderView(model: model, center: center, provider: provider)
                            }
                        } label: {
                            ConnectorRow(
                                provider: provider,
                                action: nil,
                                detail: connectedDetail(provider),
                                busy: center.operation == provider.id
                            )
                        }
                        .accessibilityIdentifier("connector-connected:" + provider.id)
                        .accessibilityValue(connectedDetail(provider))
                    }
                    ForEach(connectedMcp) { connection in
                        NavigationLink {
                            McpConnectionView(model: model, center: center, connection: connection)
                        } label: {
                            McpRow(
                                connection: connection,
                                action: nil,
                                busy: center.operation == "mcp:" + connection.id
                            )
                        }
                        .accessibilityIdentifier("mcp-connected:" + connection.id)
                    }
                }
            }
            if !available.isEmpty || !availableMcp.isEmpty || showsAddMcp {
                Section("Available") {
                    ForEach(available) { provider in
                        Button {
                            if let music = MusicLoopbackProvider(rawValue: provider.id) { showingMusic = music }
                            else { Task { await center.connect(provider, using: model) } }
                        } label: {
                            ConnectorRow(
                                provider: provider,
                                action: "Connect",
                                busy: center.operation == provider.id
                            )
                        }
                        .buttonStyle(.plain)
                        .disabled(center.operation != nil)
                        .accessibilityIdentifier("connector-available:" + provider.id)
                    }
                    ForEach(availableMcp) { connection in
                        Button {
                            Task { await center.connect(connection, using: model) }
                        } label: {
                            McpRow(
                                connection: connection,
                                action: connection.status == .reauthorizationRequired ? "Reconnect" : "Connect",
                                busy: center.operation == "mcp:" + connection.id
                            )
                        }
                        .buttonStyle(.plain)
                        .disabled(center.operation != nil || connection.status == .disabled)
                        .accessibilityIdentifier("mcp-available:" + connection.id)
                    }
                    if showsAddMcp {
                        Button { showingAddMcp = true } label: {
                            HStack(spacing: 12) {
                                ConnectorLogo(provider: "mcp", size: 34)
                                Text("Add MCP server")
                                Spacer()
                                Image(systemName: "plus").foregroundStyle(.blue)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(center.operation != nil)
                        .accessibilityIdentifier("mcp-add")
                    }
                }
            }
            if center.overview != nil, providers.isEmpty, mcpConnections.isEmpty, !showsAddMcp, !showsChatGpt, !showsClaude {
                ContentUnavailableView.search(text: query)
                    .listRowBackground(Color.clear)
            }
            if let error = center.error {
                Section {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(error).font(.subheadline).foregroundStyle(.secondary)
                        Button("Try again") { Task { await center.load(using: model) } }
                    }
                    .accessibilityIdentifier("connector-error")
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Connectors")
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search connectors")
        .refreshable { await center.load(using: model) }
        .task {
            openRequestedMusicConnector()
            await center.load(using: model)
        }
        .onChange(of: model.musicConnectorToOpen) { _, _ in openRequestedMusicConnector() }
        .sheet(isPresented: $showingAddMcp) {
            AddMcpView(model: model, center: center)
        }
        .sheet(item: $showingMusic, onDismiss: { Task { await center.load(using: model) } }) { music in
            NavigationStack {
                Form { MusicConnectionView(model: model, provider: music) }
                    .navigationTitle(music.name)
                    .toolbar { ToolbarItem(placement: .confirmationAction) {
                        Button("Done") { showingMusic = nil }
                    } }
            }
        }
        .accessibilityIdentifier("connectors-list")
    }

    private func openRequestedMusicConnector() {
        guard let provider = model.musicConnectorToOpen else { return }
        showingMusic = provider
        model.musicConnectorToOpen = nil
    }

    private func connectedDetail(_ provider: ConnectorProviderDefinition) -> String {
        guard let overview = center.overview else { return "Connected" }
        let count = overview.connections(for: provider).count
        if count == 0 { return "Connected" }
        return count == 1 ? overview.connections(for: provider)[0].label : "\(count) accounts"
    }
}

private struct AddMcpView: View {
    @ObservedObject var model: InboxModel
    @ObservedObject var center: ConnectorCenter
    @Environment(\.dismiss) private var dismiss
    @State private var target = ""

    var body: some View {
        NavigationStack {
            Form {
                TextField("MCP server URL", text: $target)
                    .textInputAutocapitalization(.never)
                    .keyboardType(.URL)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .onSubmit { add() }
                    .accessibilityIdentifier("mcp-target")
                if let error = center.error {
                    Text(error).font(.subheadline).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Add MCP server")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if center.operation == "mcp:add" { ProgressView() }
                    else { Button("Add") { add() }.disabled(target.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) }
                }
            }
        }
    }

    private func add() {
        let target = target.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !target.isEmpty else { return }
        Task { if await center.addMcp(target, using: model) { dismiss() } }
    }
}

private struct McpConnectionView: View {
    @ObservedObject var model: InboxModel
    @ObservedObject var center: ConnectorCenter
    let connection: McpConnection
    @State private var confirmingRevoke = false

    var body: some View {
        List {
            Section {
                HStack(spacing: 12) {
                    ConnectorLogo(provider: "mcp", size: 36)
                    Text(connection.name).font(.body.weight(.medium)).lineLimit(1)
                }
            }
            Section {
                Button("Revoke", role: .destructive) { confirmingRevoke = true }
                    .disabled(center.operation != nil)
                    .accessibilityIdentifier("mcp-revoke")
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(connection.name)
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog("Revoke \(connection.name)?", isPresented: $confirmingRevoke) {
            Button("Revoke", role: .destructive) {
                Task { await center.revoke(connection, using: model) }
            }
            Button("Cancel", role: .cancel) {}
        }
    }
}

private struct ConnectorProviderView: View {
    @ObservedObject var model: InboxModel
    @ObservedObject var center: ConnectorCenter
    let provider: ConnectorProviderDefinition
    @State private var pendingRevocation: ConnectorAccountConnection?

    private var overview: ConnectorOverview? { center.overview }
    private var connections: [ConnectorAccountConnection] { overview?.connections(for: provider) ?? [] }

    var body: some View {
        List {
            Section {
                HStack(spacing: 12) {
                    ConnectorLogo(provider: provider.id, size: 36)
                    Text(provider.name).font(.body.weight(.medium)).lineLimit(1)
                }
            }
            if !connections.isEmpty {
                Section(connections.count == 1 ? "Account" : "Accounts") {
                    ForEach(connections) { connection in
                        HStack {
                            Text(connection.label).font(.body.weight(.medium)).lineLimit(1)
                            Spacer(minLength: 12)
                            Button("Revoke", role: .destructive) { pendingRevocation = connection }
                                .disabled(center.operation != nil)
                        }
                        .accessibilityIdentifier("connector-account:" + connection.id)
                    }
                }
            }
            Section {
                Button {
                    Task { await center.connect(provider, using: model) }
                } label: {
                    HStack {
                        Label(connections.isEmpty ? "Connect" : "Add another account", systemImage: "plus.circle")
                        Spacer()
                        if center.operation == provider.id { ProgressView() }
                    }
                }
                .disabled(center.operation != nil)
                .accessibilityIdentifier("connector-add-account")
            }
            if provider.capabilities.count > 1 {
                Section("Services") {
                    ForEach(provider.capabilities) { capability in
                        HStack(spacing: 12) {
                            Image(systemName: capabilitySymbol(capability.id))
                                .foregroundStyle(.blue)
                                .frame(width: 26)
                            Text(capability.name)
                            Spacer()
                            if overview?.statuses[capability.id]?.connected == true {
                                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                            }
                        }
                    }
                }
            }
            if let error = center.error {
                Section { Text(error).font(.subheadline).foregroundStyle(.secondary) }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(provider.name)
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await center.load(using: model) }
        .confirmationDialog(
            "Revoke \(pendingRevocation?.label ?? "this account")?",
            isPresented: Binding(
                get: { pendingRevocation != nil },
                set: { if !$0 { pendingRevocation = nil } }
            ),
            titleVisibility: .visible
        ) {
            if let connection = pendingRevocation {
                Button("Revoke account", role: .destructive) {
                    pendingRevocation = nil
                    Task { await center.revoke(connection, from: provider, using: model) }
                }
            }
            Button("Cancel", role: .cancel) { pendingRevocation = nil }
        } message: {
            Text("Nanocodex agents will immediately lose access to this exact account. Other \(provider.name) accounts stay connected.")
        }
    }

    private func capabilitySymbol(_ id: String) -> String {
        switch id {
        case "gmail": "envelope.fill"
        case "gcalendar": "calendar"
        case "gcontacts": "person.crop.circle"
        case "gdocs": "doc.text.fill"
        case "gdrive": "externaldrive.fill"
        case "gsheets": "tablecells.fill"
        case "gslides": "rectangle.on.rectangle.angled"
        case "gtasks": "checkmark.circle.fill"
        default: "link"
        }
    }
}

private struct ConnectorRow: View {
    let provider: ConnectorProviderDefinition
    let action: String?
    var detail: String? = nil
    let busy: Bool

    var body: some View {
        HStack(spacing: 12) {
            ConnectorLogo(provider: provider.id, size: 34)
            Text(provider.name).font(.body).lineLimit(1).layoutPriority(1)
            Spacer(minLength: 10)
            if busy { ProgressView() }
            else if let action { Text(action).font(.body.weight(.medium)).foregroundStyle(.blue) }
            else {
                if let detail { Text(detail).font(.subheadline).foregroundStyle(.secondary).lineLimit(1) }
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
            }
        }
        .contentShape(Rectangle())
    }
}

private struct McpRow: View {
    let connection: McpConnection
    let action: String?
    let busy: Bool

    var body: some View {
        HStack(spacing: 12) {
            ConnectorLogo(provider: "mcp", size: 34)
            Text(connection.name).font(.body).lineLimit(1)
            Spacer(minLength: 10)
            if busy { ProgressView() }
            else if connection.status == .disabled { Text("Disabled").foregroundStyle(.secondary) }
            else if let action { Text(action).font(.body.weight(.medium)).foregroundStyle(.blue) }
        }
        .contentShape(Rectangle())
    }
}

private struct ConnectorLogo: View {
    let provider: String
    let size: CGFloat

    private var symbol: String {
        switch provider {
        case "github": "chevron.left.forwardslash.chevron.right"
        case "slack": "number"
        case "x": "xmark"
        case "spotify": "music.note"
        case "soundcloud": "cloud.fill"
        case "link": "creditcard"
        case "mcp": "network"
        default: "link"
        }
    }
    private var foreground: Color {
        switch provider {
        case "google": .blue
        case "slack": .purple
        case "spotify": .green
        case "soundcloud": .orange
        default: .primary
        }
    }

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.24, style: .continuous)
                .fill(Color(uiColor: .secondarySystemGroupedBackground))
                .shadow(color: .black.opacity(0.07), radius: 5, y: 2)
            if provider == "google" {
                Text("G").font(.system(size: size * 0.54, weight: .bold, design: .rounded))
                    .foregroundStyle(
                        AngularGradient(colors: [.blue, .red, .yellow, .green, .blue], center: .center)
                    )
            } else {
                Image(systemName: symbol).font(.system(size: size * 0.40, weight: .semibold))
                    .foregroundStyle(foreground)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

@MainActor
private final class ConnectorCenter: NSObject, ObservableObject, ASWebAuthenticationPresentationContextProviding {
    @Published var overview: ConnectorOverview?
    @Published var loading = false
    @Published var operation: String?
    @Published var error: String?
    private var authenticationSession: ASWebAuthenticationSession?

    func load(using model: InboxModel) async {
        guard !loading else { return }
        let account = model.vaultIntakeAccount
        loading = true
        defer { loading = false }
        if overview == nil {
            let cached = await model.cachedConnectorOverview()
            guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
            overview = cached
        }
        do {
            let refreshed = try await model.connectorOverview()
            guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
            overview = refreshed
            error = nil
        } catch {
            guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
            self.error = error.localizedDescription
        }
    }

    func connect(_ provider: ConnectorProviderDefinition, using model: InboxModel) async {
        guard operation == nil else { return }
        operation = provider.id
        error = nil
        defer { operation = nil }
        do {
            let authorization = try await model.beginConnectorAuthorization(provider.id)
            if provider.id == "link" {
                await UIApplication.shared.open(authorization.authorizationURL)
                let deadline = Date().addingTimeInterval(600)
                while Date() < deadline {
                    try await Task.sleep(for: .seconds(5))
                    let state = try await model.pollLinkAuthorization(attemptID: authorization.attemptID)
                    if state == "connected" { overview = try await model.connectorOverview(); return }
                    if state == "denied" || state == "expired" {
                        error = "The Link connection was declined or expired. Try connecting again."
                        return
                    }
                }
                error = "The Link connection expired. Try connecting again."
                return
            }
            let addingAnother = !(overview?.connections(for: provider).isEmpty ?? true)
            let callbackURL = try await authenticate(authorization, prefersEphemeral: addingAnother)
            switch try authorization.result(from: callbackURL) {
            case .connected:
                overview = try await model.connectorOverview()
            case .cancelled:
                return
            case .failed:
                error = "\(provider.name) couldn’t be connected. Try again."
            }
        } catch let authenticationError as ASWebAuthenticationSessionError
            where authenticationError.code == .canceledLogin {
            return
        } catch { self.error = error.localizedDescription }
    }

    func revoke(
        _ connection: ConnectorAccountConnection,
        from provider: ConnectorProviderDefinition,
        using model: InboxModel
    ) async {
        guard operation == nil else { return }
        operation = provider.id
        error = nil
        defer { operation = nil }
        do {
            try await model.disconnectConnector(provider.id, connectionID: connection.id)
            overview = try await model.connectorOverview()
        } catch { self.error = error.localizedDescription }
    }

    func addMcp(_ target: String, using model: InboxModel) async -> Bool {
        guard operation == nil else { return false }
        operation = "mcp:add"
        error = nil
        defer { operation = nil }
        do {
            let connection = try await model.addMcpConnection(target)
            let completed = try await completeMcpConnection(connection, using: model)
            overview = try await model.connectorOverview()
            return completed
        } catch let authenticationError as ASWebAuthenticationSessionError
            where authenticationError.code == .canceledLogin {
            if let refreshed = try? await model.connectorOverview() { overview = refreshed }
            return true
        } catch {
            self.error = error.localizedDescription
            if let refreshed = try? await model.connectorOverview() { overview = refreshed }
            return false
        }
    }

    func connect(_ connection: McpConnection, using model: InboxModel) async {
        guard operation == nil else { return }
        operation = "mcp:" + connection.id
        error = nil
        defer { operation = nil }
        do {
            _ = try await completeMcpConnection(connection, using: model)
            overview = try await model.connectorOverview()
        } catch let authenticationError as ASWebAuthenticationSessionError
            where authenticationError.code == .canceledLogin {
            return
        } catch { self.error = error.localizedDescription }
    }

    func revoke(_ connection: McpConnection, using model: InboxModel) async {
        guard operation == nil else { return }
        operation = "mcp:" + connection.id
        error = nil
        defer { operation = nil }
        do {
            try await model.disconnectMcpConnection(connection.id)
            overview = try await model.connectorOverview()
        } catch { self.error = error.localizedDescription }
    }

    private func completeMcpConnection(
        _ connection: McpConnection,
        using model: InboxModel
    ) async throws -> Bool {
        switch try await model.beginMcpAuthorization(connection.id) {
        case .connected:
            return true
        case .authorization(let authorization):
            let callbackURL = try await authenticate(
                authorizationURL: authorization.authorizationURL,
                callbackURL: authorization.callbackURL,
                prefersEphemeral: false
            )
            switch try authorization.result(from: callbackURL) {
            case .connected:
                return true
            case .cancelled:
                return false
            case .failed:
                error = "\(connection.name) couldn’t be connected. Try again."
                return false
            }
        }
    }

    private func authenticate(
        _ authorization: ConnectorAuthorization,
        prefersEphemeral: Bool
    ) async throws -> URL {
        try await authenticate(
            authorizationURL: authorization.authorizationURL,
            callbackURL: authorization.callbackURL,
            prefersEphemeral: prefersEphemeral
        )
    }

    private func authenticate(
        authorizationURL: URL,
        callbackURL: URL,
        prefersEphemeral: Bool
    ) async throws -> URL {
        guard let scheme = callbackURL.scheme else { throw APIError.invalidResponse }
        authenticationSession?.cancel()
        return try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
            let session = ASWebAuthenticationSession(
                url: authorizationURL,
                callbackURLScheme: scheme
            ) { [weak self] callbackURL, error in
                Task { @MainActor in
                    self?.authenticationSession = nil
                    if let callbackURL { continuation.resume(returning: callbackURL) }
                    else { continuation.resume(throwing: error ?? APIError.invalidResponse) }
                }
            }
            session.presentationContextProvider = self
            // A clean provider session makes “Add another” reliable even for
            // providers that would otherwise silently reuse the current login.
            session.prefersEphemeralWebBrowserSession = prefersEphemeral
            authenticationSession = session
            if !session.start() {
                authenticationSession = nil
                continuation.resume(throwing: APIError.invalidResponse)
            }
        }
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first(where: \.isKeyWindow) ?? UIWindow()
    }
}

private struct NativeVaultView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.scenePhase) private var scenePhase
    var chatOnly = false
    @State private var refreshOperations: [String: UUID] = [:]
    @State private var addingSSH = false
    @State private var cloudflare: [ConnectorAccountConnection] = []
    @State private var overview: VaultOverview?
    @State private var adding = false
    @State private var deleting: VaultItem?
    @State private var deletingSSH: SSHVaultIdentity?
    @State private var busy = false
    @State private var message = ""
    @State private var tokenID = ""
    @State private var accountID = ""
    @State private var captureID = ""
    @State private var captureOperation = UUID()
    @State private var addressID = ""
    @State private var login: ChatGPTLoginReceipt?
    @State private var openAI = false

    var body: some View {
        let account = model.vaultIntakeAccount
        Form {
            if !chatOnly {
            Section("Vault") {
                ForEach(overview?.items ?? []) { item in
                    VStack(alignment: .leading) {
                        Text(item.name).font(.headline)
                        Text(item.kind + (item.detail.isEmpty ? "" : " · " + item.detail)).font(.caption).foregroundStyle(.secondary)
                        HStack {
                            Button("Delete", role: .destructive) { deleting = item }
                            if item.kind == "card" {
                                Button("Balance") { run { client in
                                    let receipt = try await client.providerCard(operation: "balance", vaultID: item.id)
                    guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                                    message = try ProviderCardReceipt(receipt).display
                                } }
                                Button("Refresh") {
                                    if refreshOperations[item.id] == nil { refreshOperations[item.id] = UUID() }
                                    run { client in
                                    let receipt = try await client.providerCard(operation: "refresh", vaultID: item.id, operationID: refreshOperations[item.id])
                    guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                                    let parsed = try ProviderCardReceipt(receipt)
                                    message = parsed.display
                                    if parsed.status != "outcome_unknown" { refreshOperations[item.id] = nil }
                                } }
                            }
                        }
                    }
                }
                Button("Add Vault item") { adding = true }
                Button("Set OpenAI API key") { openAI = true }
                Button("Remove OpenAI API key", role: .destructive) { run { client in
                    _ = try await client.nativeCredentialOperation(path: "/v1/credentials/openai", method: "DELETE")
                } }
            }
            Section("SSH identities") {
                Button("Add SSH identity") { addingSSH = true }
                ForEach(overview?.ssh ?? []) { identity in
                    VStack(alignment: .leading) {
                        Text(identity.id + " · " + identity.hostname)
                        Text(identity.publicKey).font(.caption).textSelection(.enabled)
                        Button("Delete", role: .destructive) { deletingSSH = identity }
                    }
                }
            }
            Section("Cloudflare") {
                Text(cloudflare.isEmpty ? "No connected account" : "Connected")
                ForEach(cloudflare) { connection in
                    Text(connection.label)
                    Button("Disconnect " + connection.label, role: .destructive) { run { try await $0.disconnectCloudflare(connectionID: connection.id) } }
                }

                Picker("API token", selection: $tokenID) {
                    Text("Select token").tag("")
                    ForEach((overview?.items ?? []).filter { $0.kind == "api_key" }) { Text($0.name).tag($0.id) }
                }
                TextField("Account ID (account token only)", text: $accountID).autocorrectionDisabled().textInputAutocapitalization(.never)
                Button("Connect Cloudflare") { run { client in
                    var body: [String: JSON] = ["vault_id": .string(tokenID)]
                    if !accountID.isEmpty { body["account_id"] = .string(accountID) }
                    _ = try await client.nativeCredentialOperation(path: "/v1/connectors/cloudflare", method: "POST", body: .object(body))
                    guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                    message = "Cloudflare connection submitted."
                } }.disabled(tokenID.isEmpty)
            }
            Section("Provider capture") {
                TextField("Capture ID", text: $captureID).autocorrectionDisabled().textInputAutocapitalization(.never)
                    .onChange(of: captureID) { _, _ in captureOperation = UUID() }
                Picker("Billing address", selection: $addressID) {
                    Text("None").tag("")
                    ForEach((overview?.items ?? []).filter { $0.kind == "address" }) { Text($0.name).tag($0.id) }
                }
                .onChange(of: addressID) { _, _ in captureOperation = UUID() }
                Button("Save capture") { run { client in
                    let receipt = try await client.storeProviderCapture(captureID: captureID, operationID: captureOperation, addressVaultID: addressID.isEmpty ? nil : addressID)
                    guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                    message = try ProviderCardReceipt(receipt).display
                } }.disabled(captureID.isEmpty)
            }
            }
            if chatOnly { Section("ChatGPT") {
                Text(login?.state ?? "Check account status")
                if let code = login?.userCode { Text(code).textSelection(.enabled) }
                if let url = login?.verificationURL {
                    Link("Authorize device", destination: url)
                }
                Button("Start device login") { chatGPT("POST") }
                Button("Check login status") { chatGPT("GET") }
                Button("Disconnect ChatGPT", role: .destructive) { run { client in
                    _ = try await client.nativeCredentialOperation(path: "/v1/credentials/chatgpt", method: "DELETE")
                    guard model.vaultIntakeAccount == account else { return }
                    login = nil
                    await model.refreshModelCatalog()
                } }
            }
            }
            if busy { ProgressView() }
            if !message.isEmpty { Section { Text(message) } }
        }
        .navigationTitle(chatOnly ? "ChatGPT accounts" : "Vault")
        .disabled(busy)
        .task { reload() }
        .refreshable { reload() }
        .onChange(of: model.vaultIntakeAccount) { _, _ in
            overview = nil; cloudflare = []; login = nil; message = ""
            tokenID = ""; accountID = ""; captureID = ""; addressID = ""
            refreshOperations = [:]; captureOperation = UUID()
            adding = false; addingSSH = false; openAI = false
            deleting = nil; deletingSSH = nil
        }
        .onChange(of: scenePhase) { _, phase in if phase != .active { login = nil } }
        .onDisappear { login = nil }
        .sheet(isPresented: $openAI, onDismiss: { reload() }) { NativeVaultAddView(model: model, kind: "openai") }
        .sheet(isPresented: $addingSSH, onDismiss: { reload() }) { NativeVaultAddView(model: model, kind: "ssh") }
        .sheet(isPresented: $adding, onDismiss: { reload() }) { NativeVaultAddView(model: model) }
        .confirmationDialog("Delete this Vault item?", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
            Button("Delete", role: .destructive) { if let item = deleting { deleting = nil; run { try await $0.deleteVaultItem(item) } } }
        }
        .confirmationDialog("Delete this SSH identity?", isPresented: Binding(get: { deletingSSH != nil }, set: { if !$0 { deletingSSH = nil } })) {
            Button("Delete", role: .destructive) { if let item = deletingSSH { deletingSSH = nil; run { try await $0.deleteSSHIdentity(reference: item.id) } } }
        }
    }
    private func chatGPT(_ method: String) {
        let account = model.vaultIntakeAccount
        run { client in
            let value = try await client.nativeCredentialOperation(path: "/v1/credentials/chatgpt/login", method: method)
            guard model.vaultIntakeAccount == account else { return }
            guard scenePhase == .active else { return }
            login = try ChatGPTLoginReceipt(value)
            await model.refreshModelCatalog()
        }
    }
    private func reload() { if chatOnly { chatGPT("GET") } else { run { _ in } } }
    private func run(_ action: @escaping (ManagedClient) async throws -> Void) {
        guard !busy else { return }
        let account = model.vaultIntakeAccount
        busy = true
        Task { @MainActor in
            defer { busy = false }
            do {
                guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                let client = try model.vaultManagementClient()
                try await action(client)
                let loaded = try await client.vaultOverview()
                let connections = try await model.connectorOverview().statuses["cloudflare"]?.connections ?? []
                guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                overview = loaded
                cloudflare = connections
            } catch { guard model.vaultIntakeAccount == account else { return }; message = "The Vault request could not be completed. Check status before retrying a submission." }
        }
    }
}

private struct NativeVaultAddView: View {
    @ObservedObject var model: InboxModel
    @Environment(\.dismiss) private var dismiss
    @State var kind = "login"
    @State private var operationID = UUID()
    @State private var values: [String: String] = [:]
    @State private var busy = false
    @State private var error = ""
    private var fields: [String] {
        switch kind {
        case "openai": ["api_key"]
        case "api_key": ["name", "api_key"]
        case "card": ["name", "card_number", "expiry_month", "expiry_year", "cvv", "billing_zip"]
        case "address": ["name", "address_line_1", "address_line_2", "city", "state", "zip", "country"]
        case "phone": ["name", "phone_number"]
        case "ssh": ["reference", "hostname", "port", "username", "host_key_sha256", "private_key"]
        default: ["name", "username", "password", "browser_origin"]
        }
    }
    var body: some View {
        NavigationStack {
            Form {
                Picker("Kind", selection: $kind) {
                    ForEach(["login", "api_key", "card", "address", "phone", "ssh", "openai"], id: \.self) { Text($0).tag($0) }
                }.onChange(of: kind) { _, _ in values.removeAll() }
                ForEach(fields, id: \.self) { key in
                    let binding = Binding(get: { values[key] ?? "" }, set: { values[key] = $0 })
                    if ["password", "api_key", "card_number", "cvv", "private_key"].contains(key) {
                        SecureField(key.replacingOccurrences(of: "_", with: " "), text: binding)
                    } else {
                        TextField(key.replacingOccurrences(of: "_", with: " "), text: binding)
                            .autocorrectionDisabled().textInputAutocapitalization(.never)
                    }
                }
                if kind == "ssh" { Text("Leave private key empty to generate a new key. Install the resulting public key on the server.").font(.caption) }
                if !error.isEmpty { Text(error) }
            }
            .navigationTitle(kind == "openai" ? "OpenAI API key" : "Add Vault item")
            .onChange(of: model.vaultIntakeAccount) { _, _ in values.removeAll(); dismiss() }
            .onChange(of: values) { _, _ in operationID = UUID() }
            .onDisappear { values.removeAll() }
            .disabled(busy)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { values.removeAll(); dismiss() } }
                ToolbarItem(placement: .confirmationAction) { Button("Save") { save() } }
            }
        }
    }
    private func save() {
        guard !busy else { return }; let account = model.vaultIntakeAccount; busy = true
        Task { @MainActor in
            defer { busy = false }
            do {
                guard !Task.isCancelled, model.vaultIntakeAccount == account else { return }
                let client = try model.vaultManagementClient()
                if kind == "openai" {
                    _ = try await client.nativeCredentialOperation(path: "/v1/credentials/openai", method: "PUT", body: .object(["api_key": .string(values["api_key"] ?? "")]))
                } else if kind == "ssh" {
                    try await client.saveSSHIdentity(reference: values["reference"] ?? "", hostname: values["hostname"] ?? "", port: Int(values["port"] ?? "22") ?? 22, username: values["username"] ?? "", hostKeySHA256: values["host_key_sha256"] ?? "", privateKey: (values["private_key"] ?? "").isEmpty ? nil : values["private_key"])
                } else {
                    _ = try await client.saveVaultItem(kind: kind, values: values.filter { !["cvv", "address_line_2", "browser_origin"].contains($0.key) || !$0.value.isEmpty }, operationID: operationID)
                }
                guard !Task.isCancelled, model.vaultIntakeAccount == account else { values.removeAll(); return }
                values.removeAll(); dismiss()
            } catch { guard model.vaultIntakeAccount == account else { values.removeAll(); return }; self.error = "Could not save. Check Vault before retrying." }
        }
    }
}
