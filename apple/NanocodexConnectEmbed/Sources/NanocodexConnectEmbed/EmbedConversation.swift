import SwiftUI

/// Host-controlled geometry. Defaults add no chrome or fixed content width.
public struct EmbedTranscriptLayout: Equatable {
    public var maximumRowWidth: CGFloat?
    public var horizontalPadding: CGFloat
    public var rowSpacing: CGFloat
    public var verticalPadding: CGFloat

    public init(maximumRowWidth: CGFloat? = nil, horizontalPadding: CGFloat = 0,
                rowSpacing: CGFloat = 0, verticalPadding: CGFloat = 0) {
        self.maximumRowWidth = maximumRowWidth
        self.horizontalPadding = horizontalPadding
        self.rowSpacing = rowSpacing
        self.verticalPadding = verticalPadding
    }
}

/// One host-rendered row. IDs must be unique within a conversation and stable
/// across streaming updates. Change revision whenever any rendering input changes.
public struct EmbedConversationRow: Identifiable {
    public let id: String
    public let countsAsMessage: Bool
    public let revision: AnyHashable
    let content: () -> AnyView

    public init<Content: View>(id: String, countsAsMessage: Bool = true,
                               revision: AnyHashable, @ViewBuilder content: @escaping () -> Content) {
        self.id = id
        self.countsAsMessage = countsAsMessage
        self.revision = revision
        self.content = { AnyView(content()) }
    }
}

/// Automatic uses the virtualized native engine on iOS 18+. The SwiftUI scroll
/// view is available on every supported platform, including iOS 17 and macOS 14.
public enum EmbedTranscriptRendering: Equatable {
    case automatic
    case scrollView
}

/// A styleless conversation assembly. The host supplies every row, accessory,
/// and composer; the SDK supplies scrolling and conversation-scoped view identity.
///
/// The transcript fills the available space above the accessories and composer.
/// No padding, colors, typography, buttons, connection, or submission is added.
/// A vertical reading gesture sets followsLatest to false. Set it to true to
/// return to the tail and follow subsequent row revisions, including streamed text.
///
/// Changing conversationID recreates the whole child view tree (including local
/// row/composer state and scrolling). Host-owned bindings, such as draft text and
/// followsLatest, remain host-owned and are not cleared. SwiftUI cancels child
/// .task work on removal; transport lifetime must be managed by the host.
@MainActor
public struct EmbedConversation<Accessories: View, Composer: View>: View {
    private let conversationID: String
    private let rows: [EmbedConversationRow]
    @Binding private var followsLatest: Bool
    private let layout: EmbedTranscriptLayout
    private let rendering: EmbedTranscriptRendering
    private let transcriptOverlay: () -> AnyView
    private let accessories: () -> Accessories
    private let composer: () -> Composer

    public init<Overlay: View>(conversationID: String, rows: [EmbedConversationRow],
                followsLatest: Binding<Bool>, layout: EmbedTranscriptLayout = .init(),
                rendering: EmbedTranscriptRendering = .automatic,
                @ViewBuilder transcriptOverlay: @escaping () -> Overlay,
                @ViewBuilder accessories: @escaping () -> Accessories,
                @ViewBuilder composer: @escaping () -> Composer) {
        self.conversationID = conversationID
        self.rows = rows
        _followsLatest = followsLatest
        self.layout = layout
        self.rendering = rendering
        self.transcriptOverlay = { AnyView(transcriptOverlay()) }
        self.accessories = accessories
        self.composer = composer
    }

    public init(conversationID: String, rows: [EmbedConversationRow],
                followsLatest: Binding<Bool>, layout: EmbedTranscriptLayout = .init(),
                rendering: EmbedTranscriptRendering = .automatic,
                @ViewBuilder accessories: @escaping () -> Accessories,
                @ViewBuilder composer: @escaping () -> Composer) {
        self.init(conversationID: conversationID, rows: rows, followsLatest: followsLatest,
                  layout: layout, rendering: rendering, transcriptOverlay: { EmptyView() },
                  accessories: accessories, composer: composer)
    }

    public var body: some View {
        EmbedConversationContent(rows: rows, followsLatest: $followsLatest,
                                 layout: layout, rendering: rendering,
                                 transcriptOverlay: transcriptOverlay,
                                 accessories: accessories, composer: composer)
            .id(conversationID)
    }
}

public extension EmbedConversation where Accessories == EmptyView {
    init<Overlay: View>(conversationID: String, rows: [EmbedConversationRow],
         followsLatest: Binding<Bool>, layout: EmbedTranscriptLayout = .init(),
         rendering: EmbedTranscriptRendering = .automatic,
         @ViewBuilder transcriptOverlay: @escaping () -> Overlay,
         @ViewBuilder composer: @escaping () -> Composer) {
        self.init(conversationID: conversationID, rows: rows, followsLatest: followsLatest,
                  layout: layout, rendering: rendering, transcriptOverlay: transcriptOverlay,
                  accessories: { EmptyView() }, composer: composer)
    }

    init(conversationID: String, rows: [EmbedConversationRow],
         followsLatest: Binding<Bool>, layout: EmbedTranscriptLayout = .init(),
         rendering: EmbedTranscriptRendering = .automatic,
         @ViewBuilder composer: @escaping () -> Composer) {
        self.init(conversationID: conversationID, rows: rows, followsLatest: followsLatest,
                  layout: layout, rendering: rendering, accessories: { EmptyView() }, composer: composer)
    }
}

@MainActor
private struct EmbedConversationContent<Accessories: View, Composer: View>: View {
    let rows: [EmbedConversationRow]
    @Binding var followsLatest: Bool
    let layout: EmbedTranscriptLayout
    let rendering: EmbedTranscriptRendering
    let transcriptOverlay: () -> AnyView
    let accessories: () -> Accessories
    let composer: () -> Composer
    #if os(iOS)
    @State private var scroll = EmbedScrollProxy()
    @State private var atLatest = false
    #endif

    var body: some View {
        VStack(spacing: 0) {
            transcript.frame(maxWidth: .infinity, maxHeight: .infinity)
                .overlay(alignment: .bottom) { transcriptOverlay() }
            accessories()
            composer()
        }
    }

    @ViewBuilder private var transcript: some View {
        #if os(iOS)
        if #available(iOS 18.0, *), rendering == .automatic {
            EmbedTranscript(rows: rows, proxy: scroll, followsLatest: followsLatest, layout: layout,
                            onMetrics: { metrics in
                atLatest = metrics.containerSize.height > 0 &&
                    metrics.contentSize.height - metrics.contentOffset.y - metrics.containerSize.height +
                    metrics.contentInsets.bottom <= 24
            }, onPhase: { previous, phase in
                if phase == .interacting { followsLatest = false }
                if phase == .idle, previous == .interacting || previous == .decelerating, atLatest {
                    followsLatest = true
                }
            })
        } else {
            portableTranscript
        }
        #else
        portableTranscript
        #endif
    }

    private var portableTranscript: some View {
        EmbedScrollingRows(rows: rows, followsLatest: $followsLatest, layout: layout)
    }
}

private struct EmbedRowVersion: Equatable {
    let id: String
    let revision: AnyHashable
}

@MainActor
private struct EmbedScrollingRows: View {
    let rows: [EmbedConversationRow]
    @Binding var followsLatest: Bool
    let layout: EmbedTranscriptLayout
    @State private var readingID: String?
    @State private var tailVisible = false
    @GestureState private var dragging = false
    // A distinct ID type prevents a host string ID colliding with the tail marker.
    private enum Anchor: Hashable { case tail, viewport }

    var body: some View {
        GeometryReader { viewport in
            ScrollViewReader { scroll in
                ScrollView {
                    VStack(spacing: 0) {
                        LazyVStack(spacing: layout.rowSpacing) {
                            ForEach(rows) { row in
                                EmbedPortableRow(row: row)
                                    .frame(maxWidth: layout.maximumRowWidth, alignment: .leading)
                                    .frame(maxWidth: .infinity, alignment: .center)
                                    .padding(.horizontal, layout.horizontalPadding)
                            }
                        }
                        .scrollTargetLayout()
                        .padding(.vertical, layout.verticalPadding)
                        // Outside the row stack: this marker adds no row spacing.
                        Color.clear.frame(height: 0).id(Anchor.tail)
                            .background(GeometryReader { geometry in
                                Color.clear.preference(key: EmbedTailPosition.self,
                                    value: geometry.frame(in: .named(Anchor.viewport)).maxY)
                            })
                    }
                }
                .coordinateSpace(name: Anchor.viewport)
                .scrollPosition(id: $readingID, anchor: .top)
                .scrollDismissesKeyboard(.interactively)
                .simultaneousGesture(DragGesture()
                    .updating($dragging) { value, active, _ in
                        active = abs(value.translation.height) > abs(value.translation.width)
                    }
                    .onChanged { value in
                        if abs(value.translation.height) > abs(value.translation.width) {
                            followsLatest = false
                        }
                    })
                .onPreferenceChange(EmbedTailPosition.self) { bottom in
                    guard let bottom else { return }
                    tailVisible = viewport.size.height > 0 && bottom >= 0 && bottom <= viewport.size.height + 24
                    if tailVisible && !dragging { followsLatest = true }
                }
                .onChange(of: dragging) { _, active in
                    if !active && tailVisible { followsLatest = true }
                }
                .onAppear { if followsLatest { scroll.scrollTo(Anchor.tail, anchor: .bottom) } }
                .onChange(of: followsLatest) { _, follow in
                    if follow { scroll.scrollTo(Anchor.tail, anchor: .bottom) }
                }
                .onChange(of: rows.map { EmbedRowVersion(id: $0.id, revision: $0.revision) }) { old, new in
                    if followsLatest {
                        scroll.scrollTo(Anchor.tail, anchor: .bottom)
                    } else {
                        // Retain a visible message when history is prepended. A
                        // stationary loading/header row is not a reading anchor.
                        let messages = Set(rows.filter(\.countsAsMessage).map(\.id))
                        let visible = readingID.flatMap { messages.contains($0) ? $0 : nil }
                        let anchor = visible ?? old.first(where: { messages.contains($0.id) })?.id
                        if let anchor,
                           let before = old.firstIndex(where: { $0.id == anchor }),
                           let after = new.firstIndex(where: { $0.id == anchor }), after > before {
                            scroll.scrollTo(anchor, anchor: .top)
                        }
                    }
                }
            }
        }
    }
}

private struct EmbedTailPosition: PreferenceKey {
    static let defaultValue: CGFloat? = nil
    static func reduce(value: inout CGFloat?, nextValue: () -> CGFloat?) {
        value = nextValue() ?? value
    }
}

private struct EmbedTranscriptVisibilityKey: EnvironmentKey {
    static let defaultValue = true
}

public extension EnvironmentValues {
    /// Native cells report actual display visibility. In the portable lazy stack,
    /// this reports SwiftUI appearance (which may include prefetched rows).
    var embedTranscriptVisible: Bool {
        get { self[EmbedTranscriptVisibilityKey.self] }
        set { self[EmbedTranscriptVisibilityKey.self] = newValue }
    }
}

private struct EmbedPortableRow: View {
    let row: EmbedConversationRow
    @State private var mounted = false
    var body: some View {
        row.content()
            .environment(\.embedTranscriptVisible, mounted)
            .onAppear { mounted = true }
            .onDisappear { mounted = false }
    }
}
