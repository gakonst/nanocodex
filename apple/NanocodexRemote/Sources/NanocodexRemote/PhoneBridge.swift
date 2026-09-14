#if os(macOS)
import Foundation

public struct PairedPhone: Codable, Identifiable, Hashable, Sendable {
    public let id: String
    public let name: String
}

public struct PhoneBridgeConfiguration: Sendable {
    public let deviceID: String
    public let runner: URL
    var companionExecutable: URL?
    public init(deviceID: String, runner: URL) { self.deviceID = deviceID; self.runner = runner }
}

@MainActor
final class PhoneBridge: RemoteCapture {
    nonisolated(unsafe) var onFailure: @Sendable (Error) -> Void = { _ in }
    private var process: Process?
    private var parent: Pipe?
    private var output: Pipe?
    private var ready = false
    private var readiness = Data()
    private var epoch = UUID()
    private var log: FileHandle?
    private var logURL: URL?
    private let executable: URL

    init(executable: URL = PhoneBridge.bundledExecutable) { self.executable = executable }
    nonisolated static var bundledExecutable: URL { Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/nanocodex-remote") }

    nonisolated private static func command(_ executable: URL, arguments: [String]) -> Process {
        let process = Process(); process.executableURL = executable; process.arguments = arguments
        var environment = ["PATH": "/usr/bin:/bin:/usr/sbin:/sbin"]
        for key in ["HOME", "USER", "TMPDIR", "LANG", "DEVELOPER_DIR"] {
            if let value = ProcessInfo.processInfo.environment[key] { environment[key] = value }
        }
        process.environment = environment
        return process
    }

    static func devices(executable: URL = bundledExecutable) async throws -> [PairedPhone] {
        try await Task.detached {
            let process = command(executable, arguments: ["phone-list"]), output = Pipe()
            process.standardOutput = output; process.standardError = FileHandle.nullDevice
            try process.run()
            let data = output.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0, data.count <= 64 * 1024 else { throw RemoteError.phoneBridge }
            return try JSONDecoder().decode([PairedPhone].self, from: data)
        }.value
    }

    func start(_ configuration: PhoneBridgeConfiguration) async throws {
        await stop()
        let attempt = UUID(); epoch = attempt; ready = false; readiness = Data()
        guard FileManager.default.isExecutableFile(atPath: executable.path) else { throw RemoteError.phoneBridge }
        let process = Self.command(executable, arguments: ["phone-bridge", "--udid", configuration.deviceID,
            "--xctestrun", configuration.runner.path, "--parent-stdin"])
        let parent = Pipe(), output = Pipe()
        let logURL = FileManager.default.temporaryDirectory.appendingPathComponent("nanocodex-phone-\(attempt).log")
        guard FileManager.default.createFile(atPath: logURL.path, contents: nil, attributes: [.posixPermissions: 0o600]) else { throw RemoteError.phoneBridge }
        let log = try FileHandle(forWritingTo: logURL)
        self.log = log; self.logURL = logURL; self.process = process; self.parent = parent; self.output = output
        process.standardInput = parent; process.standardOutput = output; process.standardError = log
        output.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            // The helper emits one small readiness record, after owning both
            // loopback listeners and receiving a ready response from the runner.
            guard !data.isEmpty, data.count <= 4096 else { return }
            Task { @MainActor [weak self] in
                guard let self, self.epoch == attempt, !self.ready, self.readiness.count + data.count <= 4096 else { return }
                self.readiness.append(data)
                struct Ready: Decodable { let type: String; let deviceID: String }
                guard let result = try? JSONDecoder().decode(Ready.self, from: self.readiness),
                      result.type == "ready", result.deviceID == configuration.deviceID else { return }
                self.ready = true
            }
        }
        process.terminationHandler = { [weak self] _ in
            Task { @MainActor [weak self] in
                guard let self, self.epoch == attempt, self.ready else { return }
                self.onFailure(RemoteError.phoneBridge)
            }
        }
        do {
            try process.run()
            let deadline = ProcessInfo.processInfo.systemUptime + 95
            while !ready {
                guard epoch == attempt, !Task.isCancelled else { throw CancellationError() }
                guard process.isRunning, ProcessInfo.processInfo.systemUptime < deadline else { throw RemoteError.phoneBridge }
                try await Task.sleep(for: .milliseconds(100))
            }
        } catch { if epoch == attempt { await stop() }; throw error }
    }

    func stop() async {
        epoch = UUID(); ready = false
        let process = self.process, parent = self.parent, output = self.output, log = self.log, logURL = self.logURL
        self.process = nil; self.parent = nil; self.output = nil; self.log = nil; self.logURL = nil
        output?.fileHandleForReading.readabilityHandler = nil
        process?.terminationHandler = nil
        try? parent?.fileHandleForWriting.close()
        if let process {
            let deadline = ProcessInfo.processInfo.systemUptime + 5
            while process.isRunning && ProcessInfo.processInfo.systemUptime < deadline {
                await Task.detached { try? await Task.sleep(for: .milliseconds(50)) }.value
            }
            if process.isRunning { process.terminate() }
        }
        try? output?.fileHandleForReading.close(); try? log?.close()
        if let logURL { try? FileManager.default.removeItem(at: logURL) }
    }
}
#endif
