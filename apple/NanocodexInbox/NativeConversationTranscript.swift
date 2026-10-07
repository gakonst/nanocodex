import SwiftUI
import UIKit
import ChatLayout

private struct NativeTranscriptVisibilityKey: EnvironmentKey {
    static let defaultValue = true
}
extension EnvironmentValues {
    var nativeTranscriptVisible: Bool {
        get { self[NativeTranscriptVisibilityKey.self] }
        set { self[NativeTranscriptVisibilityKey.self] = newValue }
    }
}
@Observable private final class NativeCellVisibility {
    var visible = false
}
// Diffable snapshots own identity and order. A stable observed row owns its
// changing SwiftUI content, so streamed text does not reinstall the hosting
// configuration (and reset its Markdown/rendering state) on every delta.
@MainActor @Observable private final class NativeHostedRow {
    @ObservationIgnored var revision: AnyHashable
    var content: () -> AnyView
    init(_ row: NativeConversationTranscript.Row) {
        revision = row.revision
        content = row.content
    }
}
private struct NativeCellContent: View {
    let visibility: NativeCellVisibility
    let hosted: NativeHostedRow
    var body: some View {
        hosted.content().environment(\.nativeTranscriptVisible, visibility.visible)
    }
}
private final class NativeTranscriptCell: UICollectionViewCell {
    let visibility = NativeCellVisibility()
    var rowID: String?
    // ChatLayout proposes an estimated height. Measure the hosting content at
    // its final full width instead of accepting that estimate as a constraint.
    override func preferredLayoutAttributesFitting(_ attributes: UICollectionViewLayoutAttributes) -> UICollectionViewLayoutAttributes {
        let result = attributes.copy() as! UICollectionViewLayoutAttributes
        let width = attributes.size.width
        contentView.bounds.size.width = width
        let size = contentView.systemLayoutSizeFitting(
            CGSize(width: width, height: UIView.layoutFittingCompressedSize.height),
            withHorizontalFittingPriority: .required, verticalFittingPriority: .fittingSizeLevel)
        result.size = CGSize(width: width, height: max(1, ceil(size.height)))
        return result
    }
    // Keep the hosted content rendered for glass sampling, but do not expose
    // a cell entirely behind the header or dock to touch or accessibility navigation.
    func setUnobscured(_ unobscured: Bool) {
        accessibilityElementsHidden = !unobscured
        isUserInteractionEnabled = unobscured
    }
    override func prepareForReuse() {
        super.prepareForReuse()
        rowID = nil
        visibility.visible = false
        setUnobscured(false)
    }
}

@MainActor
final class NativeConversationScrollProxy {
    fileprivate var scroll: ((String, CGFloat) -> Void)?
    fileprivate var follow: ((Bool) -> Void)?

    // Reading positions are exact points below the top chrome, never fractions
    // of a self-sizing row or the keyboard-dependent viewport height.
    func scrollTo(_ id: String, topOffset: CGFloat = 0) { scroll?(id, topOffset) }
    func followLatest(animated: Bool = false) { follow?(animated) }
}

struct NativeConversationScrollMetrics: Equatable {
    var contentOffset: CGPoint
    var contentSize: CGSize
    var containerSize: CGSize
    var contentInsets: UIEdgeInsets
}

/// Hosts only the collection view's working set. A row revision must include all
/// inputs that affect its content; unchanged IDs retain their hosting state.
struct NativeConversationTranscript: UIViewRepresentable {
    struct Row {
        var id: String
        var revision: AnyHashable
        var content: () -> AnyView
    }

    var rows: [Row]
    var proxy: NativeConversationScrollProxy
    var followsLatest: Bool
    var topInset: CGFloat = 0
    var bottomInset: CGFloat
    var onFrames: ([String: CGRect]) -> Void
    var onMetrics: (NativeConversationScrollMetrics) -> Void
    var onPhase: (ScrollPhase, ScrollPhase) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> TranscriptCollectionView {
        let layout = context.coordinator.chatLayout
        layout.settings.estimatedItemSize = CGSize(width: 320, height: 120)
        layout.settings.interItemSpacing = 18
        layout.settings.additionalInsets = UIEdgeInsets(top: 24, left: 0, bottom: 24, right: 0)
        layout.supportSelfSizingInvalidation = true
        layout.delegate = context.coordinator
        let view = TranscriptCollectionView(frame: .zero, collectionViewLayout: layout)
        view.backgroundColor = .clear
        view.alwaysBounceVertical = true
        // Observed hosted rows can change height without a diffable snapshot.
        view.selfSizingInvalidation = .enabledIncludingConstraints
        view.keyboardDismissMode = .interactive
        view.contentInsetAdjustmentBehavior = .never
        view.delegate = context.coordinator
        context.coordinator.install(view)
        return view
    }

    func updateUIView(_ view: TranscriptCollectionView, context: Context) {
        context.coordinator.update(self)
    }

    static func dismantleUIView(_ view: TranscriptCollectionView, coordinator: Coordinator) {
        coordinator.parent.proxy.scroll = nil
        coordinator.parent.proxy.follow = nil
        view.didLayout = nil
        view.delegate = nil
    }

    final class TranscriptCollectionView: UICollectionView {
        var didLayout: (() -> Void)?
        override func layoutSubviews() {
            super.layoutSubviews()
            didLayout?()
        }
    }

    @MainActor
    final class Coordinator: NSObject, UICollectionViewDelegate, ChatLayoutDelegate {
        var parent: NativeConversationTranscript
        private weak var view: TranscriptCollectionView?
        let chatLayout = CollectionViewChatLayout()
        private var dataSource: ChatLayoutDiffableDataSource<Int, String>!
        private var hostedRows: [String: NativeHostedRow] = [:]
        private var ids: [String] = []
        private var transcriptRowCount = 0
        private var phase: ScrollPhase = .idle
        // One owner for scroll intent. UIKit may change offsets while a hosted
        // row self-sizes, but that must not become a second reading position.
        private enum Viewport {
            case following
            case reading(id: String, offset: CGFloat)
            case target(id: String, offset: CGFloat, pending: Bool)
        }
        private var viewport: Viewport = .following
        private var laidOutSize: CGSize = .zero
        private var laidOutInsets: UIEdgeInsets = .zero
        private var correcting = false
        private var reporting = false
        private var reportedFrames: [String: CGRect]?
        private var reportedMetrics: NativeConversationScrollMetrics?
        private var applying = false
        private var userScrollRevision: UInt64 = 0
        private var queuedUpdate: NativeConversationTranscript?
        // Last measured height per row at a given width. Rows that are re-realized
        // after a snapshot or reuse start at their real height instead of the
        // generic estimate, so scrolling back through tall answers (artifacts,
        // tables, tool output) does not shift the content under the finger.
        private var measuredHeights: [String: (width: CGFloat, height: CGFloat)] = [:]
        #if DEBUG
        private let configuredCells = NSHashTable<UICollectionViewCell>.weakObjects()
        private let mountedCounter = UILabel()
        private let scrollDiagnostics = UILabel()
        #endif

        init(_ parent: NativeConversationTranscript) { self.parent = parent }

        func install(_ view: TranscriptCollectionView) {
            self.view = view
            let registration = UICollectionView.CellRegistration<NativeTranscriptCell, String> { [weak self] cell, _, id in
                guard let hosted = self?.hostedRows[id] else { return }
                cell.rowID = id
                cell.contentConfiguration = UIHostingConfiguration {
                    NativeCellContent(visibility: cell.visibility, hosted: hosted).id(id).frame(maxWidth: 740, alignment: .leading)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.horizontal, 20)
                }.margins(.all, 0)
                cell.backgroundConfiguration = .clear()
                #if DEBUG
                self?.configuredCells.add(cell)
                self?.updateMountedCounter()
                #endif
            }
            dataSource = ChatLayoutDiffableDataSource<Int, String>(collectionView: view) { view, path, id in
                view.dequeueConfiguredReusableCell(using: registration, for: path, item: id)
            }
            #if DEBUG
            if ProcessInfo.processInfo.environment["NANOCODEX_RENDER_COUNTER"] == "1" {
                mountedCounter.isAccessibilityElement = true
                mountedCounter.accessibilityIdentifier = "conversation-native-mounted-count"
                mountedCounter.textColor = .clear
                mountedCounter.isUserInteractionEnabled = false
                view.addSubview(mountedCounter)
                scrollDiagnostics.isAccessibilityElement = true
                scrollDiagnostics.accessibilityIdentifier = "conversation-native-scroll-state"
                scrollDiagnostics.textColor = .clear
                scrollDiagnostics.isUserInteractionEnabled = false
                view.addSubview(scrollDiagnostics)
            }
            #endif
            view.didLayout = { [weak self] in self?.layoutFinished() }
            update(parent)
        }

        func update(_ next: NativeConversationTranscript) {
            guard let view else { return }
            if applying { queuedUpdate = next; return }
            if parent.proxy !== next.proxy {
                parent.proxy.scroll = nil
                parent.proxy.follow = nil
            }
            if parent.followsLatest && !next.followsLatest {
                // A drag or tool expansion stops following at the actual visible
                // point, not at the previously requested tail target.
                if case .following = viewport { captureReadingPoint() }
            } else if !parent.followsLatest && next.followsLatest {
                viewport = .following
            }
            parent = next
            updateBottomAnchoring()
            parent.proxy.scroll = { [weak self] id, offset in
                self?.requestScroll(id, offset: offset)
            }
            parent.proxy.follow = { [weak self] animated in self?.requestFollowLatest(animated: animated) }
            let newIDs = next.rows.map(\.id)
            assert(Set(newIDs).count == newIDs.count, "Transcript row IDs must be unique")
            let insetChanged = view.contentInset.top != next.topInset || view.contentInset.bottom != next.bottomInset
            let structural = ids != newIDs
            let contentChanged = next.rows.contains { hostedRows[$0.id]?.revision != $0.revision }
            if structural {
                // A gesture can settle after its last scrolling callback. Keep
                // the currently visible position when history is prepended,
                // rather than restoring an earlier overscroll/bounce offset.
                if case .reading = viewport,
                   !view.isTracking, !view.isDragging, !view.isDecelerating {
                    captureReadingPoint()
                }
                retainSurvivingReadingPoint(in: Set(newIDs))
            }
            // Commit row models at the data source transaction boundary, keeping
            // cell providers and layout delegates on the same snapshot.
            let commitRows = { [self] in
                for row in next.rows {
                    if let hosted = hostedRows[row.id] {
                        if hosted.revision != row.revision {
                            hosted.revision = row.revision
                            hosted.content = row.content
                        }
                    } else { hostedRows[row.id] = NativeHostedRow(row) }
                }
                if structural {
                    let survivors = Set(newIDs)
                    hostedRows = hostedRows.filter { survivors.contains($0.key) }
                    measuredHeights = measuredHeights.filter { survivors.contains($0.key) }
                    ids = newIDs
                    transcriptRowCount = newIDs.filter { $0 != "latest" && $0 != "transcript-header" }.count
                }
            }
            view.contentInset.top = next.topInset
            view.contentInset.bottom = next.bottomInset
            view.verticalScrollIndicatorInsets.top = next.topInset
            view.verticalScrollIndicatorInsets.bottom = next.bottomInset
            if !structural {
                commitRows()
                if contentChanged || insetChanged { view.setNeedsLayout() }
                return
            }
            var snapshot = NSDiffableDataSourceSnapshot<Int, String>()
            snapshot.appendSections([0])
            snapshot.appendItems(newIDs)
            applying = true
            let scrollRevision = userScrollRevision
            dataSource.apply(snapshot, animatingDifferences: false, commitAlongsideUpdates: commitRows) { [weak self] in
                guard let self else { return }
                self.view?.layoutIfNeeded()
                self.applying = false
                // A snapshot may finish after the reader has started another
                // swipe. Never restore its old anchor into an active gesture.
                if scrollRevision != self.userScrollRevision {
                    // The swipe may already have ended while the snapshot was
                    // applying. Adopt its current point, not the pre-update one.
                    if case .reading = self.viewport { self.captureReadingPoint() }
                } else if !self.isUserScrolling {
                    switch self.viewport {
                    case let .reading(id, offset), let .target(id, offset, false):
                        self.restore(id, offset: offset)
                    default: break
                    }
                }
                self.layoutFinished()
                if let update = self.queuedUpdate {
                    self.queuedUpdate = nil
                    self.update(update)
                }
            }
        }

        private var isUserScrolling: Bool {
            guard let view else { return false }
            return view.isTracking || view.isDragging || view.isDecelerating
        }

        private func updateBottomAnchoring() {
            // Native gesture intent takes effect before SwiftUI publishes its
            // next update. A streaming snapshot must not pin a drag to the tail.
            if case .following = viewport {
                chatLayout.keepContentOffsetAtBottomOnBatchUpdates = !isUserScrolling
            } else {
                chatLayout.keepContentOffsetAtBottomOnBatchUpdates = false
            }
        }

        private func requestFollowLatest(animated: Bool) {
            viewport = .following
            updateBottomAnchoring()
            guard animated, let view, !applying, !correcting else { layoutFinished(); return }
            correcting = true
            let moved = setOffset(view.contentSize.height - view.bounds.height + view.adjustedContentInset.bottom,
                                  animated: true)
            transition(moved ? .animating : .idle)
            correcting = false
            reportSoon()
        }

        private func requestScroll(_ id: String, offset: CGFloat) {
            viewport = .target(id: id, offset: offset, pending: true)
            updateBottomAnchoring()
            layoutFinished()
        }

        @discardableResult
        private func setOffset(_ y: CGFloat, animated: Bool = false) -> Bool {
            guard let view else { return false }
            let minimum = -view.adjustedContentInset.top
            let maximum = max(minimum, view.contentSize.height - view.bounds.height + view.adjustedContentInset.bottom)
            let target = min(maximum, max(minimum, y))
            if abs(view.contentOffset.y - target) > 0.25 {
                view.setContentOffset(CGPoint(x: 0, y: target), animated: animated)
                return true
            }
            return false
        }

        private func restore(_ id: String, offset: CGFloat) {
            guard let path = dataSource.indexPath(for: id) else { return }
            // ChatLayout already subtracts adjustedContentInset.top when restoring
            // a top snapshot, so offset stays relative to the usable top boundary.
            chatLayout.restoreContentOffset(with: ChatLayoutPositionSnapshot(
                indexPath: path, edge: .top, offset: offset - chatLayout.settings.additionalInsets.top))
        }

        private func layoutFinished() {
            guard let view, !correcting, !applying else { return }
            let viewportChanged = laidOutSize != view.bounds.size || laidOutInsets != view.adjustedContentInset
            laidOutSize = view.bounds.size
            laidOutInsets = view.adjustedContentInset
            correcting = true
            switch viewport {
            case .following:
                if phase != .animating, !view.isTracking, !view.isDragging, !view.isDecelerating,
                   let id = ids.last, let path = dataSource.indexPath(for: id),
                   abs(view.contentOffset.y - max(-view.adjustedContentInset.top,
                       view.contentSize.height - view.bounds.height + view.adjustedContentInset.bottom)) > 0.5 {
                    chatLayout.restoreContentOffset(with: ChatLayoutPositionSnapshot(indexPath: path, edge: .bottom))
                }
            case let .reading(id, offset):
                // Header/keyboard/composer resize is outside ChatLayout's batch update.
                // Restore only at that boundary; self-sizing keeps its own anchor.
                if viewportChanged, !view.isTracking, !view.isDragging, !view.isDecelerating {
                    restore(id, offset: offset)
                }
            case let .target(id, offset, pending):
                if pending || viewportChanged, dataSource.indexPath(for: id) != nil {
                    viewport = .target(id: id, offset: offset, pending: false)
                    restore(id, offset: offset)
                }
            }
            correcting = false
            reportSoon()
        }

        private func visibleReadingPoint(among survivors: Set<String>? = nil) -> (id: String, offset: CGFloat)? {
            guard let view else { return nil }
            let usableTop = view.contentOffset.y + view.adjustedContentInset.top
            return view.indexPathsForVisibleItems.compactMap { path -> (id: String, frame: CGRect)? in
                guard let id = dataSource.itemIdentifier(for: path),
                      survivors?.contains(id) ?? true,
                      let frame = view.layoutAttributesForItem(at: path)?.frame,
                      frame.maxY > usableTop else { return nil }
                return (id, frame)
            }.min(by: { $0.frame.minY < $1.frame.minY }).map { ($0.id, $0.frame.minY - usableTop) }
        }

        private func captureReadingPoint() {
            if let point = visibleReadingPoint() { viewport = .reading(id: point.id, offset: point.offset) }
            updateBottomAnchoring()
        }

        private func retainSurvivingReadingPoint(in survivors: Set<String>) {
            let current: String?
            switch viewport {
            case .following: return
            case .reading(let id, _), .target(let id, _, _): current = id
            }
            guard let current, !survivors.contains(current) else { return }
            if let point = visibleReadingPoint(among: survivors) {
                viewport = .reading(id: point.id, offset: point.offset)
            } else if parent.followsLatest {
                viewport = .following
            } else if let index = ids.firstIndex(of: current),
                      let nearest = ids[index...].first(where: { survivors.contains($0) })
                        ?? ids[..<index].reversed().first(where: { survivors.contains($0) }) {
                // The entire visible page was trimmed. Retarget to the nearest
                // surviving row rather than holding an impossible identity.
                viewport = .target(id: nearest, offset: 0, pending: true)
            }
        }

        #if DEBUG
        private func updateMountedCounter() {
            guard let view, mountedCounter.superview != nil else { return }
            // Weak cell identities include live offscreen cells in UIKit's reuse
            // pool. Unlike visibleCells, this measures retained hosted configs.
            mountedCounter.accessibilityLabel = String(configuredCells.allObjects.filter { $0.contentConfiguration != nil }.count)
            mountedCounter.accessibilityValue = String(transcriptRowCount)
            mountedCounter.frame = CGRect(x: view.contentOffset.x, y: view.contentOffset.y, width: 1, height: 1)
            view.bringSubviewToFront(mountedCounter)
            scrollDiagnostics.accessibilityLabel = "offset=\(view.contentOffset.y) size=\(view.contentSize.height) viewport=\(viewport) phase=\(phase)"
            scrollDiagnostics.frame = mountedCounter.frame
            view.bringSubviewToFront(scrollDiagnostics)
        }
        #endif

        private func isUnobscured(_ frame: CGRect, in view: UICollectionView) -> Bool {
            let top = view.bounds.minY + max(0, parent.topInset)
            let bottom = view.bounds.maxY - max(0, parent.bottomInset)
            return bottom > top && frame.maxY > top && frame.minY < bottom
        }

        private func reportSoon() {
            // Update synchronously during layout/scrolling so a newly covered
            // cell cannot remain interactive until the deferred frame report.
            if let view {
                for case let cell as NativeTranscriptCell in view.visibleCells {
                    cell.setUnobscured(isUnobscured(cell.frame, in: view))
                }
            }
            #if DEBUG
            updateMountedCounter()
            #endif
            guard !reporting else { return }
            reporting = true
            DispatchQueue.main.async { [weak self] in
                guard let self, let view = self.view else { return }
                self.reporting = false
                // Completion reports the settled snapshot; transient frames can
                // otherwise overwrite the reader's saved position mid-update.
                guard !self.applying, !self.correcting else { return }
                var frames: [String: CGRect] = [:]
                for path in view.indexPathsForVisibleItems {
                    guard let id = self.dataSource.itemIdentifier(for: path),
                          let frame = view.layoutAttributesForItem(at: path)?.frame,
                          self.isUnobscured(frame, in: view) else { continue }
                    // Match scrollTo(topOffset:) and reading anchors so callers
                    // can save and restore these frames without adding chrome twice.
                    frames[id] = frame.offsetBy(dx: -view.contentOffset.x,
                                                dy: -view.contentOffset.y - view.adjustedContentInset.top)
                }
                if self.reportedFrames != frames {
                    self.reportedFrames = frames
                    self.parent.onFrames(frames)
                }
                self.reportMetrics()
            }
        }

        private func reportMetrics() {
            guard let view else { return }
            let metrics = NativeConversationScrollMetrics(contentOffset: view.contentOffset, contentSize: view.contentSize,
                                                          containerSize: view.bounds.size, contentInsets: view.adjustedContentInset)
            if reportedMetrics != metrics {
                reportedMetrics = metrics
                parent.onMetrics(metrics)
            }
        }

        private func transition(_ next: ScrollPhase) {
            guard phase != next else { return }
            let previous = phase
            phase = next
            parent.onPhase(previous, next)
        }

        func collectionView(_ collectionView: UICollectionView, willDisplay cell: UICollectionViewCell, forItemAt indexPath: IndexPath) {
            guard let cell = cell as? NativeTranscriptCell else { return }
            cell.visibility.visible = true
            cell.setUnobscured(isUnobscured(cell.frame, in: collectionView))
        }
        func sizeForItem(_ chatLayout: CollectionViewChatLayout, at indexPath: IndexPath) -> ItemSize {
            guard let id = dataSource.itemIdentifier(for: indexPath), let measured = measuredHeights[id],
                  let width = view?.bounds.width, abs(measured.width - width) < 0.5 else { return .auto }
            return .estimated(CGSize(width: width, height: measured.height))
        }

        func collectionView(_ collectionView: UICollectionView, didEndDisplaying cell: UICollectionViewCell, forItemAt indexPath: IndexPath) {
            guard let cell = cell as? NativeTranscriptCell else { return }
            // The index path can already describe a newer snapshot; the cell's
            // own identity is the measurement owner.
            if let id = cell.rowID, cell.bounds.height > 1 {
                measuredHeights[id] = (collectionView.bounds.width, cell.bounds.height)
            }
            cell.visibility.visible = false
            cell.setUnobscured(false)
        }

        func scrollViewDidScroll(_ scrollView: UIScrollView) {
            if !correcting, scrollView.isDragging || scrollView.isDecelerating { userScrollRevision &+= 1 }
            if scrollView.isDragging { transition(.interacting) }
            if !correcting, !applying {
                if scrollView.isTracking || scrollView.isDragging {
                    captureReadingPoint()
                } else if scrollView.isDecelerating {
                    // A Latest tap can supersede a still-settling drag. Do not
                    // turn that old inertia back into a new reading intent.
                    if case .following = viewport { } else { captureReadingPoint() }
                } else if phase != .animating {
                    view?.setNeedsLayout() // Reconcile a UIKit self-sizing offset adjustment.
                }
            }
            reportSoon()
        }
        func scrollViewWillBeginDragging(_ scrollView: UIScrollView) {
            userScrollRevision &+= 1
            captureReadingPoint()
            transition(.tracking)
        }
        func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate decelerate: Bool) {
            // Flush while the phase still describes the drag. The coalesced
            // layout report may otherwise arrive after idle has resumed following
            // using stale atLatest geometry, snapping a quick swipe to the bottom.
            reportMetrics()
            transition(decelerate ? .decelerating : .idle)
            updateBottomAnchoring()
        }
        func scrollViewDidEndDecelerating(_ scrollView: UIScrollView) {
            reportMetrics()
            transition(.idle)
            updateBottomAnchoring()
        }
        func scrollViewDidEndScrollingAnimation(_ scrollView: UIScrollView) {
            transition(.idle)
            layoutFinished()
        }
    }
}
