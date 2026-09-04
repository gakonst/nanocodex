import Foundation
import Security

struct RuntimeFailure: LocalizedError {
    var message: String
    var errorDescription: String? { message }
}

@MainActor
final class RuntimeClient {
    var onEvent: ((JSONValue) -> Void)?
    var onFailure: ((String) -> Void)?
    private var process: Process?
    private var input: FileHandle?
    private var output: FileHandle?
    private var diagnostics: FileHandle?
    private var pending: [String: CheckedContinuation<JSONValue, Error>] = [:]
    private var deadlines: [String: Task<Void, Never>] = [:]
    private var buffer = Data()
    private var stopped = false
    private var nextID = 0
    private let dataDirectory: String?
    #if DEBUG
    var requestOverride: ((String, [JSONValue]) async throws -> JSONValue)?
    #endif

    init(dataDirectory: String? = nil) { self.dataDirectory = dataDirectory }

    func start(credential: AccountKeychain.Credential? = nil) throws {
        guard process == nil else { return }
        guard let resources = Bundle.main.resourceURL else { throw RuntimeFailure(message: "Nanocodex’s app resources are missing.") }
        let host = resources.appendingPathComponent("runtime/host.mjs")
        let node = resources.appendingPathComponent("runtime/node")
        guard FileManager.default.isExecutableFile(atPath: node.path), FileManager.default.fileExists(atPath: host.path) else {
            throw RuntimeFailure(message: "Nanocodex’s runtime is missing. Rebuild the app with its bundled runtime.")
        }
        let child = Process(), stdin = Pipe(), stdout = Pipe(), stderr = Pipe()
        child.executableURL = node; child.arguments = [host.path]
        var env = ProcessInfo.processInfo.environment.filter { ["HOME", "PATH", "TMPDIR", "LANG", "USER", "NC_API_KEY", "NANOCODEX_API_KEY", "NANOCODEX_MANAGED_URL", "NANOCODEX_HAND_BINARY", "NANOCODEX_VM_ROOTFS", "NANOCODEX_VM_GUEST_RUNTIME", "NANOCODEX_KRUNFW_DIR"].contains($0.key) }
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Nanocodex/Native")
        env["NANOCODEX_DESKTOP_DATA"] = dataDirectory ?? ProcessInfo.processInfo.environment["NANOCODEX_DESKTOP_DATA"] ?? support.path
        #if DEBUG
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        if FileManager.default.fileExists(atPath: root.appendingPathComponent(".env").path) { env["NANOCODEX_ENV_FILE"] = root.appendingPathComponent(".env").path }
        #endif
        if let explicit = ProcessInfo.processInfo.environment["NANOCODEX_ENV_FILE"] { env["NANOCODEX_ENV_FILE"] = explicit }
        if let credential {
            env["NANOCODEX_API_KEY"] = credential.apiKey
            env["NANOCODEX_MANAGED_URL"] = credential.baseUrl
        }
        child.environment = env
        child.standardInput = stdin; child.standardOutput = stdout; child.standardError = stderr
        stdout.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if data.isEmpty { handle.readabilityHandler = nil; return }
            Task { @MainActor [weak self] in self?.receive(data) }
        }
        // Consume diagnostic output without copying credentials or subprocess text to the UI/logs.
        stderr.fileHandleForReading.readabilityHandler = { handle in
            if handle.availableData.isEmpty { handle.readabilityHandler = nil }
        }
        child.terminationHandler = { [weak self] _ in Task { @MainActor [weak self] in self?.terminated() } }
        try child.run()
        process = child; input = stdin.fileHandleForWriting; output = stdout.fileHandleForReading; diagnostics = stderr.fileHandleForReading
    }

    func call<T: Decodable>(_ method: String, _ args: [JSONValue] = [], as type: T.Type = T.self) async throws -> T {
        let result = try await request(method, args)
        return try result.decode(type)
    }
    @discardableResult
    func request(_ method: String, _ args: [JSONValue] = []) async throws -> JSONValue {
        #if DEBUG
        if let requestOverride { return try await requestOverride(method, args) }
        #endif
        guard let input, !stopped else { throw RuntimeFailure(message: "The Nanocodex runtime is unavailable. Quit and reopen Nanocodex.") }
        nextID += 1; let id = String(nextID)
        let message: JSONValue = .object(["id": .string(id), "method": .string(method), "args": .array(args)])
        var data = try JSONEncoder().encode(message); data.append(0x0a)
        return try await withCheckedThrowingContinuation { continuation in
            pending[id] = continuation
            deadlines[id] = Task { [weak self] in
                try? await Task.sleep(for: .seconds(60))
                guard !Task.isCancelled, let self else { return }
                self.pending.removeValue(forKey: id)?.resume(throwing: RuntimeFailure(message: "Nanocodex’s runtime did not respond. Reopen the app to reconnect."))
                self.deadlines.removeValue(forKey: id)
            }
            do { try input.write(contentsOf: data) }
            catch { deadlines.removeValue(forKey: id)?.cancel(); pending.removeValue(forKey: id)?.resume(throwing: error) }
        }
    }
    private func receive(_ data: Data) {
        guard !data.isEmpty else { return }
        buffer.append(data)
        while let newline = buffer.firstIndex(of: 0x0a) {
            let line = buffer.prefix(upTo: newline); buffer.removeSubrange(...newline)
            guard let value = try? JSONDecoder().decode(JSONValue.self, from: line) else { continue }
            if value["event"] != .null { onEvent?(value["event"]); continue }
            let id = value["id"].string
            guard let continuation = pending.removeValue(forKey: id) else { continue }
            deadlines.removeValue(forKey: id)?.cancel()
            if value["error"] != .null { continuation.resume(throwing: RuntimeFailure(message: value["error"].string)) }
            else { continuation.resume(returning: value["result"]) }
        }
    }
    private func terminated() {
        for continuation in pending.values { continuation.resume(throwing: RuntimeFailure(message: "Nanocodex’s runtime stopped. Reopen the app to reconnect.")) }
        pending.removeAll()
        deadlines.values.forEach { $0.cancel() }; deadlines.removeAll()
        if !stopped { onFailure?("Nanocodex’s runtime stopped. Reopen the app to reconnect.") }
    }
    func stop() {
        guard !stopped else { return }; stopped = true
        try? input?.close(); input = nil
        output?.readabilityHandler = nil
        diagnostics?.readabilityHandler = nil
        // stdin EOF lets the helper stop Hands and release every owned process before exiting.
        let child = process
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 25) { if child?.isRunning == true { child?.terminate() } }
    }
}

enum AccountKeychain {
    private static let service = "xyz.paradigm.nanocodex.native.account"
    struct Credential: Codable { var baseUrl: String; var apiKey: String }
    static func environmentCredential() -> Credential? {
        var values: [String: String] = [:]
        var file = ProcessInfo.processInfo.environment["NANOCODEX_ENV_FILE"]
        #if DEBUG
        if file == nil { file = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent(".env").path }
        #endif
        if let file, let contents = try? String(contentsOfFile: file, encoding: .utf8) {
            for raw in contents.components(separatedBy: .newlines) {
                var line = raw.trimmingCharacters(in: .whitespaces)
                if line.hasPrefix("export ") { line = String(line.dropFirst(7)) }
                guard !line.hasPrefix("#"), let equals = line.firstIndex(of: "=") else { continue }
                let name = String(line[..<equals]).trimmingCharacters(in: .whitespaces)
                guard ["NC_API_KEY", "NANOCODEX_API_KEY", "NANOCODEX_MANAGED_URL"].contains(name) else { continue }
                var value = String(line[line.index(after: equals)...]).trimmingCharacters(in: .whitespaces)
                if value.count >= 2, let first = value.first, ["\"", "'"].contains(String(first)), value.last == first { value = String(value.dropFirst().dropLast()) }
                else { value = value.components(separatedBy: " #").first ?? value }
                values[name] = value
            }
        }
        values.merge(ProcessInfo.processInfo.environment) { _, environment in environment }
        guard let key = values["NANOCODEX_API_KEY"] ?? values["NC_API_KEY"], !key.isEmpty else { return nil }
        return Credential(baseUrl: values["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev", apiKey: key)
    }
    static func read() -> Credential? {
        var result: CFTypeRef?
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "managed", kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne]
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        return try? JSONDecoder().decode(Credential.self, from: data)
    }
    static func save(_ credential: Credential) throws {
        let data = try JSONEncoder().encode(credential)
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "managed"]
        let updated = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if updated == errSecSuccess { return }
        guard updated == errSecItemNotFound else { throw RuntimeFailure(message: "macOS could not save this account in Keychain (\(updated)).") }
        var insert = query
        insert[kSecValueData as String] = data
        insert[kSecAttrLabel as String] = "Nanocodex managed account"
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(insert as CFDictionary, nil)
        guard status == errSecSuccess else { throw RuntimeFailure(message: "macOS could not save this account in Keychain (\(status)).") }
    }
    static func remove() { SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "managed"] as CFDictionary) }
    static func removeChecked() throws {
        let status = SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "managed"] as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw RuntimeFailure(message: "macOS could not update the saved account in Keychain (\(status)).") }
    }
}
