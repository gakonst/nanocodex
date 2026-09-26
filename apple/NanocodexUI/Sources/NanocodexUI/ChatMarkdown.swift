import SwiftUI
import os

private let markdownPerformanceLog = OSLog(subsystem: "xyz.paradigm.centaur", category: .pointsOfInterest)

/// Foundation owns Markdown parsing, including incomplete streamed replies.
/// This view supplies the block layout that SwiftUI Text does not render.
public struct ChatMarkdown: View {
    private let text: String
    private let compact: Bool

    public init(text: String, compact: Bool = false) { self.text = text; self.compact = compact }

    public var body: some View {
        ChatMarkdownContent(text: text, compact: compact).equatable()
    }
}

/// Keystrokes, scroll geometry, and other rows' streamed updates must not
/// reparse unchanged messages. Environment changes still update this view.
private struct ChatMarkdownContent: View, Equatable {
    let text: String
    let compact: Bool
    @StateObject private var renderer = ChatMarkdownRenderer()
    #if os(macOS)
    @ScaledMetric(relativeTo: .body) private var textSize = 16
    #else
    @ScaledMetric(relativeTo: .body) private var textSize = 17
    #endif

    static func == (lhs: Self, rhs: Self) -> Bool { lhs.text == rhs.text && lhs.compact == rhs.compact }

    var body: some View {
        Group {
            if let rendered = renderer.rendered, let last = rendered.blocks.last,
               let pending = ChatMarkdownLiveTail.pending(source: rendered.source, latest: text, last: last) {
                content(rendered.blocks, pending: pending)
            } else {
                Text(text).lineSpacing(compact ? 3 : 5).textSelection(.enabled)
                    .frame(maxWidth: compact ? nil : .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .font(.system(size: textSize))
        .task(id: text) { renderer.update(text) }
        .onDisappear { renderer.cancel() }
    }

    private func content(_ blocks: [ChatMarkdownBlock], pending: ChatMarkdownLiveTail.Pending) -> some View {
        VStack(alignment: .leading, spacing: compact ? 10 : 14) {
            ForEach(blocks) { block in
                switch block.kind {
                case .code(let language):
                    code(block.text, language: language, pending: block.id == blocks.last?.id ? pending.inline : "")
                case .table(let rows):
                    ChatMarkdownTable(rows: rows, textSize: textSize)
                case .text(let heading, let marker, let quote):
                    HStack(alignment: .top, spacing: 10) {
                        if quote { Rectangle().fill(.secondary.opacity(0.3)).frame(width: 3) }
                        if let marker { Text(marker).foregroundStyle(.secondary).frame(minWidth: 14, alignment: .trailing) }
                        Text(inlineText(block.text, pending: block.id == blocks.last?.id ? pending.inline : ""))
                            .font(.system(size: heading > 0 ? textSize + (heading == 1 ? 8 : heading == 2 ? 4 : 2) : textSize, weight: heading > 0 ? .semibold : .regular))
                            .lineSpacing(compact ? 3 : 5)
                            .foregroundStyle(quote ? Color.secondary : .primary)
                            .textSelection(.enabled)
                            .frame(maxWidth: compact ? nil : .infinity, alignment: .leading)
                            .accessibilityAddTraits(heading > 0 ? .isHeader : [])
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, heading > 0 && block.id != blocks.first?.id ? 10 : 0)
                }
            }
            // Tables and closed code fences cannot absorb new prose. Keep the
            // unparsed bytes visible below the existing block.
            if !pending.following.isEmpty {
                Text(pending.following).lineSpacing(compact ? 3 : 5).textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .font(.system(size: textSize))
        .frame(maxWidth: compact ? nil : .infinity, alignment: .leading)
    }

    private func inlineText(_ value: AttributedString, pending: String) -> AttributedString {
        var styled = ChatMarkdownInline.style(value, textSize: textSize)
        styled.append(AttributedString(pending))
        return styled
    }

    private func code(_ value: AttributedString, language: String, pending: String) -> some View {
        let source = String(value.characters) + pending
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

}

/// A background parse may trail a live reply. Preserve every newly arrived
/// character without replacing already measured Markdown blocks with raw text.
/// Foundation Markdown strips trailing spaces/newlines from prose blocks, so
/// restore those separators before appending the unparsed continuation.
enum ChatMarkdownLiveTail {
    struct Pending {
        let inline: String
        let following: String
    }

    static func pending(source: String, latest: String, last: ChatMarkdownBlock) -> Pending? {
        guard latest.hasPrefix(source) else { return nil }
        let suffix = String(latest.dropFirst(source.count))
        guard !suffix.isEmpty else { return Pending(inline: "", following: "") }
        if case .table = last.kind { return Pending(inline: "", following: suffix) }
        if case .code = last.kind, codeFenceClosed(source) {
            return Pending(inline: "", following: suffix)
        }
        if case .text = last.kind, String(last.text.characters).last?.isWhitespace != true {
            let separator = String(source.reversed().prefix(while: \.isWhitespace).reversed())
            return Pending(inline: separator + suffix, following: "")
        }
        return Pending(inline: suffix, following: "")
    }

    private static func codeFenceClosed(_ source: String) -> Bool {
        let lines = source.split(separator: "\n", omittingEmptySubsequences: false)
        guard let index = lines.lastIndex(where: { !$0.trimmingCharacters(in: .whitespaces).isEmpty }) else { return false }
        let line = lines[index].drop(while: { $0 == " " || $0 == "\t" })
        guard let marker = line.first, marker == "`" || marker == "~" else { return false }
        let fence = line.prefix(while: { $0 == marker })
        guard fence.count >= 3, line.dropFirst(fence.count).allSatisfy(\.isWhitespace) else { return false }
        // A lone opening fence is not a completed code block.
        return lines[..<index].contains { preceding in
            let trimmed = preceding.drop(while: { $0 == " " || $0 == "\t" })
            let opening = trimmed.prefix(while: { $0 == marker })
            return opening.count >= 3 && opening.count <= fence.count
        }
    }
}

/// Link appearance must not inherit the inbox's monochrome control tint.
/// Keep the URL attribute intact so Text retains its native link interaction.
enum ChatMarkdownInline {
    static func style(_ value: AttributedString, textSize: CGFloat) -> AttributedString {
        var styled = value
        for run in value.runs {
            if run.inlinePresentationIntent?.contains(.code) == true {
                styled[run.range].font = .system(size: textSize - 2, design: .monospaced)
                styled[run.range].backgroundColor = Color.primary.opacity(0.06)
            }
            if run.link != nil {
                styled[run.range].foregroundColor = .blue
                styled[run.range].underlineStyle = .single
            }
        }
        return styled
    }
}

struct ChatMarkdownTable: View {
    let rows: [[AttributedString]]
    let textSize: CGFloat

    var body: some View {
        ViewThatFits(in: .horizontal) {
            grid
            VStack(alignment: .leading, spacing: 6) {
                Label("Scroll horizontally for more columns", systemImage: "arrow.left.and.right")
                    .font(.caption).foregroundStyle(.secondary)
                ScrollView(.horizontal) { grid }
                    .scrollIndicators(.visible)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private var grid: some View {
        Grid(alignment: .topLeading, horizontalSpacing: 0, verticalSpacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.offset) { rowIndex, cells in
                GridRow {
                    ForEach(Array(cells.enumerated()), id: \.offset) { _, cell in
                        Text(ChatMarkdownInline.style(cell, textSize: textSize))
                            .font(.system(size: textSize, weight: rowIndex == 0 ? .semibold : .regular))
                            // Bound the content width even inside a horizontal ScrollView.
                            // Scaling with the font keeps columns readable at larger text sizes.
                            .frame(width: textSize * 8, alignment: .leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                            .padding(.horizontal, 14).padding(.vertical, 10)
                            .frame(maxHeight: .infinity, alignment: .topLeading)
                            .background(rowIndex == 0 ? ChatPalette.userBubble : .clear)
                            .overlay(alignment: .bottom) { Divider().opacity(0.5) }
                    }
                }
            }
        }
        .fixedSize(horizontal: true, vertical: false)
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
    private final class Parsed {
        let blocks: [ChatMarkdownBlock]
        init(_ blocks: [ChatMarkdownBlock]) { self.blocks = blocks }
    }
    private let cache: NSCache<NSString, Parsed> = {
        let cache = NSCache<NSString, Parsed>()
        cache.countLimit = 64
        cache.totalCostLimit = 8 * 1024 * 1024
        return cache
    }()
    func blocks(for text: String) throws -> [ChatMarkdownBlock] {
        assert(!Thread.isMainThread)
        try Task.checkCancellation()
        let key = text as NSString
        if let parsed = cache.object(forKey: key) { return parsed.blocks }
        let blocks = ChatMarkdownBlock.parse(text)
        try Task.checkCancellation()
        if text.utf8.count <= 1_000_000 {
            // Account for both the source key and rendered text/runs. NSCache
            // also releases recreatable results under system memory pressure.
            var cost = key.length * 4
            for block in blocks {
                cost += MemoryLayout<ChatMarkdownBlock>.stride + block.text.runs.count * 128
                if case .table(let rows) = block.kind {
                    cost += rows.reduce(0) { $0 + $1.reduce(0) { $0 + $1.runs.count * 128 } }
                }
            }
            if cost <= cache.totalCostLimit { cache.setObject(Parsed(blocks), forKey: key, cost: cost) }
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
