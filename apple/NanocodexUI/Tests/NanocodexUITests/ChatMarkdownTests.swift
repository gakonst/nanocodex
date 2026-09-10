import XCTest
@testable import NanocodexUI

final class ChatMarkdownTests: XCTestCase {
    @MainActor
    func testMarkdownWorkerKeepsMainActorAvailableAndHonorsCancellation() async throws {
        let source = String(repeating: "## Heading\n\nA **bold** paragraph with [a link](https://example.com).\n\n", count: 1500)
        let parser = ChatMarkdownParser()
        let task = Task { try await parser.blocks(for: source) }
        let responsive = expectation(description: "Main queue remains available")
        DispatchQueue.main.async { responsive.fulfill() }
        await fulfillment(of: [responsive], timeout: 1)
        let blocks = try await task.value
        XCTAssertEqual(blocks.count, 3000)
        let cached = try await parser.blocks(for: source)
        XCTAssertEqual(cached.map(\.id), blocks.map(\.id))
        // Cancellation must still win when a revisited message is cached.
        let cancelled = Task { try await parser.blocks(for: source) }
        cancelled.cancel()
        do { _ = try await cancelled.value; XCTFail("Cancelled parse must not publish") }
        catch is CancellationError { }
    }

    @MainActor
    func testContinuousStreamingPublishesBeforeTheStreamEnds() async throws {
        let renderer = ChatMarkdownRenderer()
        defer { renderer.cancel() }
        var source = "# Streaming\n\n", advancedDuringStream = false
        for index in 0..<60 {
            source += "word "; renderer.update(source)
            try await Task.sleep(for: .milliseconds(5))
            if index > 5, index < 59, (renderer.rendered?.source.count ?? 0) > 30 { advancedDuringStream = true }
        }
        XCTAssertTrue(advancedDuringStream, "Frequent deltas must not indefinitely postpone visible progress")
        let deadline = Date().addingTimeInterval(2)
        while renderer.rendered?.source != source, Date() < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertEqual(renderer.rendered?.source, source)
        renderer.update("replacement"); renderer.cancel(); renderer.update("new conversation")
        let replacementDeadline = Date().addingTimeInterval(2)
        while renderer.rendered?.source != "new conversation", Date() < replacementDeadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertEqual(renderer.rendered?.source, "new conversation")
    }

    func testCodePreservesIndentationAndLiteralMarkdown() {
        let blocks = ChatMarkdownBlock.parse("Before\n\n```swift\n    let marker = \"**literal**\"\n```\n\nAfter")
        XCTAssertEqual(blocks.count, 3)
        guard case .code("swift") = blocks[1].kind else { return XCTFail("Expected a code block") }
        XCTAssertEqual(String(blocks[1].text.characters), "    let marker = \"**literal**\"\n")
        XCTAssertEqual(String(blocks[2].text.characters), "After")
    }

    func testInlineRunsStayTogetherAndKeepLinks() {
        let blocks = ChatMarkdownBlock.parse("# A **heading**\n\nRead [the docs](https://example.com/docs).")
        XCTAssertEqual(blocks.count, 2)
        guard case .text(heading: 1, marker: nil, quote: false) = blocks[0].kind else { return XCTFail("Expected a heading") }
        XCTAssertEqual(String(blocks[0].text.characters), "A heading")
        XCTAssertEqual(String(blocks[1].text.characters), "Read the docs.")
        XCTAssertEqual(blocks[1].text.runs.compactMap(\.link).first?.absoluteString, "https://example.com/docs")
    }

    func testListsQuotesAndTablesRetainTheirStructure() {
        let blocks = ChatMarkdownBlock.parse("- First\n- Second\n\n> A quote\n\n| Name | Value |\n| --- | --- |\n| **A** | `1` |")
        XCTAssertEqual(blocks.count, 4)
        guard case .text(heading: 0, marker: "•", quote: false) = blocks[0].kind,
              case .text(heading: 0, marker: nil, quote: true) = blocks[2].kind,
              case .table(let rows) = blocks[3].kind else { return XCTFail("Expected structured blocks") }
        XCTAssertEqual(rows.map { $0.map { String($0.characters) } }, [["Name", "Value"], ["A", "1"]])
    }

    func testUnclosedStreamingFenceRemainsCode() {
        let blocks = ChatMarkdownBlock.parse("Working\n\n```js\nconst value =")
        XCTAssertEqual(blocks.count, 2)
        guard case .code("js") = blocks[1].kind else { return XCTFail("Expected streamed code") }
        XCTAssertTrue(String(blocks[1].text.characters).contains("const value ="))
    }

    func testSyntaxHighlightingPreservesCodeAndAdaptsToAppearance() async {
        let source = "\n    let greeting = \"Hello 👋 <world> **literal**\"\n\n"
        let light = await ChatCodeHighlighter.highlight(source, language: "swift", dark: false)
        let dark = await ChatCodeHighlighter.highlight(source, language: "swift", dark: true)
        XCTAssertEqual(String(light.characters), source)
        XCTAssertEqual(String(dark.characters), source)
        XCTAssertGreaterThan(light.runs.count, 2)
        XCTAssertNotEqual(light, dark)
    }

    func testStreamingCodeAndUnsupportedLanguagesKeepLiteralContent() async {
        let partial = "\tconst value = \"unfinished"
        let highlighted = await ChatCodeHighlighter.highlight(partial, language: "js", dark: false)
        XCTAssertEqual(String(highlighted.characters), partial)
        XCTAssertGreaterThan(highlighted.runs.count, 1)
        let unknown = await ChatCodeHighlighter.highlight(partial, language: "not-a-code-language", dark: false)
        XCTAssertEqual(String(unknown.characters), partial)
        let empty = await ChatCodeHighlighter.highlight("\n\t ", language: "swift", dark: false)
        XCTAssertEqual(String(empty.characters), "\n\t ")
    }

    func testRevisitedCodeKeepsSourceLanguageAndAppearanceIndependent() async {
        let source = "let value = 7\n"
        let light = await ChatCodeHighlighter.highlight(source, language: "swift", dark: false)
        let dark = await ChatCodeHighlighter.highlight(source, language: "swift", dark: true)
        let unknown = await ChatCodeHighlighter.highlight(source, language: "not-a-code-language", dark: false)
        let longer = await ChatCodeHighlighter.highlight(source + "let other = 8\n", language: "swift", dark: false)
        let revisited = await ChatCodeHighlighter.highlight(source, language: "SWIFT", dark: false)
        XCTAssertEqual(revisited, light)
        XCTAssertNotEqual(revisited, dark)
        XCTAssertEqual(unknown, AttributedString(source))
        XCTAssertEqual(String(longer.characters), source + "let other = 8\n")
        XCTAssertEqual(String(revisited.characters), source)
    }
}
