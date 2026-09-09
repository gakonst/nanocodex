import SwiftUI
import os

private let markdownPerformanceLog = OSLog(subsystem: "xyz.paradigm.centaur", category: .pointsOfInterest)

/// Foundation owns Markdown parsing, including incomplete streamed replies.
/// This view supplies the block layout that SwiftUI Text does not render.
public struct ChatMarkdown: View {
    private let text: String

    public init(text: String) { self.text = text }

    public var body: some View {
        ChatMarkdownContent(text: text).equatable()
    }
}

/// Keystrokes, scroll geometry, and other rows' streamed updates must not
/// reparse unchanged messages. Environment changes still update this view.
private struct ChatMarkdownContent: View, Equatable {
    let text: String
    @StateObject private var renderer = ChatMarkdownRenderer()
    #if os(macOS)
    @ScaledMetric(relativeTo: .body) private var textSize = 16
    #else
    @ScaledMetric(relativeTo: .body) private var textSize = 17
    #endif

    static func == (lhs: Self, rhs: Self) -> Bool { lhs.text == rhs.text }

    var body: some View {
        Group {
            if let rendered = renderer.rendered, rendered.source == text || text.hasPrefix(rendered.source) {
                content(rendered.blocks)
            } else {
                Text(text).lineSpacing(5).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .font(.system(size: textSize))
        .task(id: text) { renderer.update(text) }
        .onDisappear { renderer.cancel() }
    }

    private func content(_ blocks: [ChatMarkdownBlock]) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(blocks) { block in
                switch block.kind {
                case .code(let language): code(block.text, language: language)
                case .table(let rows): table(rows)
                case .text(let heading, let marker, let quote):
                    HStack(alignment: .top, spacing: 10) {
                        if quote { Rectangle().fill(.secondary.opacity(0.3)).frame(width: 3) }
                        if let marker { Text(marker).foregroundStyle(.secondary).frame(minWidth: 14, alignment: .trailing) }
                        Text(inline(block.text))
                            .font(.system(size: heading > 0 ? textSize + (heading == 1 ? 8 : heading == 2 ? 4 : 2) : textSize, weight: heading > 0 ? .semibold : .regular))
                            .lineSpacing(5)
                            .foregroundStyle(quote ? Color.secondary : .primary)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .accessibilityAddTraits(heading > 0 ? .isHeader : [])
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, heading > 0 && block.id != blocks.first?.id ? 10 : 0)
                }
            }
        }
        .font(.system(size: textSize))
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func inline(_ value: AttributedString) -> AttributedString {
        var styled = value
        for run in value.runs where run.inlinePresentationIntent?.contains(.code) == true {
            styled[run.range].font = .system(size: textSize - 2, design: .monospaced)
            styled[run.range].backgroundColor = Color.primary.opacity(0.06)
        }
        return styled
    }

    private func code(_ value: AttributedString, language: String) -> some View {
        let source = String(value.characters)
        return VStack(spacing: 0) {
            HStack {
                Text(language.isEmpty ? "Code" : language).font(.system(size: 12)).foregroundStyle(.secondary)
                Spacer()
                ChatCopyButton(text: source, label: "Copy code", showsLabel: true)
            }.padding(.leading, 16).padding(.trailing, 6).padding(.vertical, 3)
            Divider().opacity(0.35)
            ScrollView(.horizontal) {
                ChatCodeText(source: source, language: language)
                    .font(.system(size: textSize - 3, design: .monospaced)).lineSpacing(4)
                    .textSelection(.enabled).fixedSize(horizontal: true, vertical: true)
                    .padding(16).frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .background(ChatPalette.userBubble, in: RoundedRectangle(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Color.primary.opacity(0.06)))
    }

    private func table(_ rows: [[AttributedString]]) -> some View {
        ScrollView(.horizontal) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                ForEach(Array(rows.enumerated()), id: \.offset) { rowIndex, cells in
                    GridRow {
                        ForEach(Array(cells.enumerated()), id: \.offset) { _, cell in
                            Text(inline(cell)).font(.system(size: textSize, weight: rowIndex == 0 ? .semibold : .regular))
                                .textSelection(.enabled).padding(.horizontal, 14).padding(.vertical, 10)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(rowIndex == 0 ? ChatPalette.userBubble : .clear)
                                .overlay(alignment: .bottom) { Divider().opacity(0.5) }
                        }
                    }
                }
            }
        }.clipShape(RoundedRectangle(cornerRadius: 10))
    }
}

/// At most one parse per visible message is in flight. New deltas replace the
/// queued source, rather than repeatedly cancelling work and starving a stream.
@MainActor
final class ChatMarkdownRenderer: ObservableObject {
    @Published private(set) var rendered: (source: String, blocks: [ChatMarkdownBlock])?
    private var latest = ""
    private var task: Task<Void, Never>?
    private var generation = 0
    func update(_ text: String) {
        latest = text
        guard task == nil, rendered?.source != text else { return }
        let generation = generation
        task = Task { [weak self] in
            guard let self else { return }
            defer { if self.generation == generation { self.task = nil } }
            do {
                if self.rendered != nil { try await Task.sleep(for: .milliseconds(32)) }
                while !Task.isCancelled {
                    let source = self.latest
                    let blocks = try await ChatMarkdownParser.shared.blocks(for: source)
                    try Task.checkCancellation()
                    // A parsed prefix is useful while a reply is streaming; a
                    // replaced/corrected message must not show obsolete content.
                    if self.latest == source || self.latest.hasPrefix(source) { self.rendered = (source, blocks) }
                    if self.latest == source { return }
                    try await Task.sleep(for: .milliseconds(32))
                }
            } catch { }
        }
    }
    func cancel() { generation += 1; task?.cancel(); task = nil }
}

/// Parsing runs on this actor's executor, with a bounded cache for revisited
/// messages. Theme and Dynamic Type styling remain in the SwiftUI renderer.
actor ChatMarkdownParser {
    static let shared = ChatMarkdownParser()
    private var cache: [String: [ChatMarkdownBlock]] = [:]
    private var recent: [String] = []
    private var bytes = 0
    func blocks(for text: String) throws -> [ChatMarkdownBlock] {
        assert(!Thread.isMainThread)
        try Task.checkCancellation()
        if let blocks = cache[text] { return blocks }
        let blocks = ChatMarkdownBlock.parse(text)
        try Task.checkCancellation()
        let cost = text.utf8.count
        if cost <= 1_000_000 {
            cache[text] = blocks; recent.append(text); bytes += cost
            while recent.count > 64 || bytes > 4_000_000 {
                let old = recent.removeFirst(); bytes -= old.utf8.count; cache.removeValue(forKey: old)
            }
        }
        return blocks
    }
}

struct ChatMarkdownBlock: Identifiable, Sendable {
    enum Kind: Sendable {
        case text(heading: Int, marker: String?, quote: Bool)
        case code(String)
        case table([[AttributedString]])
    }
    var id: Int
    var text: AttributedString
    var kind: Kind

    static func parse(_ source: String) -> [Self] {
        let signpost = OSSignpostID(log: markdownPerformanceLog)
        os_signpost(.begin, log: markdownPerformanceLog, name: "ChatMarkdownParse", signpostID: signpost, "bytes=%d", source.utf8.count)
        defer { os_signpost(.end, log: markdownPerformanceLog, name: "ChatMarkdownParse", signpostID: signpost) }
        guard let markdown = try? AttributedString(markdown: source, options: .init(failurePolicy: .returnPartiallyParsedIfPossible)) else {
            return [Self(id: 0, text: AttributedString(source), kind: .text(heading: 0, marker: nil, quote: false))]
        }
        var result: [Self] = []
        for run in markdown.runs {
            let value = AttributedString(markdown[run.range])
            let components = run.presentationIntent?.components ?? []
            let identity = components.first?.identity ?? 0
            var heading = 0, ordinal: Int?, unordered = false, quote = false
            var language: String?, tableID: Int?, row = 0, column = 0
            for component in components {
                switch component.kind {
                case .header(let level): heading = level
                case .listItem(let number): ordinal = number
                case .unorderedList: unordered = true
                case .blockQuote: quote = true
                case .codeBlock(let hint): language = hint ?? ""
                case .table: tableID = component.identity
                case .tableRow(let index): row = index
                case .tableCell(let index): column = index
                default: break
                }
            }
            if let tableID {
                if result.last?.id != tableID { result.append(Self(id: tableID, text: AttributedString(), kind: .table([]))) }
                if case .table(var rows) = result[result.count - 1].kind {
                    while rows.count <= row { rows.append([]) }
                    while rows[row].count <= column { rows[row].append(AttributedString()) }
                    rows[row][column].append(value)
                    result[result.count - 1].kind = .table(rows)
                }
            } else if result.last?.id == identity {
                result[result.count - 1].text.append(value)
            } else {
                result.append(Self(id: identity, text: value, kind: language.map(Kind.code) ?? .text(heading: heading, marker: ordinal.map { unordered ? "•" : "\($0)." }, quote: quote)))
            }
        }
        return result
    }
}
