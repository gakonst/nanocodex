import XCTest
import AVFoundation
import CryptoKit
@testable import InboxCore

final class VideoAttachmentTests: XCTestCase {
    private var fixture: URL { Bundle.module.url(forResource: "VideoAudioCheck", withExtension: "mp4", subdirectory: "Fixtures")! }

    private func largeFixture(in root: URL) throws -> URL {
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let source = try XCTUnwrap(Bundle.module.url(forResource: "VideoLongCheck", withExtension: "mp4", subdirectory: "Fixtures"))
        let url = root.appendingPathComponent("VideoLongCheck.mp4")
        try FileManager.default.copyItem(at: source, to: url)
        let file = try FileHandle(forWritingTo: url)
        defer { try? file.close() }
        try file.seekToEnd()
        // A valid MP4 free-space atom extends to EOF. Sparse padding crosses
        // the former source-size cap without retaining a huge test fixture.
        try file.write(contentsOf: Data([0, 0, 0, 0, 0x66, 0x72, 0x65, 0x65]))
        try file.truncate(atOffset: 101 * 1024 * 1024)
        return url
    }

    func testLongLargeVideoStoresAndReplaysWithoutSourceCaps() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try largeFixture(in: root)
        let prepared = try await VideoAttachmentPreparation.prepare(url: source)
        XCTAssertEqual(prepared.attachment.byteCount, 101 * 1024 * 1024)
        XCTAssertEqual(prepared.attachment.video?.duration ?? 0, 90, accuracy: 0.01)
        XCTAssertEqual(prepared.attachment.video?.timestamps, [], "Original videos have no sampled prompt frames")
        let restored = try JSONDecoder().decode(MessageAttachment.self, from: JSONEncoder().encode(prepared.attachment))
        let store = try AttachmentStore(scope: "large-video", rootDirectory: root)
        try store.save(prepared)
        XCTAssertEqual(try store.url(for: restored).resourceValues(forKeys: [.fileSizeKey]).fileSize, restored.byteCount)
        XCTAssertEqual(try store.content(for: [restored]), prepared.content)
        var command = AgentCommand(agentID: "large-video-test", input: "Describe this video.", kind: .followUp)
        command.images = try store.content(for: [restored])
        XCTAssertNoThrow(try command.requestSpec(), "Only the prepared request is subject to the managed service's byte limit")
    }

    func testVideoStorageReplayGroupingAndIsolation() async throws {
        let prepared = try await VideoAttachmentPreparation.prepare(url: fixture)
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let store = try AttachmentStore(scope: "video-one", rootDirectory: root)
        let other = try AttachmentStore(scope: "video-two", rootDirectory: root)
        try store.save(prepared)
        XCTAssertTrue(prepared.attachment.isVideo)
        XCTAssertEqual(prepared.attachment.video?.duration ?? 0, 3, accuracy: 0.01)
        XCTAssertEqual(prepared.attachment.video?.timestamps, [])
        XCTAssertEqual(prepared.attachment.video?.hasAudio, true)
        XCTAssertTrue(prepared.content.allSatisfy { $0["type"].string == "text" })
        let restored = try JSONDecoder().decode(MessageAttachment.self, from: JSONEncoder().encode(prepared.attachment))
        XCTAssertEqual(try store.content(for: [restored]), prepared.content)
        XCTAssertEqual(try Data(contentsOf: store.url(for: restored)), try Data(contentsOf: fixture))
        XCTAssertTrue(FileManager.default.fileExists(atPath: try store.previewURL(for: restored).path))
        XCTAssertThrowsError(try other.content(for: [restored]))
        XCTAssertThrowsError(try store.save(prepared), "Duplicate saves must not remove the existing clip")
        XCTAssertEqual(try store.content(for: [restored]), prepared.content)

        let prompt = "Describe the changes in this clip."
        let input = [.object(["type": JSON.string("text"), "text": .string(prompt)])] + prepared.content
        let event = try AgentEvent(.object(["cursor": .string("1"), "type": .string("turn_accepted"), "turn_id": .string("video-turn"), "input": .array(input)]))
        let row = try XCTUnwrap(transcript([event]).first)
        XCTAssertEqual(row.text, prompt)
        XCTAssertNil(row.images, "Video frames must stay grouped as one video")
        XCTAssertEqual(row.videos?.first?.timestamps, restored.video?.timestamps)
        XCTAssertEqual(row.videos?.first?.images.count, 0)
        XCTAssertEqual(row.videos?.first?.hasAudio, true)
        XCTAssertEqual(row.videos?.first?.path, "/brain/attachments/" + restored.id.lowercased() + "/original.mp4")
        let legacyContent = VideoAttachmentContent.make(id: restored.id, name: restored.name, duration: 3, timestamps: [0], images: [try XCTUnwrap(prepared.poster)])
        let legacy = VideoAttachmentContent.project(legacyContent)
        XCTAssertTrue(legacy.remaining.isEmpty)
        XCTAssertEqual(legacy.videos.first?.images.count, 1)
        XCTAssertNil(legacy.videos.first?.path)
        XCTAssertNil(legacy.videos.first?.hasAudio)
        let unprefixed = legacyContent.map { value in
            value["type"].string == "text" ? JSON.object(["type": .string("text"), "text": .string(value["text"].string.replacingOccurrences(of: VideoAttachmentContent.prefix, with: "[Video attachment v1]\n"))]) : value
        }
        XCTAssertEqual(VideoAttachmentContent.project(unprefixed).videos, legacy.videos)
        let reloaded = try JSONDecoder().decode(TranscriptRow.self, from: JSONEncoder().encode(row))
        XCTAssertEqual(reloaded, row)
        var command = AgentCommand(agentID: "video-test", input: prompt, kind: .followUp)
        command.images = try store.content(for: Array(repeating: restored, count: 4))
        XCTAssertNoThrow(try command.requestSpec(), "Four prepared clips fit the existing request boundary")
        try store.prune(keeping: [restored.id])
        XCTAssertNoThrow(try store.content(for: [restored]))
        try store.remove(restored)
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(at: root.appendingPathComponent("video-one"), includingPropertiesForKeys: nil, options: .skipsHiddenFiles).isEmpty)
    }

    func testVideoMetadataAndPayloadTamperingAreRejected() async throws {
        let prepared = try await VideoAttachmentPreparation.prepare(url: fixture)
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let store = try AttachmentStore(scope: "video", rootDirectory: root)
        try store.save(prepared)
        let metadata = VideoAttachmentInfo(duration: .infinity, timestamps: [0], promptByteCount: 20)
        XCTAssertThrowsError(try MessageAttachment(name: "video.mp4", mediaType: "video/mp4", byteCount: 1, video: metadata))
        let overflow = VideoAttachmentInfo(duration: 1e100, timestamps: [], promptByteCount: 1, original: true)
        XCTAssertThrowsError(try MessageAttachment(name: "video.mp4", mediaType: "video/mp4", byteCount: 1, video: overflow))
        XCTAssertThrowsError(try prepared.attachment.originalVideoContent(path: "/brain/attachments/other/original.mp4"))
        let bytes = try store.fileURL(for: prepared.attachment.id, extension: "json")
        try Data("[]".utf8).write(to: bytes)
        XCTAssertThrowsError(try store.content(for: [prepared.attachment]))
        let outside = root.appendingPathComponent("outside")
        try Data("preserve".utf8).write(to: outside)
        try FileManager.default.removeItem(at: bytes)
        try FileManager.default.createSymbolicLink(at: bytes, withDestinationURL: outside)
        XCTAssertThrowsError(try store.content(for: [prepared.attachment]))
        XCTAssertEqual(try String(contentsOf: outside, encoding: .utf8), "preserve")
        let malformed: [JSON] = [.object(["type": .string("text"), "text": .string("[Video attachment v1]\n{}")] )]
        XCTAssertEqual(VideoAttachmentContent.project(malformed).remaining, malformed)
    }

    func testLiveOriginalVideoReachesAgentAndSurvivesReload() async throws {
        let env = ProcessInfo.processInfo.environment
        guard env["NANOCODEX_VIDEO_LIVE"] == "1" else { throw XCTSkip("Opt in with NANOCODEX_VIDEO_LIVE=1 and NC_API_KEY") }
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try env["NANOCODEX_VIDEO_LARGE"] == "1" ? largeFixture(in: root) : fixture
        let prepared = try await VideoAttachmentPreparation.prepare(url: source)
        let credential = try AccountCredential(origin: env["NANOCODEX_MANAGED_URL"] ?? "https://nanocodex.gakonst.workers.dev", apiKey: XCTUnwrap(env["NC_API_KEY"]))
        let client = ManagedClient(credential: credential)
        defer { client.close() }
        let agent = try await client.create(requestID: UUID().uuidString)
        addTeardownBlock {
            let cleanup = ManagedClient(credential: credential)
            defer { cleanup.close() }
            _ = try await cleanup.json(path: ManagedClient.agentPath(agent), method: "DELETE")
        }
        let path = try await client.uploadAttachment(agentID: agent, attachment: prepared.attachment, source: source)
        let resumed = try await client.uploadAttachment(agentID: agent, attachment: prepared.attachment, source: source)
        XCTAssertEqual(resumed, path)
        let digest = SHA256.hash(data: try Data(contentsOf: source, options: .mappedIfSafe)).map { String(format: "%02x", $0) }.joined()
        var command = AgentCommand(agentID: agent, input: "Use tools to compute the SHA-256 and byte count of the attached original video file at its /brain path. Reply ORIGINAL_FILE_OK, the computed digest and byte count. Do not infer the bytes from the filename or metadata. For a file over 64 MiB, use a native execution Hand with /brain mounted.", kind: .followUp)
        command.images = try prepared.attachment.originalVideoContent(path: path)
        let accepted = try await client.command(command)
        let reply = try await completed(command, client: client)
        XCTAssertTrue(reply.contains("ORIGINAL_FILE_OK"), reply)
        XCTAssertTrue(reply.contains(digest), reply)
        XCTAssertTrue(reply.contains(String(prepared.attachment.byteCount)), reply)
        let replay = try await client.command(command)
        XCTAssertEqual(replay["accepted_cursor"], accepted["accepted_cursor"])
        let history = try await client.history(agent)
        let row = try XCTUnwrap(transcript(history.events).first { $0.role == "You" })
        let video = try XCTUnwrap(row.videos?.first)
        XCTAssertEqual(video.path, path)
        XCTAssertEqual(video.hasAudio, prepared.attachment.video?.hasAudio)
        let download = try await client.downloadVideo(agentID: agent, video: video)
        defer { try? FileManager.default.removeItem(at: download) }
        XCTAssertEqual(SHA256.hash(data: try Data(contentsOf: download, options: .mappedIfSafe)), SHA256.hash(data: try Data(contentsOf: source, options: .mappedIfSafe)))
        let audio = try await AVURLAsset(url: download).loadTracks(withMediaType: .audio)
        XCTAssertEqual(!audio.isEmpty, prepared.attachment.video?.hasAudio)
        XCTAssertEqual(row.text, command.input)
        let followUp = AgentCommand(agentID: agent, input: "Read the attached file again using its existing /brain path and compute its SHA-256. Reply only with the digest.", kind: .followUp)
        _ = try await client.command(followUp)
        let second = try await completed(followUp, client: client)
        XCTAssertTrue(second.contains(digest), second)
        print("Real original video: agent computed the exact file digest twice; upload retry, playback download, audio tracks and durable history passed.")
    }
    private func completed(_ command: AgentCommand, client: ManagedClient) async throws -> String {
        for _ in 0..<240 {
            let result = try await client.json(path: ManagedClient.agentPath(command.agentID) + "/turns/" + command.requestID)
            if result["state"].string == "completed" { return result["terminal"]["final_message"].string }
            if ["failed", "cancelled"].contains(result["state"].string) { XCTFail(result.pretty); throw APIError.invalidResponse }
            try await Task.sleep(for: .seconds(1))
        }
        throw APIError.invalidResponse
    }
}
