import XCTest
@testable import NanocodexUI

final class ChatCodeViewportTests: XCTestCase {
    func testOffscreenFenceUsesVerticalChatInsteadOfNestedHorizontalScroll() {
        let code = CGSize(width: 1200, height: 700)
        let horizontal = CGRect(x: 0, y: 0, width: 320, height: 700)
        let chatAboveCode = CGRect(x: 0, y: -1600, width: 360, height: 800)
        XCTAssertFalse(ChatCodeViewport.isNear(size: code, vertical: chatAboveCode, horizontal: horizontal))
        let chatApproachingCode = CGRect(x: 0, y: -900, width: 360, height: 800)
        XCTAssertTrue(ChatCodeViewport.isNear(size: code, vertical: chatApproachingCode, horizontal: horizontal))
    }

    func testVeryTallCodeStillHighlightsWhenOnlyTinyFractionIsVisible() {
        let code = CGSize(width: 320, height: 1_000_000)
        XCTAssertTrue(ChatCodeViewport.isNear(size: code,
            vertical: CGRect(x: 0, y: 500_000, width: 360, height: 800), horizontal: nil))
        XCTAssertFalse(ChatCodeViewport.isNear(size: code,
            vertical: CGRect(x: 0, y: 1_001_000, width: 360, height: 800), horizontal: nil))
    }

    func testStandaloneCodeAndHorizontalOnlyConsumersRemainSupported() {
        let code = CGSize(width: 1200, height: 700)
        XCTAssertTrue(ChatCodeViewport.isNear(size: code, vertical: nil, horizontal: nil))
        XCTAssertTrue(ChatCodeViewport.isNear(size: code, vertical: nil,
            horizontal: CGRect(x: 900, y: 0, width: 320, height: 700)))
        XCTAssertFalse(ChatCodeViewport.isNear(size: code, vertical: nil,
            horizontal: CGRect(x: 1600, y: 0, width: 320, height: 700)))
    }

    func testUnmeasuredGeometryDoesNotLaunchColdWork() {
        XCTAssertFalse(ChatCodeViewport.isNear(size: .zero, vertical: nil, horizontal: nil))
        XCTAssertFalse(ChatCodeViewport.isNear(size: CGSize(width: 320, height: 700), vertical: .zero, horizontal: nil))
    }
}

#if os(macOS)
import AppKit
import SwiftUI

@MainActor
private final class CodeScrollFixtureState: ObservableObject {
    @Published var showLast = false
}

@MainActor
private struct CodeScrollFixture: View {
    @ObservedObject var state: CodeScrollFixtureState
    let first: String
    let last: String

    var body: some View {
        ScrollViewReader { reader in
            ScrollView {
                VStack(spacing: 0) {
                    ChatCodeText(source: first, language: "swift")
                    Color.clear.frame(height: 5000)
                    // Markdown uses this same nesting; the horizontal viewport
                    // must not make vertically offscreen code eligible.
                    ScrollView(.horizontal) {
                        ChatCodeText(source: last, language: "swift")
                            .fixedSize(horizontal: true, vertical: true)
                    }.id("last")
                }
            }
            .onChange(of: state.showLast) { _, showLast in
                if showLast { reader.scrollTo("last", anchor: .bottom) }
            }
        }
        .font(.system(size: 14, design: .monospaced))
        .environment(\.colorScheme, .light)
    }
}

extension ChatCodeViewportTests {
    @MainActor
    func testHostedOffscreenCodeWaitsForVerticalViewport() async throws {
        let marker = UUID().uuidString
        let first = "let first = \"\(marker)\"\n"
        let last = "let last = \"\(marker)\"\n"
        let state = CodeScrollFixtureState()
        let window = NSWindow(contentRect: CGRect(x: -4000, y: -4000, width: 400, height: 400),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = NSHostingView(rootView: CodeScrollFixture(state: state, first: first, last: last))
        window.orderBack(nil)
        defer { window.close() }
        window.contentView?.layoutSubtreeIfNeeded()
        let initialDeadline = Date().addingTimeInterval(5)
        while ChatCodeHighlighter.cachedText(first, language: "swift", dark: false) == nil,
              Date() < initialDeadline {
            try await Task.sleep(for: .milliseconds(20))
        }
        XCTAssertNotNil(ChatCodeHighlighter.cachedText(first, language: "swift", dark: false))
        // Allow any eagerly launched import to finish before checking the
        // offscreen sentinel; it is only one line, just like the visible code.
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertNil(ChatCodeHighlighter.cachedText(last, language: "swift", dark: false))
        state.showLast = true
        let scrollDeadline = Date().addingTimeInterval(5)
        while ChatCodeHighlighter.cachedText(last, language: "swift", dark: false) == nil,
              Date() < scrollDeadline {
            try await Task.sleep(for: .milliseconds(20))
        }
        XCTAssertEqual(ChatCodeHighlighter.cachedText(last, language: "swift", dark: false)
            .map { String($0.characters) }, last)
    }
}
#endif
