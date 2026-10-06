import SwiftUI
import InboxCore

private struct MemoryEntry: Identifiable {
    let path: String
    let isDirectory: Bool
    var id: String { path }
    var name: String { path.split(separator: "/").last.map(String.init) ?? path }
}

private struct MemoryDirectory {
    var entries: [MemoryEntry] = []
    var cursor: String?
    var loaded = false
    var loading = false
    var error: String?
    var request = UUID()
}

private enum MemoryTreeRow: Identifiable {
    case entry(MemoryEntry, depth: Int)
    case status(path: String, depth: Int)

    var id: String {
        switch self {
        case .entry(let entry, _): "entry:" + entry.path
        case .status(let path, _): "status:" + path
        }
    }
}

@MainActor
private final class MemoryBrowser: ObservableObject {
    let model: InboxModel
    let account: UUID
    @Published var expanded: Set<String> = []
    @Published private(set) var directories: [String: MemoryDirectory] = [:]
    private var revision = UUID()

    init(model: InboxModel) {
        self.model = model
        account = model.vaultIntakeAccount
    }

    var isCurrent: Bool { model.connected && model.vaultIntakeAccount == account }

    var rows: [MemoryTreeRow] {
        guard isCurrent else { return [] }
        var result: [MemoryTreeRow] = []
        func appendDirectory(_ path: String, depth: Int) {
            let directory = directories[path] ?? MemoryDirectory()
            for entry in directory.entries {
                result.append(.entry(entry, depth: depth))
                if entry.isDirectory && expanded.contains(entry.path) {
                    appendDirectory(entry.path, depth: depth + 1)
                }
            }
            if !directory.loaded || directory.loading || directory.error != nil
                || directory.entries.isEmpty || directory.cursor != nil {
                result.append(.status(path: path, depth: depth))
            }
        }
        appendDirectory("", depth: 0)
        return result
    }

    func toggle(_ path: String) {
        guard isCurrent else { return }
        if expanded.contains(path) { expanded.remove(path) }
        else {
            expanded.insert(path)
            if directories[path]?.loaded != true {
                Task { await load(path) }
            }
        }
    }

    func refresh() async {
        guard isCurrent else { return }
        revision = UUID()
        directories = [:]
        expanded = []
        await load("")
    }

    func load(_ path: String, more: Bool = false) async {
        guard isCurrent else { return }
        var directory = directories[path] ?? MemoryDirectory()
        guard !directory.loading, !more || directory.cursor != nil else { return }
        let cursor = more ? directory.cursor : nil
        let token = UUID(), version = revision
        directory.loading = true; directory.error = nil; directory.request = token
        directories[path] = directory
        defer {
            if isCurrent, revision == version, directories[path]?.request == token {
                directories[path]?.loading = false
            }
        }
        do {
            let result = try await model.memoryList(path: path, cursor: cursor)
            try Task.checkCancellation()
            guard isCurrent, revision == version, directories[path]?.request == token else { return }
            guard case .array(let values) = result["entries"], case .bool(let truncated) = result["truncated"] else {
                throw APIError.invalidResponse
            }
            let entries = try values.map { value -> MemoryEntry in
                guard case .string(let child) = value["path"], !child.isEmpty,
                      case .string(let kind) = value["entry_type"], ["file", "directory"].contains(kind) else {
                    throw APIError.invalidResponse
                }
                // Accept immediate children only: malformed responses must not
                // create recursive folders or escape their displayed parent.
                let prefix = path.isEmpty ? "" : path + "/"
                guard child.hasPrefix(prefix) else { throw APIError.invalidResponse }
                let name = String(child.dropFirst(prefix.count))
                guard !name.isEmpty, name != ".", name != "..", !name.contains("/") else {
                    throw APIError.invalidResponse
                }
                return MemoryEntry(path: child, isDirectory: kind == "directory")
            }
            let next = result["next_cursor"].string
            guard !truncated || (!next.isEmpty && next != cursor) else { throw APIError.invalidResponse }
            var combined = more ? directory.entries : []
            var known = Set(combined.map(\.path))
            combined.append(contentsOf: entries.filter { known.insert($0.path).inserted })
            // Keep folders together, including entries received on later pages.
            combined.sort {
                if $0.isDirectory != $1.isDirectory { return $0.isDirectory }
                return $0.path.localizedStandardCompare($1.path) == .orderedAscending
            }
            directory.entries = combined
            directory.cursor = truncated ? next : nil
            directory.loaded = true
            directory.loading = false
            directories[path] = directory
        } catch is CancellationError {
        } catch {
            guard isCurrent, revision == version, directories[path]?.request == token else { return }
            directories[path]?.error = memoryErrorDescription(error)
        }
    }
}

struct MemoriesView: View {
    @ObservedObject var model: InboxModel
    @StateObject private var browser: MemoryBrowser

    init(model: InboxModel) {
        self.model = model
        _browser = StateObject(wrappedValue: MemoryBrowser(model: model))
    }

    var body: some View {
        List {
            Section {
                HStack {
                    Text("Memories").font(.title2.weight(.semibold)).accessibilityAddTraits(.isHeader)
                    Spacer()
                    Button {
                        Task { await browser.refresh() }
                    } label: {
                        Image(systemName: "arrow.clockwise").frame(minWidth: 44, minHeight: 44)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Refresh memories")
                    .accessibilityIdentifier("memories-refresh")
                    .disabled(browser.directories[""]?.loading == true)
                }
                Text("Browse your saved memory files. Shared team files appear in the team folder when available.")
                    .font(.subheadline).foregroundStyle(.secondary)
            }
            Section {
                ForEach(browser.rows) { row in
                    switch row {
                    case .entry(let entry, let depth):
                        entryRow(entry, depth: depth)
                    case .status(let path, let depth):
                        directoryStatus(path).padding(.leading, indentation(depth))
                    }
                }
            }
        }
        .listStyle(.plain)
        .accessibilityIdentifier("memories-tree")
        .task {
            if browser.directories[""]?.loaded != true { await browser.load("") }
        }
        .refreshable { await browser.refresh() }
    }

    // Preserve readable row widths in deeply nested folders and large text sizes.
    private func indentation(_ depth: Int) -> CGFloat { CGFloat(min(depth, 6)) * 12 }

    @ViewBuilder
    private func entryRow(_ entry: MemoryEntry, depth: Int) -> some View {
        if entry.isDirectory {
            Button { browser.toggle(entry.path) } label: {
                HStack(spacing: 10) {
                    Image(systemName: browser.expanded.contains(entry.path) ? "chevron.down" : "chevron.right")
                        .font(.caption.weight(.semibold)).frame(width: 12).accessibilityHidden(true)
                    Image(systemName: "folder").accessibilityHidden(true)
                    Text(entry.name).foregroundStyle(.primary).multilineTextAlignment(.leading)
                    Spacer(minLength: 0)
                }
                .padding(.leading, indentation(depth)).frame(minHeight: 44).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(entry.path + ", folder")
            .accessibilityValue(browser.expanded.contains(entry.path) ? "Expanded" : "Collapsed")
            .accessibilityHint(browser.expanded.contains(entry.path) ? "Collapse folder" : "Expand folder")
            .accessibilityIdentifier("memory-folder-" + entry.path)
        } else {
            NavigationLink {
                MemoryReaderView(model: model, path: entry.path, account: browser.account)
                    .id(browser.account)
            } label: {
                Label(entry.name, systemImage: "doc.text")
                    .padding(.leading, indentation(depth) + 22).frame(minHeight: 44)
            }
            .accessibilityLabel(entry.path + ", file")
            .accessibilityIdentifier("memory-file-" + entry.path)
        }
    }

    @ViewBuilder
    private func directoryStatus(_ path: String) -> some View {
        let directory = browser.directories[path] ?? MemoryDirectory()
        VStack(alignment: .leading, spacing: 12) {
            if directory.loading || (!directory.loaded && directory.error == nil) {
                ProgressView("Loading memories…")
            } else if let error = directory.error {
                Text(error).font(.subheadline).foregroundStyle(.secondary)
                Button("Retry") {
                    Task { await browser.load(path, more: directory.loaded && directory.cursor != nil) }
                }
                .buttonStyle(.bordered)
                .accessibilityLabel(path.isEmpty ? "Retry loading memories" : "Retry loading " + path)
                .accessibilityIdentifier("memory-retry-list-" + path)
            } else if directory.loaded && directory.entries.isEmpty {
                Text(path.isEmpty ? "No memories yet" : "This folder is empty.")
                    .foregroundStyle(.secondary)
                if path.isEmpty {
                    Text("Ask in chat to remember something, then refresh this list.")
                        .font(.subheadline).foregroundStyle(.secondary)
                }
            }
            if directory.cursor != nil && !directory.loading && directory.error == nil {
                Button("Load more") { Task { await browser.load(path, more: true) } }
                    .buttonStyle(.bordered)
                    .accessibilityLabel(path.isEmpty ? "Load more memories" : "Load more in " + path)
                    .accessibilityIdentifier("memory-load-more-" + path)
            }
        }
        .padding(.vertical, 8)
    }
}

private struct MemoryReaderView: View {
    @ObservedObject var model: InboxModel
    let path: String
    let account: UUID
    @State private var content = ""
    @State private var loaded = false
    @State private var loading = false
    @State private var error: String?
    @State private var nextLine: Int? = 1
    @State private var omittedLongLine = false
    @State private var request = UUID()

    private var isCurrent: Bool { model.connected && model.vaultIntakeAccount == account }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if isCurrent {
                    Text(path).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    if loaded {
                        if content.isEmpty { Text("This file is empty.").foregroundStyle(.secondary) }
                        else {
                            Text(verbatim: content)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .textSelection(.enabled)
                                .accessibilityIdentifier("memory-content")
                        }
                    }
                    if omittedLongLine {
                        Label("A line exceeds the memory service’s reading limit. Its middle is omitted in the text above.", systemImage: "exclamationmark.triangle")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                    if loading { ProgressView("Loading file…") }
                    if let error {
                        Text(error).font(.subheadline).foregroundStyle(.secondary)
                        Button("Retry") { Task { await load() } }
                            .buttonStyle(.bordered).accessibilityIdentifier("memory-retry-read")
                    } else if loaded && nextLine != nil && !loading {
                        Button("Read more") { Task { await load() } }
                            .buttonStyle(.bordered).accessibilityIdentifier("memory-read-more")
                    }
                }
            }
            .padding().frame(maxWidth: 760, alignment: .leading).frame(maxWidth: .infinity)
        }
        .accessibilityIdentifier("memory-reader")
        .navigationTitle(path.split(separator: "/").last.map(String.init) ?? path)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .task { if !loaded { await load() } }
        .refreshable {
            request = UUID(); content = ""; loaded = false; loading = false
            error = nil; nextLine = 1; omittedLongLine = false
            await load()
        }
    }

    @MainActor private func load() async {
        guard isCurrent, !loading, let offset = nextLine else { return }
        let token = UUID()
        request = token; loading = true; error = nil
        defer { if request == token { loading = false } }
        do {
            var limit = 200
            var text = ""
            var truncated = false
            var byteTruncated = false
            while true {
                let result = try await model.memoryRead(path: path, lineOffset: offset, maxLines: limit)
                try Task.checkCancellation()
                guard isCurrent, request == token else { return }
                guard case .string(let page) = result["content"],
                      case .bool(let hasMore) = result["truncated"],
                      result["path"].string == path,
                      result["start_line_number"] == .number(Double(offset)) else { throw APIError.invalidResponse }
                // The existing API caps text at 80,000 UTF-8 bytes by retaining
                // its head AND tail. Reduce the requested window before using
                // newline counts, otherwise continuation could silently skip text.
                byteTruncated = page.utf8.count > 80_000
                if byteTruncated && limit > 1 { limit = max(1, limit / 2); continue }
                text = page; truncated = hasMore
                break
            }
            let newlines = text.utf8.filter { $0 == 10 }.count
            if truncated && !byteTruncated && (newlines == 0 || !text.hasSuffix("\n")) {
                throw APIError.invalidResponse
            }
            content += text
            loaded = true
            omittedLongLine = omittedLongLine || byteTruncated
            nextLine = truncated && newlines > 0 ? offset + newlines : nil
        } catch is CancellationError {
        } catch {
            if isCurrent, request == token { self.error = memoryErrorDescription(error) }
        }
    }
}

private func memoryErrorDescription(_ error: Error) -> String {
    switch error as? APIError {
    case .http(401), .invalidCredential: "Sign in again to read your memories."
    case .http(403): "This account doesn’t have permission to read these memories."
    case .http(404): "This memory is no longer available. Refresh the file list."
    default: "Couldn’t load memories. " + error.localizedDescription
    }
}
