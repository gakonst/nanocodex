import XCTest
import SwiftUI
@testable import NanocodexUI

final class ChatMarkdownTests: XCTestCase {
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

    func testUnclosedStreamingFenceRemainsCode() {
        let blocks = ChatMarkdownBlock.parse("Working\n\n```js\nconst value =")
        XCTAssertEqual(blocks.count, 2)
        guard case .code("js") = blocks[1].kind else { return XCTFail("Expected streamed code") }
        XCTAssertTrue(String(blocks[1].text.characters).contains("const value ="))
    }

    @MainActor
    func testPhoneTableWrapsLongCellsAndConstrainsOverflow() throws {
        func renderedHeight(_ columns: Int, _ textSize: CGFloat, _ long: Bool) throws -> Int {
            let cell = AttributedString(long ? String(repeating: "Readable table content ", count: 8) : "Short")
            let rows = [Array(repeating: AttributedString("Header"), count: columns), Array(repeating: cell, count: columns)]
            let renderer = ImageRenderer(content: ChatMarkdownTable(rows: rows, textSize: textSize).frame(width: 343))
            let image = try XCTUnwrap(renderer.cgImage)
            XCTAssertEqual(image.width, 343, "Overflow must remain inside the phone's message width")
            return image.height
        }
        let short = try renderedHeight(2, 17, false)
        let wrapped = try renderedHeight(2, 17, true)
        XCTAssertGreaterThan(wrapped, short + 100, "Long cells must wrap into multiple lines")
        let wide = try renderedHeight(3, 17, true)
        XCTAssertGreaterThan(wide, wrapped, "Overflow tables include a visible scrolling hint")
        let scaled = try renderedHeight(2, 28, true)
        XCTAssertGreaterThan(scaled, wrapped, "Larger text must grow vertically without clipping")
    }

    @MainActor
    func testLinksRemainVisiblyBlueUnderMonochromeInboxTint() throws {
        let parsed = ChatMarkdownBlock.parse("A complete sentence. [Their explanation](https://example.com/source)")[0].text
        for scheme in [ColorScheme.light, .dark] {
            let renderer = ImageRenderer(content:
                Text(ChatMarkdownInline.style(parsed, textSize: 17))
                    .font(.system(size: 17)).foregroundStyle(.primary).tint(.primary)
                    .padding().background(scheme == .light ? Color.white : Color.black)
                    .environment(\.colorScheme, scheme)
            )
            let image = try XCTUnwrap(renderer.cgImage)
            var pixels = [UInt8](repeating: 0, count: image.width * image.height * 4)
            let context = try XCTUnwrap(CGContext(data: &pixels, width: image.width, height: image.height,
                bitsPerComponent: 8, bytesPerRow: image.width * 4,
                space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            let bluePixels = stride(from: 0, to: pixels.count, by: 4).filter {
                Int(pixels[$0 + 2]) > Int(pixels[$0]) + 40 && pixels[$0 + 2] > 100
            }
            XCTAssertGreaterThan(bluePixels.count, 30, "Links must be visibly distinct in \(scheme) mode")
        }
    }

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

    func testReturningCodeHasColoredTextAvailableBeforeAnAsyncTask() async {
        // A fresh source makes this a cold request even when tests share a cache.
        let source = "\n\tconst cacheTest = \"" + UUID().uuidString + "\";\n"
            + String(repeating: "    console.log(cacheTest); // preserve every line\n", count: 200)
            + "\n  "
        XCTAssertNil(ChatCodeHighlighter.cachedText(source, language: "javascript", dark: false))
        let rendered = await ChatCodeHighlighter.highlight(source, language: "javascript", dark: false)
        let firstLayout = ChatCodeHighlighter.cachedText(source, language: "JAVASCRIPT extra-fence-hint", dark: false)
        XCTAssertEqual(firstLayout, rendered)
        XCTAssertEqual(firstLayout.map { String($0.characters) }, source)
        XCTAssertGreaterThan(rendered.runs.count, 200)
        XCTAssertNil(ChatCodeHighlighter.cachedText(source, language: "javascript", dark: true))
        XCTAssertNil(ChatCodeHighlighter.cachedText(source + " ", language: "javascript", dark: false))
        XCTAssertNil(ChatCodeHighlighter.cachedText(source, language: "bash", dark: false))
    }
}
