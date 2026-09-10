import XCTest
@testable import NanocodexUI

final class ChatGeneratedOutputTests: XCTestCase {
    private func json(_ value: Any) throws -> String {
        String(data: try JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed, .sortedKeys]), encoding: .utf8)!
    }

    func testActualCodeModeAndStructuredResultsPreserveDistinctContentAndDeduplicateImages() throws {
        let source = "data:image/png;base64,cGl4ZWxz"
        let code: [[String: Any]] = [
            ["type": "input_text", "text": "Script completed\nWall time 0.5 seconds\nOutput:\n"],
            ["type": "input_text", "text": "## Generated chart\n\n**Ready** to review."],
            ["type": "input_image", "image_url": source, "detail": "high"],
        ]
        let structured: [String: Any] = ["exit_code": 0, "content": [["type": "resource_link", "uri": "https://example.com/report.pdf", "name": "Report.pdf", "mimeType": "application/pdf"]]]
        let nested: [String: Any] = ["content": [["type": "image", "data": "cGl4ZWxz", "mimeType": "image/png"]]]
        let outputs = ChatGeneratedOutput.parse(results: [try json(structured), try json(try json(code)), try json(nested), try json(try json(code))], includeText: true)
        XCTAssertEqual(outputs.filter { $0.kind == .image }.count, 1)
        XCTAssertEqual(outputs.first { $0.kind == .image }?.source, source)
        XCTAssertEqual(outputs.filter { $0.kind == .text }.map(\.text), ["## Generated chart\n\n**Ready** to review."])
        XCTAssertEqual(outputs.first { $0.kind == .file }?.title, "Report.pdf")
        XCTAssertFalse(outputs.contains { $0.text.contains("exit_code") })
    }

    func testMCPEmbeddedResourcesAudioVideoAndTextBecomeUsableOutputs() throws {
        let content: [[String: Any]] = [
            ["type": "audio", "mimeType": "audio/wav", "data": "YXVkaW8="],
            ["type": "video", "url": "https://example.com/demo.mp4", "mimeType": "video/mp4"],
            ["type": "resource", "resource": ["uri": "artifact://report.csv", "mimeType": "text/csv", "blob": "YSxiCjEsMgo="]],
            ["type": "resource", "resource": ["uri": "artifact://notes.md", "mimeType": "text/markdown", "text": "# Notes\n\nKeep **both** outputs."]],
            ["type": "resource", "resource": ["uri": "artifact://page.html", "mimeType": "text/html", "text": "<script>doNotExecute()</script>"]],
        ]
        let outputs = ChatGeneratedOutput.parse(results: [try json(["content": content])], includeText: true)
        XCTAssertEqual(outputs.first { $0.kind == .audio }?.source, "data:audio/wav;base64,YXVkaW8=")
        XCTAssertEqual(outputs.filter { $0.kind == .video }.count, 1)
        XCTAssertEqual(outputs.filter { $0.kind == .file }.count, 3)
        XCTAssertTrue(outputs.contains { $0.kind == .text && $0.text.contains("# Notes") })
        XCTAssertFalse(outputs.contains { $0.kind == .text && $0.text.contains("<script>") })
        XCTAssertFalse(outputs.contains { $0.kind == .unsupported })
    }

    func testUnsafeAndLocalResourcesStayUnavailableWithoutBinaryDiagnostics() throws {
        let payload: [String: Any] = ["content": [
            ["type": "resource_link", "uri": "javascript:alert(1)", "name": "Unsafe"],
            ["type": "resource_link", "uri": "sandbox:/mnt/data/report.pdf", "name": "Workspace report"],
            ["type": "input_image", "image_url": "data:image/png;base64,DO_NOT_PRINT"],
            ["type": "resource", "resource": ["mimeType": "application/pdf", "blob": "ALSO_PRIVATE"]],
        ]]
        let encoded = try json(payload)
        let outputs = ChatGeneratedOutput.parse(results: [encoded])
        XCTAssertEqual(outputs.filter { $0.kind == .unsupported }.count, 2)
        XCTAssertFalse(outputs.contains { $0.source?.hasPrefix("javascript:") == true || $0.source?.hasPrefix("sandbox:") == true })
        let diagnostics = ChatGeneratedOutput.sanitizedText(try json(encoded))
        XCTAssertFalse(diagnostics.contains("DO_NOT_PRINT")); XCTAssertFalse(diagnostics.contains("ALSO_PRIVATE"))
    }

    func testTextPolicyAndMarkdownGeneratedAttachments() throws {
        XCTAssertTrue(ChatGeneratedOutput.parse(results: [try json("Command output")]).isEmpty)
        XCTAssertEqual(ChatGeneratedOutput.parse(results: [try json("## Result")], includeText: true).first?.text, "## Result")
        let outputs = ChatGeneratedOutput.parse(results: [try json(["type": "input_text", "text": "A chart:\n![Preview](https://example.com/chart.png)\n[Download](https://example.com/report.pdf)"])], includeText: true)
        XCTAssertTrue(outputs.contains { $0.kind == .image })
        XCTAssertTrue(outputs.contains { $0.kind == .file })
        XCTAssertTrue(outputs.contains { $0.kind == .text && $0.text.contains("A chart:") })
        XCTAssertEqual(outputs.map(\.kind), [.text, .image, .file], "Markdown assets keep their emitted order")
    }

    func testEmbeddedTextAndDownloadBothRetainFullContents() throws {
        let original = "a,b\n" + String(repeating: "1,2\n", count: 50_001)
        let encoded = try json(["type": "resource", "resource": ["uri": "file:///report.csv", "mimeType": "text/csv", "text": original]])
        let outputs = ChatGeneratedOutput.parse(results: [encoded], includeText: true)
        let file = try XCTUnwrap(outputs.first { $0.kind == .file })
        let encodedBytes = try XCTUnwrap(file.source?.split(separator: ",", maxSplits: 1).last)
        XCTAssertEqual(Data(base64Encoded: String(encodedBytes)), Data(original.utf8))
        XCTAssertEqual(file.title, "report.csv")
        XCTAssertEqual(outputs.first { $0.kind == .text }?.text, original)
        XCTAssertFalse(outputs.contains { $0.kind == .unsupported })
    }

    func testDuplicateJSONResourceInsideEmittedTextDoesNotBecomeRawJSON() throws {
        let resource = try json(["type": "resource", "resource": ["uri": "file:///report.csv", "mimeType": "text/csv", "blob": "YSxiCjEsMgo="]])
        let emitted = try json([["type": "input_text", "text": resource]])
        let outputs = ChatGeneratedOutput.parse(results: [emitted, emitted], includeText: true)
        XCTAssertEqual(outputs.map(\.kind), [.file])
        XCTAssertEqual(outputs.first?.title, "report.csv")
        XCTAssertFalse(outputs.contains { $0.text.contains("mimeType") || $0.text.contains("YSxiCjEsMgo=") })
    }

    func testToolTextStaysHiddenThroughMemoryMCPAndCodeModeEnvelopes() throws {
        let memory = try json(["operation": "read", "memories": [["key": ["id": 1, "version": 2], "content": "INTERNAL_MEMORY_RECORD"]]])
        let command = "INTERNAL_COMMAND_OUTPUT: connection diagnostics"
        let content: [[String: Any]] = [
            ["type": "text", "text": memory],
            ["type": "input_text", "text": command],
            ["type": "output_text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n" + memory],
        ]
        let payloads = [try json(memory), try json(command), try json(["content": content]),
                        try json(try json(content)), try json(["structured_result": ["content": content]]),
                        try json(["result": ["structuredContent": ["outputs": content]]])]
        for payload in payloads {
            XCTAssertTrue(ChatGeneratedOutput.parse(results: [payload]).isEmpty, "Tool text must never bypass the default attachment-only policy")
        }
        let longLog = try json(["type": "input_text", "text": String(repeating: command, count: 10_000)])
        XCTAssertTrue(ChatGeneratedOutput.parse(results: [longLog]).isEmpty, "Long diagnostics must not turn into a generated download")
    }

    func testAttachmentsRemainAvailableWithoutToolTextOrResourcePreviews() throws {
        let payload = try json(["content": [
            ["type": "input_text", "text": "INTERNAL_CAPTION\n![Chart](https://example.com/chart.png)\n[Report](https://example.com/report.pdf)"],
            ["type": "resource", "resource": ["uri": "artifact:///notes.json", "mimeType": "application/json", "text": "{\"internal_record\":true}"]],
            ["image_url": "https://example.com/second.png", "output_hint": "INTERNAL_OUTPUT_HINT"],
            ["type": "audio", "mimeType": "audio/wav", "data": "YXVkaW8="],
        ]])
        let outputs = ChatGeneratedOutput.parse(results: [payload])
        XCTAssertEqual(outputs.map(\.kind), [.image, .file, .file, .image, .audio])
        XCTAssertTrue(outputs.allSatisfy { $0.text.isEmpty })
        let file = try XCTUnwrap(outputs.first { $0.title == "notes.json" })
        let encodedBytes = try XCTUnwrap(file.source?.split(separator: ",", maxSplits: 1).last)
        XCTAssertEqual(Data(base64Encoded: String(encodedBytes)), Data("{\"internal_record\":true}".utf8))
    }
    func testAllArtifactsSurviveLargeArraysAndDeepEnvelopes() throws {
        let artifacts = (0..<256).map { ["type": "resource_link", "uri": "https://example.com/\($0).pdf"] }
        var payload: Any = ["content": Array(repeating: ["ignored": true] as [String: Any], count: 4096) + artifacts]
        for _ in 0..<64 { payload = ["result": payload] }
        let outputs = ChatGeneratedOutput.parse(results: [try json(payload)])
        XCTAssertEqual(outputs.count, artifacts.count)
        XCTAssertEqual(outputs.map(\.source), artifacts.map { $0["uri"] })
    }

    func testSanitizationRetainsEveryDiagnosticBeyondOldDepthArrayAndTextLimits() throws {
        let text = "BEGIN\n" + String(repeating: "line\n", count: 50_000) + "END"
        XCTAssertEqual(ChatGeneratedOutput.sanitizedText(text), text)
        var payload: Any = ["content": (0..<256).map { ["number": $0, "text": "row-\($0)"] }]
        for _ in 0..<64 { payload = ["result": payload] }
        let sanitized = ChatGeneratedOutput.sanitizedText(try json(payload))
        XCTAssertTrue(sanitized.contains("row-255"))
        XCTAssertFalse(sanitized.contains("omitted"))
    }

    func testLargeEmbeddedAssetWritesAllBytesToFileAndCanBeReopened() async throws {
        let original = Data(repeating: 0x61, count: 17 * 1024 * 1024 + 3)
        let source = "data:application/octet-stream;base64," + original.base64EncodedString()
        let output = try XCTUnwrap(ChatGeneratedOutput.parse(results: [try json([
            "type": "file", "url": source, "name": "Large output.bin", "mimeType": "application/octet-stream"
        ])]).first)
        XCTAssertEqual(output.kind, .file)
        let file = try await GeneratedAsset.playableURL(output)
        defer { try? FileManager.default.removeItem(at: file.deletingLastPathComponent()) }
        XCTAssertTrue(file.isFileURL)
        XCTAssertEqual(try Data(contentsOf: file, options: .mappedIfSafe), original)
        let reopened = try await GeneratedAsset.playableURL(output)
        XCTAssertEqual(reopened, file)
    }

    func testMalformedBase64NeverPublishesPartialFile() async throws {
        let source = "data:application/octet-stream;base64," + String(repeating: "YWFh", count: 16_384) + "YQ==MORE"
        let output = try XCTUnwrap(ChatGeneratedOutput.parse(results: [try json([
            "type": "file", "url": source, "mimeType": "application/octet-stream"
        ])]).first)
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("CentaurGeneratedOutputs").appendingPathComponent(output.id)
        defer { try? FileManager.default.removeItem(at: folder) }
        do { _ = try await GeneratedAsset.playableURL(output); XCTFail("Invalid base64 must fail") }
        catch { XCTAssertTrue(try FileManager.default.contentsOfDirectory(atPath: folder.path).isEmpty) }
    }

}
