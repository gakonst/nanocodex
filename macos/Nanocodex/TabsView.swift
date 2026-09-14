import SwiftUI
import UniformTypeIdentifiers

struct TopTabsView: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        HStack(spacing: 8) {
            Button { model.showingSearch = true } label: { Image(systemName: "magnifyingglass") }
                .help("Search conversations (⌘K)").accessibilityIdentifier("search-threads")
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 5) {
                        ForEach(model.browserTabs) { node in BrowserTab(node: node).id(node.id) }
                    }.padding(.vertical, 5)
                }
                .onChange(of: model.activeTabID) { _, id in
                    if let node = model.browserTabs.first(where: { $0.leaves.contains(id) }) { proxy.scrollTo(node.id) }
                }
            }
            Button { model.newTab() } label: { Image(systemName: "plus") }
                .help("New tab (⌘T)").accessibilityIdentifier("new-tab")
            Menu {
                Button("Hands", systemImage: "hand.raised") { model.screen = .hands }
                Button("Remote Screens", systemImage: "display") { model.showingScreens = true }
                Button("Connections", systemImage: "link") { model.openAccount() }
                Divider()
                Button("Search History…", systemImage: "clock") { model.showingSearch = true }
                Button("Settings…", systemImage: "gearshape") { model.showingSettings = true }
            } label: { Image(systemName: "ellipsis") }
            .menuStyle(.borderlessButton).fixedSize().help("Workspace and account")
        }.buttonStyle(.plain).font(.system(size: 13))
            .padding(.horizontal, 14).frame(height: 46).background(.ultraThinMaterial)
            .accessibilityIdentifier("top-tabs")
    }
}

private struct BrowserTab: View {
    @EnvironmentObject private var model: AppModel
    let node: PaneNode
    @State private var hovering = false
    private var selected: Bool { node.leaves.contains(model.activeTabID) && model.screen == .chat }
    private var agent: WorkspaceTab? { model.tab(node.leaves.contains(model.activeTabID) ? model.activeTabID : node.leaves[0]) }
    private var running: Bool { node.leaves.contains { model.working($0) } }
    private var attentionColor: Color? {
        let agents = node.leaves.compactMap { model.tab($0) }
        if agents.contains(where: model.hasAttentionError) { return .orange }
        return agents.contains { model.update(for: $0).needsAttention($0) } ? .accentColor : nil
    }
    var body: some View {
        HStack(spacing: 8) {
            Button { model.selectWorkspace(node) } label: {
                HStack(spacing: 8) {
                    if running { ProgressView().controlSize(.mini) }
                    else {
                        Image(systemName: node.leaves.count > 1 ? "rectangle.split.2x2" : "bubble.left").foregroundStyle(.secondary)
                            .overlay(alignment: .topTrailing) {
                                if let attentionColor { Circle().fill(attentionColor).frame(width: 5, height: 5).offset(x: 3, y: -2) }
                            }
                    }
                    Text(agent.map(model.title) ?? "New thread").lineLimit(1).font(.system(size: 12, weight: selected ? .medium : .regular))
                    if node.leaves.count > 1 { Text("\(node.leaves.count)").font(.system(size: 10, weight: .medium)).foregroundStyle(.secondary) }
                    Spacer(minLength: 0)
                }.contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("select-browser-tab-" + node.id)
            Button { model.closeWorkspace(node) } label: { Image(systemName: "xmark").font(.system(size: 9, weight: .medium)) }
                .opacity(hovering || selected ? 1 : 0).help("Close tab").accessibilityLabel("Close tab")
        }.padding(.horizontal, 11).frame(width: 192, height: 32)
            .background(selected ? Color.primary.opacity(0.085) : Color.primary.opacity(hovering ? 0.04 : 0), in: RoundedRectangle(cornerRadius: 9))
            .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(Color.primary.opacity(selected ? 0.08 : 0)))
            .contentShape(Rectangle()).onHover { hovering = $0 }
            .accessibilityElement(children: .contain).accessibilityIdentifier("browser-tab-" + node.id)
            .onDrag { NSItemProvider(object: node.leaves[0] as NSString) }
            .onDrop(of: [.plainText], isTargeted: nil) { providers in
                guard let provider = providers.first else { return false }
                _ = provider.loadObject(ofClass: String.self) { value, _ in if let value { Task { @MainActor in model.moveTab(value, before: node.leaves[0]) } } }
                return true
            }
            .contextMenu {
                Button("Split Right") { model.selectWorkspace(node); model.splitAgent(axis: "horizontal") }
                Button("Split Below") { model.selectWorkspace(node); model.splitAgent(axis: "vertical") }
                if let agent { Button("Rename Agent…") { model.renameTab(agent) } }
                Divider()
                Button("Close Tab") { model.closeWorkspace(node) }
            }
    }
}

struct ThreadSearchView: View {
    @EnvironmentObject private var model: AppModel
    @State private var query = ""
    @State private var selectedID: String?
    @FocusState private var focused: Bool
    var filtered: [AgentThread] { model.state.threads.filter { query.isEmpty || $0.title.localizedCaseInsensitiveContains(query) } }
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search threads", text: $query).textFieldStyle(.plain).font(.system(size: 17)).focused($focused).accessibilityIdentifier("search-threads-input")
                    .onSubmit { if let thread = filtered.first(where: { $0.id == selectedID }) ?? filtered.first { model.open(thread) } }
                    .onKeyPress(.upArrow) { moveSelection(-1); return .handled }
                    .onKeyPress(.downArrow) { moveSelection(1); return .handled }
                    .onChange(of: query) { selectedID = filtered.first?.id }
                Button("Done") { model.showingSearch = false }.keyboardShortcut(.cancelAction)
            }.padding(20)
            Divider()
            if filtered.isEmpty { ContentUnavailableView(query.isEmpty ? "No threads yet" : "No matching threads", systemImage: "bubble.left.and.bubble.right", description: Text(query.isEmpty ? "Start a new thread to get going." : "Try a different search.")) }
            else {
                List(filtered, selection: $selectedID) { thread in
                    Button { model.open(thread) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "bubble.left").foregroundStyle(.secondary)
                            VStack(alignment: .leading, spacing: 4) { Text(thread.title).lineLimit(1); Text("\(thread.turnCount) turns").font(.caption).foregroundStyle(.secondary) }
                            Spacer()
                            Text(Date(timeIntervalSince1970: thread.updatedAt / 1000), style: .relative).font(.caption).foregroundStyle(.secondary)
                        }.padding(.vertical, 7).contentShape(Rectangle())
                    }.buttonStyle(.plain).tag(thread.id)
                }.listStyle(.plain)
            }
        }.frame(width: 600, height: 450).onAppear { selectedID = filtered.first?.id; focused = true }
    }
    private func moveSelection(_ offset: Int) {
        guard !filtered.isEmpty else { return }
        let index = filtered.firstIndex { $0.id == selectedID } ?? 0
        selectedID = filtered[min(filtered.count - 1, max(0, index + offset))].id
    }
}
