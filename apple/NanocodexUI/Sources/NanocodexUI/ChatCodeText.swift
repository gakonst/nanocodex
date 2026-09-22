import HighlightSwift
import SwiftUI
import os

private let codePerformanceLog = OSLog(subsystem: "xyz.paradigm.centaur", category: .pointsOfInterest)

public struct ChatCodeText: View {
    let source: String
    let language: String
    @Environment(\.colorScheme) private var colorScheme
    public init(source: String, language: String) {
        self.source = source
        self.language = language
    }

    public var body: some View {
        ChatCodeContent(source: source, language: language, dark: colorScheme == .dark)
            .equatable()
    }
}

/// A returning row must use its cached colors on the first layout, instead of
/// laying out plain text and publishing the same cached result a task later.
private struct ChatCodeContent: View, Equatable {
    let source: String
    let language: String
    let dark: Bool
    @State private var nearViewport = false
    @State private var highlighted: (request: Request, text: AttributedString)?

    private struct Request: Hashable {
        let source: String
        let language: String
        let dark: Bool
    }

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.source == rhs.source && lhs.language == rhs.language && lhs.dark == rhs.dark
    }

    var body: some View {
        let request = Request(source: source, language: language, dark: dark)
        let current = highlighted?.request == request ? highlighted?.text : nil
        let cached = current ?? ChatCodeHighlighter.cachedText(source, language: language, dark: dark)
        Text(cached ?? AttributedString(source))
            .onGeometryChange(for: Bool.self) { geometry in
                ChatCodeViewport.isNear(size: geometry.size,
                    vertical: geometry.bounds(of: .scrollView(axis: .vertical)),
                    horizontal: geometry.bounds(of: .scrollView(axis: .horizontal)))
            } action: { nearViewport = $0 }
            .task(id: nearViewport ? request : nil) {
                // The body already rendered a cache hit. Publishing it again
                // would needlessly invalidate this potentially large Text.
                // Eager transcript layout must not launch HTML imports for
                // every offscreen code block. Keep exact plain text until near.
                guard nearViewport, cached == nil else { return }
                let text = await ChatCodeHighlighter.highlight(source, language: language, dark: dark)
                guard !Task.isCancelled else { return }
                highlighted = (request, text)
            }
    }
}

/// bounds(of:) supplies viewport rectangles in the code view's local space.
/// Prefer the vertical chat viewport over a nested horizontal code scroller.
/// Test intersection, not percent visibility: code may be many screens tall.
enum ChatCodeViewport {
    static func isNear(size: CGSize, vertical: CGRect?, horizontal: CGRect?) -> Bool {
        guard size.width > 0, size.height > 0 else { return false }
        guard let viewport = vertical ?? horizontal else { return true }
        guard viewport.width > 0, viewport.height > 0 else { return false }
        let margin = vertical == nil ? CGSize(width: 240, height: 0) : CGSize(width: 0, height: 240)
        return CGRect(origin: .zero, size: size).intersects(
            viewport.insetBy(dx: -margin.width, dy: -margin.height))
    }
}

enum ChatCodeHighlighter {
    private static let engine = Highlight()
    private final class Rendered {
        let text: AttributedString
        init(_ text: AttributedString) { self.text = text }
    }
    private static let cache: NSCache<NSString, Rendered> = {
        let cache = NSCache<NSString, Rendered>()
        cache.countLimit = 64
        cache.totalCostLimit = 8 * 1024 * 1024
        return cache
    }()

    private static func key(_ source: String, language: String, dark: Bool) -> NSString {
        let alias = language.split(whereSeparator: \.isWhitespace).first.map(String.init)?.lowercased() ?? ""
        // Length-prefix arbitrary fence hints; appearance changes rendered colors.
        return "\(dark):\(alias.utf8.count):\(alias)\(source)" as NSString
    }

    static func cachedText(_ source: String, language: String, dark: Bool) -> AttributedString? {
        cache.object(forKey: key(source, language: language, dark: dark))?.text
    }

    static func highlight(_ source: String, language: String, dark: Bool) async -> AttributedString {
        // Check before allocating/scanning the full source on a revisited row.
        if !Task.isCancelled, let cached = cachedText(source, language: language, dark: dark) { return cached }
        let plain = AttributedString(source)
        let trimmed = source.trimmingCharacters(in: .whitespacesAndNewlines)
        // Large tool payloads (including encoded images) are not syntax documents.
        // Keep their exact text without sending them through the HTML highlighter.
        guard source.utf8.count <= 16_384, !trimmed.isEmpty, !Task.isCancelled else { return plain }
        let alias = language.split(whereSeparator: \.isWhitespace).first.map(String.init)?.lowercased() ?? ""
        let key = key(source, language: language, dark: dark)
        let mode: HighlightMode = alias.isEmpty ? .automatic : .languageAliasIgnoreIllegal(alias)
        let signpost = OSSignpostID(log: codePerformanceLog)
        os_signpost(.begin, log: codePerformanceLog, name: "ChatCodeHighlight", signpostID: signpost, "bytes=%d", source.utf8.count)
        defer { os_signpost(.end, log: codePerformanceLog, name: "ChatCodeHighlight", signpostID: signpost) }
        guard let result = try? await engine.request(source, mode: mode, colors: dark ? .dark(.github) : .light(.github)) else { return plain }

        // The highlighter's HTML bridge trims fence whitespace. Keep the exact
        // original code, including indentation and streamed trailing newlines.
        let rendered = String(result.attributedText.characters)
        guard rendered.trimmingCharacters(in: .whitespacesAndNewlines) == trimmed,
              let originalRange = source.range(of: trimmed),
              let renderedRange = result.attributedText.range(of: trimmed) else { return plain }
        var text = AttributedString(String(source[..<originalRange.lowerBound]))
        text.append(AttributedString(result.attributedText[renderedRange]))
        text.append(AttributedString(String(source[originalRange.upperBound...])))
        if !Task.isCancelled, source.utf8.count <= 1_000_000 {
            let cost = key.length * 4 + text.runs.count * 128
            if cost <= cache.totalCostLimit { cache.setObject(Rendered(text), forKey: key, cost: cost) }
        }
        return text
    }
}
