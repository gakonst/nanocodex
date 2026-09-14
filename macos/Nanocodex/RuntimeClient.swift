import Foundation
import Security

struct RuntimeFailure: LocalizedError {
    var message: String
    var errorDescription: String? { message }
}

enum RuntimeEvent: Decodable, Sendable {
    case state(DesktopState), thread(ThreadSnapshot), ignored
    private enum CodingKeys: String, CodingKey { case type, state, thread }
    init(from decoder: Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        switch try fields.decode(String.self, forKey: .type) {
        case "state": self = .state(try fields.decode(DesktopState.self, forKey: .state))
        case "thread": self = .thread(try fields.decode(ThreadSnapshot.self, forKey: .thread))
        default: self = .ignored
        }
    }
}
private struct RuntimeFrame: Decodable, Sendable {
    var id: JSONValue?
    var event: RuntimeEvent?
    var result: JSONValue?
    var error: String?
}

/// Keep JSON decoding and fragmented-frame assembly off the main actor. One
/// serial queue preserves stdout order, including responses mixed with events.
private final class RuntimeFrameDecoder: @unchecked Sendable {
    private let queue = DispatchQueue(label: "xyz.paradigm.nanocodex.runtime.decode", qos: .userInitiated)
    private var buffer = Data()
    private let decoder = JSONDecoder()

    func receive(_ data: Data, deliver: @escaping @Sendable ([RuntimeFrame]) -> Void) {
        queue.async { deliver(self.decode(data)) }
    }
    func finish(_ completion: @escaping @Sendable () -> Void) { queue.async(execute: completion) }
    #if DEBUG
    func receiveForTesting(_ data: Data) -> [RuntimeFrame] { queue.sync { decode(data) } }
    #endif
    private func decode(_ data: Data) -> [RuntimeFrame] {
        guard !data.isEmpty else { return [] }
        buffer.append(data)
        var frames: [RuntimeFrame] = [], consumed = buffer.startIndex
        while let newline = buffer[consumed...].firstIndex(of: 0x0a) {
            if let value = try? decoder.decode(RuntimeFrame.self, from: buffer[consumed..<newline]) { frames.append(value) }
            consumed = buffer.index(after: newline)
        }
        if consumed != buffer.startIndex { buffer.removeSubrange(..<consumed) }
        return frames
    }
}

@MainActor
final class RuntimeClient {
    var onEvent: ((RuntimeEvent) -> Void)?
    var onFailure: ((String) -> Void)?
    private var process: Process?
    private var input: FileHandle?
    private var output: FileHandle?
    private var diagnostics: FileHandle?
    private var pending: [String: CheckedContinuation<JSONValue, Error>] = [:]
    private var deadlines: [String: Task<Void, Never>] = [:]
    private let decoder = RuntimeFrameDecoder()
    private var stopped = false
    private var processExited = false
    private var outputEnded = false
    private var nextID = 0
    private let dataDirectory: String?
    #if DEBUG
    var requestOverride: ((String, [JSONValue]) async throws -> JSONValue)?
    func receiveForTesting(_ data: Data) { receive(decoder.receiveForTesting(data)) }
    func receiveAsynchronouslyForTesting(_ data: Data) {
        decoder.receive(data) { [weak self] frames in
            DispatchQueue.main.async { [weak self] in self?.receive(frames) }
        }
    }
    func startForTesting(executable: URL, arguments: [String]) throws {
        let child = Process()
        child.executableURL = executable; child.arguments = arguments
        child.environment = ["PATH": "/usr/bin:/bin"]
        try launch(child)
    }
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
        let child = Process()
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
        try launch(child)
    }
    private func launch(_ child: Process) throws {
        let stdin = Pipe(), stdout = Pipe(), stderr = Pipe()
        child.standardInput = stdin; child.standardOutput = stdout; child.standardError = stderr
        let decoder = decoder
        stdout.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if data.isEmpty {
                handle.readabilityHandler = nil
                // EOF is queued behind every decoded stdout chunk. Process exit
                // alone can precede the final readability callback.
                decoder.finish { [weak self] in
                    DispatchQueue.main.async { [weak self] in self?.outputEnded = true; self?.finishTermination() }
                }
                return
            }
            decoder.receive(data) { [weak self] frames in
                DispatchQueue.main.async { [weak self] in self?.receive(frames) }
            }
        }
        // Consume diagnostic output without copying credentials or subprocess text to the UI/logs.
        stderr.fileHandleForReading.readabilityHandler = { handle in
            if handle.availableData.isEmpty { handle.readabilityHandler = nil }
        }
        child.terminationHandler = { [weak self] _ in
            DispatchQueue.main.async { [weak self] in self?.processExited = true; self?.finishTermination() }
        }
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
    private func receive(_ frames: [RuntimeFrame]) {
        guard !stopped else { return }
        for value in frames {
            if let event = value.event { onEvent?(event); continue }
            let id = value.id?.string ?? ""
            guard let continuation = pending.removeValue(forKey: id) else { continue }
            deadlines.removeValue(forKey: id)?.cancel()
            if let error = value.error { continuation.resume(throwing: RuntimeFailure(message: error)) }
            else { continuation.resume(returning: value.result ?? .null) }
        }
    }
    private func finishTermination() {
        guard processExited, outputEnded else { return }
        let expected = stopped
        stopped = true
        try? input?.close(); input = nil
        output?.readabilityHandler = nil; diagnostics?.readabilityHandler = nil
        for continuation in pending.values { continuation.resume(throwing: RuntimeFailure(message: "Nanocodex’s runtime stopped. Reopen the app to reconnect.")) }
        pending.removeAll()
        deadlines.values.forEach { $0.cancel() }; deadlines.removeAll()
        if !expected { onFailure?("Nanocodex’s runtime stopped. Reopen the app to reconnect.") }
    }
    func stop() {
        guard !stopped else { return }; stopped = true
        try? input?.close(); input = nil
        output?.readabilityHandler = nil
        diagnostics?.readabilityHandler = nil
        outputEnded = true
        finishTermination()
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
