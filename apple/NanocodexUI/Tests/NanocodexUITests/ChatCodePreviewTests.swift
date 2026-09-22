import XCTest
@testable import NanocodexUI

final class ChatCodePreviewTests: XCTestCase {
    func testShortMultilineCommandIsUnchanged() {
        let source = "printf 'ready'\n  swift test\nexit 7\n"
        let preview = ChatCodePreview(source)
        XCTAssertEqual(preview.text, source)
        XCTAssertFalse(preview.isTruncated)
    }

    func testEncodedPayloadCannotMakeThePreviewGrowWithTheCommand() {
        let source = "python3 - <<'PY'\nimage = '" + String(repeating: "aGVsbG8=", count: 25_000) + "'\nPY"
        let preview = ChatCodePreview(source)
        XCTAssertEqual(preview.text.count, 512)
        XCTAssertTrue(source.hasPrefix(preview.text))
        XCTAssertTrue(preview.isTruncated)
        XCTAssertTrue(source.hasSuffix("'\nPY"), "The complete input is retained independently of its preview")
    }

    func testNewlinesCannotCreateAScreenfulOfEmptyPreview() {
        let source = "echo ready\n" + String(repeating: "\n", count: 20_000) + "echo done"
        let preview = ChatCodePreview(source)
        XCTAssertLessThanOrEqual(preview.text.filter { $0 == "\n" }.count, 7)
        XCTAssertTrue(preview.isTruncated)
    }

    func testCRLFAndUnicodeLineBreaksAreBoundedWithoutRewritingShortSource() {
        let short = "echo one\r\necho two\r\n"
        XCTAssertEqual(ChatCodePreview(short).text, short)
        for newline in ["\r\n", "\r", "\u{2028}"] {
            let preview = ChatCodePreview(String(repeating: newline, count: 200))
            XCTAssertTrue(preview.isTruncated)
            XCTAssertEqual(preview.text.count, 7)
        }
    }

    func testLargeSourceBypassesHighlightingWithoutLosingText() async {
        let source = "echo '" + String(repeating: "YWJjZA==", count: 20_000) + "'\n"
        let rendered = await ChatCodeHighlighter.highlight(source, language: "bash", dark: false)
        XCTAssertEqual(rendered, AttributedString(source))
    }
}
