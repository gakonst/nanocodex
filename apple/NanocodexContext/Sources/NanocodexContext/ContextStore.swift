import Foundation
import Darwin

public struct CaptureSession: Sendable {
    public let scope: String
    fileprivate let generation: String
}

/// Shared by the containing app, its App Intents, and its share extension.
/// All read/modify/write operations take a cross-process lock, then replace the
/// file atomically. No credentials are written to this container.
public struct ContextStore: Sendable {
    public static let appGroup = "group.xyz.paradigm.centaur"
    public let directory: URL
    public init(directory: URL) { self.directory = directory }

    public static func shared() throws -> Self {
        #if os(iOS)
        guard let root = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup) else { throw CaptureError.unavailable }
        #else
        let root = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            .appendingPathComponent("xyz.paradigm.centaur", isDirectory: true)
        #endif
        return Self(directory: root.appendingPathComponent("Context", isDirectory: true))
    }

    private struct Account: Codable {
        var enabled = false
        var items: [CapturedContext] = []
        var routes: [String: String] = [:]
    }
    private struct State: Codable {
        var version = 1
        var activeScope: String?
        var generation: String?
        var accounts: [String: Account] = [:]
    }

    public func activate(_ scope: String?) throws {
        try transaction { state in state.activeScope = scope; state.generation = UUID().uuidString }
    }
    public func captureScope() throws -> String {
        try captureSession().scope
    }
    public func captureSession() throws -> CaptureSession {
        try transaction(write: false) { state in
            guard let scope = state.activeScope, state.accounts[scope]?.enabled == true else { throw CaptureError.disabled }
            return CaptureSession(scope: scope, generation: state.generation ?? "")
        }
    }
    public func snapshot(scope: String) throws -> ContextSnapshot {
        try transaction(write: false) { state in
            let account = state.accounts[scope] ?? Account()
            return ContextSnapshot(enabled: account.enabled, items: account.items.reversed(), routes: account.routes)
        }
    }
    /// Hand queries must resolve consent and the active account in one transaction.
    public func handSnapshot(scope: String) throws -> ContextSnapshot {
        try transaction(write: false) { state in
            guard state.activeScope == scope else { throw CaptureError.accountChanged }
            let account = state.accounts[scope] ?? Account()
            return ContextSnapshot(enabled: account.enabled, items: account.enabled ? account.items.reversed() : [], routes: [:])
        }
    }
    public func setEnabled(_ enabled: Bool, scope: String) throws {
        try transaction { state in
            guard state.activeScope == scope else { throw CaptureError.accountChanged }
            state.accounts[scope, default: Account()].enabled = enabled
            state.generation = UUID().uuidString
        }
    }
    public func route(source: String, agentID: String?, scope: String) throws {
        try transaction { state in
            guard state.activeScope == scope else { throw CaptureError.accountChanged }
            state.accounts[scope, default: Account()].routes[source.lowercased()] = agentID
        }
    }
    @discardableResult
    public func capture(_ inputs: [CaptureInput], session: CaptureSession) throws -> [CapturedContext] {
        try capture(inputs, scope: session.scope, generation: session.generation)
    }
    @discardableResult
    public func capture(_ inputs: [CaptureInput], scope: String, now: Date = Date()) throws -> [CapturedContext] {
        try capture(inputs, scope: scope, now: now, generation: nil)
    }
    private func capture(_ inputs: [CaptureInput], scope: String, now: Date = Date(), generation: String?) throws -> [CapturedContext] {
        guard !inputs.isEmpty else { throw CaptureError.empty }
        let inputs = try inputs.map { try $0.validated() }
        return try transaction { state in
            guard state.activeScope == scope else { throw CaptureError.accountChanged }
            if let generation, state.generation != generation { throw CaptureError.accountChanged }
            guard var account = state.accounts[scope], account.enabled else { throw CaptureError.disabled }
            var results: [CapturedContext] = []
            for input in inputs {
                if let existing = account.items.last(where: { item in
                    guard item.input.sourceKey == input.sourceKey else { return false }
                    if !input.externalID.isEmpty { return item.input.externalID == input.externalID && item.input.thread == input.thread && item.input.sender == input.sender }
                    // Without a provider ID, an exact retry is deduplicated only
                    // briefly; identical later messages remain distinct events.
                    return item.input == input && abs(now.timeIntervalSince(item.capturedAt)) <= 300
                }) { results.append(existing); continue }
                let item = CapturedContext(input: input, capturedAt: now)
                account.items.append(item); results.append(item)
            }
            state.accounts[scope] = account
            return results
        }
    }
    public func markUsed(_ ids: [String], agentID: String, turnID: String, scope: String) throws {
        try transaction { state in
            guard state.activeScope == scope else { throw CaptureError.accountChanged }
            guard var account = state.accounts[scope] else { return }
            let selected = Set(ids)
            for index in account.items.indices where selected.contains(account.items[index].id) {
                account.items[index].usedBy[agentID] = turnID
            }
            state.accounts[scope] = account
        }
    }
    public func remove(_ ids: Set<String>, scope: String) throws {
        try transaction { state in
            guard state.activeScope == scope else { throw CaptureError.accountChanged }
            state.accounts[scope]?.items.removeAll { ids.contains($0.id) }
        }
    }

    private func transaction<T>(write: Bool = true, _ operation: (inout State) throws -> T) throws -> T {
        let manager = FileManager.default
        try manager.createDirectory(at: directory, withIntermediateDirectories: true)
        #if os(iOS)
        try manager.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], ofItemAtPath: directory.path)
        #endif
        var root = directory
        var values = URLResourceValues(); values.isExcludedFromBackup = true
        try root.setResourceValues(values)
        let descriptor = open(directory.appendingPathComponent("context.lock").path, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR)
        guard descriptor >= 0 else { throw CaptureError.unavailable }
        defer { close(descriptor) }
        guard flock(descriptor, LOCK_EX) == 0 else { throw CaptureError.unavailable }
        defer { flock(descriptor, LOCK_UN) }
        let file = directory.appendingPathComponent("context.json")
        var state = State()
        if manager.fileExists(atPath: file.path) {
            // Corrupt or unknown storage must not silently reset consent or data.
            let data = try Data(contentsOf: file, options: .mappedIfSafe)
            state = try JSONDecoder().decode(State.self, from: data)
            guard state.version == 1 else { throw CaptureError.unavailable }
        }
        let result = try operation(&state)
        if write {
            let data = try JSONEncoder().encode(state)
            #if os(iOS)
            try data.write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            #else
            try data.write(to: file, options: .atomic)
            try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
            #endif
        }
        return result
    }
}
