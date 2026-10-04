#if os(iOS)
import SwiftUI
import UIKit

/// A virtualized native conversation surface with host-owned rows and transport.
///
/// Keep IDs stable and include every rendering input in the row revision. A text
/// delta updates the existing hosted row; it does not reset Markdown or selection.
/// Use a separate proxy and view identity for each conversation. Insets reserve
/// space for the host's floating header and composer without covering scrollback.
@available(iOS 18.0, *)
@MainActor
public struct EmbedTranscript: View {
    public typealias Row = EmbedConversationRow

    private let rows: [Row]
    private let proxy: EmbedScrollProxy
    private let followsLatest: Bool
    private let topInset: CGFloat
    private let bottomInset: CGFloat
    private let layout: EmbedTranscriptLayout
    private let onFrames: ([String: CGRect]) -> Void
    private let onMetrics: (EmbedScrollMetrics) -> Void
    private let onPhase: (ScrollPhase, ScrollPhase) -> Void

    public init(rows: [Row], proxy: EmbedScrollProxy, followsLatest: Bool,
                topInset: CGFloat = 0, bottomInset: CGFloat = 0,
                layout: EmbedTranscriptLayout = .init(),
                onFrames: @escaping ([String: CGRect]) -> Void = { _ in },
                onMetrics: @escaping (EmbedScrollMetrics) -> Void = { _ in },
                onPhase: @escaping (ScrollPhase, ScrollPhase) -> Void = { _, _ in }) {
        self.rows = rows
        self.proxy = proxy
        self.followsLatest = followsLatest
        self.topInset = topInset
        self.bottomInset = bottomInset
        self.layout = layout
        self.onFrames = onFrames
        self.onMetrics = onMetrics
        self.onPhase = onPhase
    }

    public var body: some View {
        NativeConversationTranscript(layout: layout, rows: rows, proxy: proxy, followsLatest: followsLatest,
                                     topInset: topInset, bottomInset: bottomInset,
                                     onFrames: onFrames, onMetrics: onMetrics, onPhase: onPhase)
    }
}
#endif
