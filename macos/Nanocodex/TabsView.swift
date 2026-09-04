import SwiftUI
import UniformTypeIdentifiers

struct SidebarView: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            navButton("New thread", symbol: "square.and.pencil", shortcut: "⌘N") { model.newTab() }.accessibilityIdentifier("new-thread")
            navButton("Search", symbol: "magnifyingglass", shortcut: "⌘K") { model.showingSearch = true }
            navButton("Hands", symbol: "hand.raised", selected: model.screen == .hands) { model.screen = .hands }.accessibilityIdentifier("hands-navigation")
            navButton("Connections", symbol: "link") { model.openAccount() }
            if model.tabPosition == "left" {
                HStack {
                    Text("Tabs").font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
                    Spacer()
                    Button { model.newTab() } label: { Image(systemName: "plus").font(.system(size: 11)) }.buttonStyle(.plain).help("New tab")
                }.padding(.horizontal, 10).padding(.top, 26).padding(.bottom, 4)
                ScrollView {
                    LazyVStack(spacing: 3) { ForEach(model.tabs) { tab in TabItem(tab: tab, horizontal: false) } }
                }.scrollIndicators(.hidden)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Threads").font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
                    Text("Your open threads are in the tab bar above.").font(.system(size: 12)).foregroundStyle(.secondary)
                    Button("Browse history") { model.showingSearch = true }.buttonStyle(.link)
                }.padding(10).padding(.top, 22)
                Spacer()
            }
            Spacer(minLength: 12)
            HStack(spacing: 9) {
                ZStack { RoundedRectangle(cornerRadius: 8).fill(Color.primary.opacity(0.07)); Text("N").font(.system(size: 16, weight: .semibold)) }.frame(width: 30, height: 30)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Nanocodex").font(.system(size: 12, weight: .medium))
                    HStack(spacing: 5) {
                        Circle().fill(model.state.connected ? .green : Color.secondary.opacity(0.5)).frame(width: 5, height: 5)
                        Text(model.isStarting ? "Connecting…" : model.state.connected ? "Connected" : "Connect account").font(.system(size: 11)).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Button { model.showingSettings = true } label: { Image(systemName: "gearshape").foregroundStyle(.secondary) }.buttonStyle(.plain).help("Settings (⌘,)").accessibilityIdentifier("settings-navigation")
            }.padding(9)
        }.padding(.horizontal, 10).padding(.top, 16).padding(.bottom, 8)
    }
    private func navButton(_ title: String, symbol: String, shortcut: String = "", selected: Bool = false, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 10) { Image(systemName: symbol).frame(width: 17); Text(title); Spacer(); Text(shortcut).foregroundStyle(.tertiary).font(.system(size: 11)) }
                .font(.system(size: 13)).padding(.horizontal, 10).padding(.vertical, 9)
                .background(selected ? Color.primary.opacity(0.06) : .clear, in: RoundedRectangle(cornerRadius: 7))
                .contentShape(Rectangle())
        }.buttonStyle(.plain)
    }
}

struct TopTabsView: View {
    @EnvironmentObject private var model: AppModel
    var body: some View {
        HStack(spacing: 3) {
            ScrollView(.horizontal) { HStack(spacing: 3) { ForEach(model.tabs) { tab in TabItem(tab: tab, horizontal: true).frame(width: 205) } }.padding(6) }.scrollIndicators(.hidden)
            Button { model.newTab() } label: { Image(systemName: "plus").padding(10) }.buttonStyle(.plain).help("New tab")
        }.background(Color(nsColor: .windowBackgroundColor)).overlay(alignment: .bottom) { Divider().opacity(0.35) }.accessibilityIdentifier("top-tabs")
    }
}

struct TabItem: View {
    @EnvironmentObject private var model: AppModel
    let tab: WorkspaceTab
    let horizontal: Bool
    @State private var hovering = false
    var selected: Bool { tab.id == model.activeTabID && model.screen == .chat }
    var running: Bool { tab.threadId.flatMap { model.snapshots[$0] }.map { !$0.activeTurns.isEmpty } ?? (model.pending[tab.id] != nil) }
    var body: some View {
        HStack(spacing: 7) {
            Button { model.select(tab.id) } label: {
                HStack(spacing: 8) {
                    if running { ProgressView().controlSize(.mini).frame(width: 13, height: 13) }
                    else { Image(systemName: tab.threadId == nil ? "square.and.pencil" : "bubble.left").font(.system(size: 12)).foregroundStyle(.secondary).frame(width: 13) }
                    Text(model.title(tab)).font(.system(size: 12)).lineLimit(1).truncationMode(.tail)
                    Spacer(minLength: 0)
                }.contentShape(Rectangle())
            }.buttonStyle(.plain).accessibilityIdentifier("tab-\(tab.id)")
            Button { model.closeTab(tab.id) } label: { Image(systemName: "xmark").font(.system(size: 9, weight: .medium)).frame(width: 16, height: 18) }
                .buttonStyle(.plain).foregroundStyle(.secondary).opacity(hovering || selected ? 1 : 0).help("Close tab").accessibilityLabel("Close \(model.title(tab))")
        }
        .padding(.horizontal, 10).padding(.vertical, horizontal ? 9 : 10)
        .background(selected ? Color.primary.opacity(0.075) : hovering ? Color.primary.opacity(0.035) : .clear, in: RoundedRectangle(cornerRadius: 7))
        .onHover { hovering = $0 }
        .onDrag { NSItemProvider(object: tab.id as NSString) }
        .onDrop(of: [.plainText], isTargeted: nil) { providers in
            guard let provider = providers.first else { return false }
            _ = provider.loadObject(ofClass: NSString.self) { value, _ in
                if let id = value as? String { Task { @MainActor in model.moveTab(id, before: tab.id) } }
            }
            return true
        }
        .contextMenu {
            Button("Rename Tab…") { model.renameTab(tab) }
            Button("Close Tab") { model.closeTab(tab.id) }
            Button("New Tab") { model.newTab() }
            Divider()
            Button(horizontal ? "Move Tabs to Sidebar" : "Move Tabs to Top") { model.tabPosition = horizontal ? "left" : "top"; model.persistLayout() }
        }
        .help(model.title(tab))
    }
}

struct ThreadSearchView: View {
    @EnvironmentObject private var model: AppModel
    @State private var query = ""
    @FocusState private var focused: Bool
    var filtered: [AgentThread] { model.state.threads.filter { query.isEmpty || $0.title.localizedCaseInsensitiveContains(query) } }
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search threads", text: $query).textFieldStyle(.plain).font(.system(size: 17)).focused($focused).accessibilityIdentifier("search-threads-input")
                Button("Done") { model.showingSearch = false }.keyboardShortcut(.cancelAction)
            }.padding(20)
            Divider()
            if filtered.isEmpty { ContentUnavailableView(query.isEmpty ? "No threads yet" : "No matching threads", systemImage: "bubble.left.and.bubble.right", description: Text(query.isEmpty ? "Start a new thread to get going." : "Try a different search.")) }
            else {
                List(filtered) { thread in
                    Button { model.open(thread) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "bubble.left").foregroundStyle(.secondary)
                            VStack(alignment: .leading, spacing: 4) { Text(thread.title).lineLimit(1); Text("\(thread.turnCount) turns").font(.caption).foregroundStyle(.secondary) }
                            Spacer()
                            Text(Date(timeIntervalSince1970: thread.updatedAt / 1000), style: .relative).font(.caption).foregroundStyle(.secondary)
                        }.padding(.vertical, 7).contentShape(Rectangle())
                    }.buttonStyle(.plain)
                }.listStyle(.plain)
            }
        }.frame(width: 600, height: 450).onAppear { focused = true }
    }
}
