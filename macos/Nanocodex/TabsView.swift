import SwiftUI
import UniformTypeIdentifiers
import NanocodexRemote

struct TopTabsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var detaching = false
    var body: some View {
        let nodes = model.browserTabs
        WorkspaceGlassGroup {
        HStack(spacing: 8) {
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    LazyHStack(spacing: 5) {
                        ForEach(nodes) { node in
                            BrowserTab(node: node)
                                .containerRelativeFrame(.horizontal) { width, _ in
                                    min(224, max(124, (width - CGFloat(max(0, nodes.count - 1)) * 5) / CGFloat(max(1, nodes.count))))
                                }
                                .id(node.id)
                        }
                    }.padding(.vertical, 5)
                        .animation(reduceMotion ? nil : .smooth(duration: 0.24), value: nodes.map(\.id))
                }
                .onChange(of: model.activeTabID, initial: true) { _, id in
                    if let node = nodes.first(where: { $0.leaves.contains(id) }) { proxy.scrollTo(node.id) }
                }
            }
            if model.draggingPaneID != nil {
                Text("Drop here for a separate tab").font(.caption).foregroundStyle(.secondary)
                    .transition(.opacity)
            }
            Button("New tab", systemImage: "plus") { model.newTab() }
                .labelStyle(.iconOnly).frame(width: 28, height: 28)
                .help("New conversation (⌘T)").accessibilityIdentifier("new-tab-in-strip")
        }.buttonStyle(.plain).font(.system(size: 13))
            .padding(.horizontal, 12).frame(height: 44)
            .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(detaching ? Color.accentColor : .clear, lineWidth: 2))
            .onDrop(of: [WorkspaceDrag.paneType], isTargeted: $detaching) { _ in
                guard let id = model.draggingPaneID else { return false }
                model.separatePane(id); return true
            }
            .animation(reduceMotion ? nil : .smooth(duration: 0.2), value: model.activeTabID)
            .accessibilityElement(children: .contain).accessibilityIdentifier("top-tabs")
        }
    }
}

/// The system sidebar supplies selection, keyboard navigation, scrolling, and
/// the native sidebar material. Its detail column stays mounted when collapsed.
struct SidebarTabsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var detaching = false
    private var selection: Binding<String?> {
        Binding(get: {
            guard model.screen == .chat else { return nil }
            return model.browserTabs.first { $0.leaves.contains(model.activeTabID) }?.id
        }, set: { id in
            guard let node = model.browserTabs.first(where: { $0.id == id }) else { return }
            model.selectSidebarWorkspace(node)
        })
    }
    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Tabs").font(.headline)
                Text("\(model.browserTabs.count)").font(.caption).foregroundStyle(.secondary).monospacedDigit()
                Spacer()
                Button("New tab", systemImage: "plus") { model.newTab() }
                    .labelStyle(.iconOnly).buttonStyle(.borderless)
                    .help("New conversation (⌘T)").accessibilityIdentifier("new-tab-in-sidebar")
            }.padding(.horizontal, 18).padding(.top, 16).padding(.bottom, 8)
            List(selection: selection) {
                ForEach(model.browserTabs) { node in
                    BrowserTab(node: node, vertical: true)
                        .tag(node.id)
                        .listRowSeparator(.hidden)
                }
            }.listStyle(.sidebar)
                .onKeyPress(.return) { model.focusComposer(); return .handled }
                .onExitCommand { model.enterNavigation(in: NSApp.keyWindow) }
                .accessibilityIdentifier("sidebar-tab-list")
            if model.draggingPaneID != nil {
                Label("Drop here for a separate tab", systemImage: "plus.rectangle.on.rectangle")
                    .font(.caption).foregroundStyle(.secondary).padding(14)
            }
        }
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(detaching ? Color.accentColor : .clear, lineWidth: 2))
        .onDrop(of: [WorkspaceDrag.paneType], isTargeted: $detaching) { _ in
            guard let id = model.draggingPaneID else { return false }
            model.separatePane(id); return true
        }
        .accessibilityElement(children: .contain).accessibilityIdentifier("sidebar-tabs")
    }
}

/// Window actions belong to NSToolbar, so macOS supplies glass, grouping,
/// overflow, keyboard focus, and the correct contrast for the active window.
struct WorkspaceToolbar: ToolbarContent {
    let model: AppModel
    let canGoBack: Bool
    let canGoForward: Bool
    let showingScreens: Bool
    let screensAvailable: Bool
    let tabCount: Int
    let verticalTabs: Bool
    let setVerticalTabs: (Bool) -> Void
    init(model: AppModel, verticalTabs: Bool, setVerticalTabs: @escaping (Bool) -> Void) {
        self.model = model
        self.verticalTabs = verticalTabs; self.setVerticalTabs = setVerticalTabs
        canGoBack = model.canGoBack; canGoForward = model.canGoForward
        showingScreens = model.showingScreens; screensAvailable = model.remoteService != nil
        tabCount = model.browserTabs.count
    }
    var body: some ToolbarContent {
        ToolbarItemGroup(placement: .navigation) {
            Toggle(isOn: Binding(get: { verticalTabs }, set: setVerticalTabs)) {
                Label("Vertical tabs", systemImage: "sidebar.left")
            }.toggleStyle(.button)
                .help(verticalTabs ? "Use horizontal tabs" : "Use vertical tabs")
                .accessibilityValue(verticalTabs ? "Vertical tabs" : "Horizontal tabs")
                .accessibilityIdentifier("toggle-tab-sidebar")
            Button("Back", systemImage: "chevron.left") { model.navigateHistory(back: true) }
                .disabled(!canGoBack).help("Back (⌘[)").accessibilityIdentifier("conversation-back")
            Button("Forward", systemImage: "chevron.right") { model.navigateHistory(back: false) }
                .disabled(!canGoForward).help("Forward (⌘])").accessibilityIdentifier("conversation-forward")
        }
        ToolbarItemGroup(placement: .primaryAction) {
            Button("Search conversations", systemImage: "magnifyingglass") { model.showingSearch = true }
                .help("Search conversations (⌘K)").accessibilityIdentifier("search-threads")
            Button("New conversation", systemImage: "square.and.pencil") { model.newTab() }
                .help("New conversation (⌘T)").accessibilityIdentifier("new-tab")
            Button("Tab overview", systemImage: "square.on.square") { model.showingTabOverview = true }
                .help("Tab overview (⌘⇧O)").accessibilityValue("\(tabCount) tabs")
                .accessibilityIdentifier("tab-overview")
        }
        if #available(macOS 26.0, *) { ToolbarSpacer(.fixed, placement: .primaryAction) }
        ToolbarItemGroup(placement: .primaryAction) {
            WorkspaceLayoutMenu()
            Toggle(isOn: Binding(get: { showingScreens }, set: { model.showingScreens = $0 })) { Label("Remote screens", systemImage: "rectangle.trailinghalf.inset.filled") }
                .toggleStyle(.button).help("Show or hide screen pane (⌘⌥S)")
                .accessibilityValue(showingScreens ? "Visible" : "Hidden")
                .accessibilityIdentifier("workspace-remote-screens").disabled(!screensAvailable)
            Menu {
                Button("Hands", systemImage: "hand.raised") { model.screen = .hands }
                Button("Connections", systemImage: "link") { model.openAccount() }
                Divider()
                Button("Keyboard Shortcuts…", systemImage: "keyboard") { model.screen = .chat; model.showingKeyboardHelp = true }
                Button("Settings…", systemImage: "gearshape") { model.showingSettings = true }
            } label: { Label("Workspace and account", systemImage: "ellipsis") }
                .help("Workspace and account").accessibilityIdentifier("workspace-menu")
        }
        ToolbarItem(placement: .automatic) {
            RemoteSharingIndicator(host: model.remoteMacHost, phoneHost: model.remotePhoneHost)
        }
    }
}

private struct BrowserTab: View {
    @EnvironmentObject private var model: AppModel
    let node: PaneNode
    var vertical = false
    private var selected: Bool { node.leaves.contains(model.activeTabID) && model.screen == .chat }
    private var agent: WorkspaceTab? { model.tab(node.leaves.contains(model.activeTabID) ? model.activeTabID : node.leaves[0]) }
    private var running: Bool { node.leaves.contains { model.working($0) } }
    private var attentionColor: Color? {
        let agents = node.leaves.compactMap { model.tab($0) }
        if agents.contains(where: model.hasAttentionError) { return .orange }
        return agents.contains { model.update(for: $0).needsAttention($0) } ? .accentColor : nil
    }
    var body: some View {
        BrowserTabContent(model: model, node: node, selected: selected, agentID: agent?.id,
                          title: agent.map(model.title) ?? "New thread", running: running,
                          attentionColor: attentionColor, vertical: vertical).equatable()
    }
}

/// Draft and transcript updates don't change a tab's controls. Keep those
/// publications outside its hover, drag, material, and accessibility tree.
private struct BrowserTabContent: View, Equatable {
    let model: AppModel
    let node: PaneNode
    let selected: Bool
    let agentID: String?
    let title: String
    let running: Bool
    let attentionColor: Color?
    let vertical: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false
    @State private var dropTarget = false
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.model === rhs.model && lhs.node == rhs.node && lhs.selected == rhs.selected
            && lhs.agentID == rhs.agentID && lhs.title == rhs.title
            && lhs.running == rhs.running && lhs.attentionColor == rhs.attentionColor
            && lhs.vertical == rhs.vertical
    }
    var body: some View {
        HStack(spacing: 8) {
            Group {
                if vertical { tabLabel.accessibilityElement(children: .ignore) }
                else { Button { model.selectWorkspace(node) } label: { tabLabel }.buttonStyle(.plain) }
            }.accessibilityIdentifier("select-browser-tab-" + node.id)
                .accessibilityLabel(title)
                .accessibilityValue(running ? "Working" : attentionColor != nil ? "Ready to review" : "Up to date")
                .accessibilityAddTraits(selected ? [.isSelected] : [])
            Button { model.closeWorkspace(node) } label: {
                Image(systemName: "xmark").font(.system(size: 9, weight: .medium)).frame(width: 20, height: 24).contentShape(Rectangle())
            }.buttonStyle(.plain)
                .opacity(hovering || selected ? 1 : 0).help("Close tab").accessibilityLabel("Close tab")
                .accessibilityIdentifier("close-browser-tab-" + node.id)
        }.padding(.horizontal, vertical ? 4 : 11).frame(maxWidth: .infinity).frame(height: vertical ? 30 : 32)
            .background(RoundedRectangle(cornerRadius: 10).fill(Color.primary.opacity(hovering && !selected ? 0.05 : 0)))
            .workspaceGlass(cornerRadius: 10, enabled: selected && !vertical, interactive: true)
            .overlay(alignment: vertical ? .top : .leading) {
                if dropTarget {
                    Capsule().fill(Color.accentColor)
                        .frame(width: vertical ? nil : 3, height: vertical ? 3 : nil)
                        .padding(vertical ? .horizontal : .vertical, 4)
                }
            }
            .animation(reduceMotion ? nil : .smooth(duration: 0.18), value: hovering)
            .animation(reduceMotion ? nil : .smooth(duration: 0.18), value: dropTarget)
            .contentShape(Rectangle()).onHover { hovering = $0 }
            .accessibilityElement(children: .contain).accessibilityIdentifier("browser-tab-" + node.id)
            .onDrag { NSItemProvider(item: node.leaves[0] as NSString, typeIdentifier: WorkspaceDrag.tabType.identifier) }
            .onDrop(of: [WorkspaceDrag.tabType], isTargeted: $dropTarget) { providers in
                guard let provider = providers.first else { return false }
                _ = provider.loadItem(forTypeIdentifier: WorkspaceDrag.tabType.identifier) { value, _ in
                    let id = (value as? String) ?? (value as? Data).flatMap { String(data: $0, encoding: .utf8) }
                    if let id { Task { @MainActor in model.moveTab(id, before: node.leaves[0]) } }
                }
                return true
            }
            .contextMenu {
                Button("Split Right") { model.selectWorkspace(node); model.splitAgent(axis: "horizontal") }
                Button("Split Below") { model.selectWorkspace(node); model.splitAgent(axis: "vertical") }
                if let agentID { Button("Rename Agent…") { if let agent = model.tab(agentID) { model.renameTab(agent) } } }
                Divider()
                Button("Close Tab") { model.closeWorkspace(node) }
            }
    }
    private var tabLabel: some View {
        HStack(spacing: 8) {
            if running { ProgressView().controlSize(.mini) }
            else {
                Image(systemName: node.leaves.count > 1 ? "rectangle.split.2x2" : "bubble.left").foregroundStyle(.secondary)
                    .overlay(alignment: .topTrailing) {
                        if let attentionColor { Circle().fill(attentionColor).frame(width: 5, height: 5).offset(x: 3, y: -2) }
                    }
            }
            Text(title).lineLimit(1).font(.system(size: vertical ? 13 : 12, weight: selected ? .medium : .regular))
            if node.leaves.count > 1 { Text("\(node.leaves.count)").font(.system(size: 10, weight: .medium)).foregroundStyle(.secondary) }
            Spacer(minLength: 0)
        }.contentShape(Rectangle())
    }
}

struct TabOverviewView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @FocusState private var searching: Bool
    private var workspaces: [PaneNode] {
        model.browserTabs.filter { node in
            query.isEmpty || node.leaves.contains { id in model.tab(id).map { model.title($0).localizedCaseInsensitiveContains(query) } == true }
        }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Open conversations").font(.title2.weight(.semibold))
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            TextField("Find an open conversation", text: $query).textFieldStyle(.roundedBorder).focused($searching)
                .accessibilityIdentifier("tab-overview-search")
                .onSubmit { if let first = workspaces.first { open(first) } }
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 260), alignment: .top)], spacing: 12) {
                    ForEach(workspaces) { node in
                        Button { open(node) } label: {
                            VStack(alignment: .leading, spacing: 12) {
                                HStack {
                                    Image(systemName: node.leaves.count > 1 ? "rectangle.split.2x2" : "bubble.left")
                                    Text(node.leaves.count > 1 ? "\(node.leaves.count) panes" : "Conversation").font(.caption)
                                    Spacer()
                                    if node.leaves.contains(model.activeTabID) { Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.accentColor) }
                                }.foregroundStyle(.secondary)
                                ForEach(node.leaves, id: \.self) { id in
                                    if let tab = model.tab(id) {
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text(model.title(tab)).font(.headline).lineLimit(2)
                                            Text(status(tab)).font(.caption).foregroundStyle(.secondary)
                                            if !tab.draft.isEmpty { Text("Draft: " + tab.draft).font(.caption).foregroundStyle(.secondary).lineLimit(2) }
                                        }.frame(maxWidth: .infinity, alignment: .leading)
                                    }
                                }
                            }.padding(16).frame(maxWidth: .infinity, minHeight: 116, alignment: .topLeading)
                                .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 14))
                                .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(node.leaves.contains(model.activeTabID) ? Color.accentColor : Color.primary.opacity(0.08)))
                                .contentShape(RoundedRectangle(cornerRadius: 14))
                        }.buttonStyle(.plain).accessibilityIdentifier("overview-tab-" + node.id)
                    }
                }
                if workspaces.isEmpty { ContentUnavailableView.search(text: query) }
            }
            HStack {
                Button("Search all history…") { dismiss(); model.showingSearch = true }
                Spacer()
                Button("New conversation", systemImage: "plus") { dismiss(); model.newTab() }
            }
        }.padding(24).frame(width: 680, height: 520)
            .background(Color(nsColor: .windowBackgroundColor)).foregroundStyle(.primary)
            .onAppear { searching = true }
            .accessibilityIdentifier("tab-overview-content")
    }
    private func open(_ node: PaneNode) { model.selectWorkspace(node); dismiss() }
    private func status(_ tab: WorkspaceTab) -> String {
        if model.hasAttentionError(tab) { return "Needs attention" }
        if model.working(tab.id) { return "Working" }
        if model.update(for: tab).needsAttention(tab) { return "Ready to review" }
        return tab.threadId == nil ? "New conversation" : "Up to date"
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
