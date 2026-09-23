import SwiftUI
import UIKit

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
private struct NativeCellContent: View {
    let visibility: NativeCellVisibility
    let row: NativeConversationTranscript.Row
    var body: some View {
        row.content().environment(\.nativeTranscriptVisible, visibility.visible)
    }
}
private final class NativeTranscriptCell: UICollectionViewCell {
    let visibility = NativeCellVisibility()
    override func prepareForReuse() {
        super.prepareForReuse()
        visibility.visible = false
    }
}

@MainActor
final class NativeConversationScrollProxy {
    fileprivate var scroll: ((String, UnitPoint, Bool) -> Void)?

    func scrollTo(_ id: String, anchor: UnitPoint? = nil, animated: Bool = false) {
        scroll?(id, anchor ?? .top, animated)
    }
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
    var bottomInset: CGFloat
    var onFrames: ([String: CGRect]) -> Void
    var onMetrics: (NativeConversationScrollMetrics) -> Void
    var onPhase: (ScrollPhase, ScrollPhase) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> TranscriptCollectionView {
        let item = NSCollectionLayoutItem(layoutSize: .init(widthDimension: .fractionalWidth(1), heightDimension: .estimated(120)))
        let group = NSCollectionLayoutGroup.vertical(layoutSize: .init(widthDimension: .fractionalWidth(1), heightDimension: .estimated(120)), subitems: [item])
        let section = NSCollectionLayoutSection(group: group)
        section.interGroupSpacing = 18
        section.contentInsets = .init(top: 24, leading: 0, bottom: 24, trailing: 0)
        let view = TranscriptCollectionView(frame: .zero, collectionViewLayout: UICollectionViewCompositionalLayout(section: section))
        view.backgroundColor = .clear
        view.alwaysBounceVertical = true
        // Markdown can publish its measured content after a row reconfiguration.
        // Propagate those intrinsic-size changes into the native layout on iOS 18.
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
    final class Coordinator: NSObject, UICollectionViewDelegate {
        var parent: NativeConversationTranscript
        private weak var view: TranscriptCollectionView?
        private var dataSource: UICollectionViewDiffableDataSource<Int, String>!
        private var rows: [String: Row] = [:]
        private var ids: [String] = []
        private var transcriptRowCount = 0
        private var phase: ScrollPhase = .idle
        private var anchor: (id: String, offset: CGFloat)?
        private var retainedTarget: (id: String, anchor: UnitPoint)?
        private var pendingTarget: (id: String, anchor: UnitPoint, animated: Bool)?
        private var lastSize = CGSize.zero
        private var lastBounds = CGSize.zero
        private var correcting = false
        private var needsRetention = false
        private var reporting = false
        private var reportedFrames: [String: CGRect]?
        private var reportedMetrics: NativeConversationScrollMetrics?
        private var applying = false
        private var queuedUpdate: NativeConversationTranscript?
        #if DEBUG
        private let configuredCells = NSHashTable<UICollectionViewCell>.weakObjects()
        private let mountedCounter = UILabel()
        private let scrollDiagnostics = UILabel()
        #endif

        init(_ parent: NativeConversationTranscript) { self.parent = parent }

        func install(_ view: TranscriptCollectionView) {
            self.view = view
            let registration = UICollectionView.CellRegistration<NativeTranscriptCell, String> { [weak self] cell, _, id in
                guard let row = self?.rows[id] else { return }
                cell.contentConfiguration = UIHostingConfiguration {
                    NativeCellContent(visibility: cell.visibility, row: row).id(row.id).frame(maxWidth: 740, alignment: .leading)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.horizontal, 20)
                }.margins(.all, 0)
                cell.backgroundConfiguration = .clear()
                #if DEBUG
                self?.configuredCells.add(cell)
                self?.updateMountedCounter()
                #endif
            }
            dataSource = UICollectionViewDiffableDataSource<Int, String>(collectionView: view) { view, path, id in
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
            if parent.proxy !== next.proxy { parent.proxy.scroll = nil }
            if parent.followsLatest && !next.followsLatest && retainedTarget?.id == "latest" {
                retainedTarget = nil
                pendingTarget = nil
                captureAnchor()
            }
            parent = next
            parent.proxy.scroll = { [weak self] id, anchor, animated in
                self?.requestScroll(id, anchor: anchor, animated: animated)
            }
            let newIDs = next.rows.map(\.id)
            assert(Set(newIDs).count == newIDs.count, "Transcript row IDs must be unique")
            // Compare before replacing the provider's row table.
            let changedIDs = next.rows.compactMap { row -> String? in
                guard let old = rows[row.id], old.revision != row.revision else { return nil }
                return row.id
            }
            let insetChanged = view.contentInset.bottom != next.bottomInset
            let structural = ids != newIDs
            if structural || !changedIDs.isEmpty || insetChanged { needsRetention = true }
            if let saved = anchor, !newIDs.contains(saved.id) {
                let survivors = Set(newIDs)
                anchor = view.indexPathsForVisibleItems.sorted().compactMap { path -> (id: String, offset: CGFloat)? in
                    guard let id = dataSource.itemIdentifier(for: path), survivors.contains(id),
                          let frame = view.layoutAttributesForItem(at: path)?.frame else { return nil }
                    return (id, frame.minY - view.contentOffset.y)
                }.first
            }
            rows = Dictionary(uniqueKeysWithValues: next.rows.map { ($0.id, $0) })
            view.contentInset.bottom = next.bottomInset
            view.verticalScrollIndicatorInsets.bottom = next.bottomInset
            guard structural || !changedIDs.isEmpty else {
                if insetChanged { view.setNeedsLayout() }
                return
            }
            ids = newIDs
            transcriptRowCount = newIDs.filter { $0 != "latest" && $0 != "transcript-header" }.count
            var snapshot = NSDiffableDataSourceSnapshot<Int, String>()
            snapshot.appendSections([0])
            snapshot.appendItems(newIDs)
            snapshot.reconfigureItems(changedIDs)
            applying = true
            dataSource.apply(snapshot, animatingDifferences: false) { [weak self] in
                guard let self else { return }
                self.applying = false
                // Reconfigured hosting content must be measured again before
                // restoring the reading point, even when row IDs did not change.
                self.view?.collectionViewLayout.invalidateLayout()
                self.view?.layoutIfNeeded()
                self.layoutFinished()
                if let update = self.queuedUpdate {
                    self.queuedUpdate = nil
                    self.update(update)
                }
            }
        }

        private func requestScroll(_ id: String, anchor: UnitPoint, animated: Bool) {
            retainedTarget = (id, anchor)
            pendingTarget = (id, anchor, animated)
            performPendingScroll()
        }

        private func performPendingScroll() {
            guard !applying, let view, let target = pendingTarget,
                  let path = dataSource.indexPath(for: target.id),
                  let attributes = view.collectionViewLayout.layoutAttributesForItem(at: path) else { return }
            let inset = view.adjustedContentInset
            let height = view.bounds.height - inset.top - inset.bottom
            let y = attributes.frame.minY - inset.top - (height - attributes.frame.height) * target.anchor.y
            pendingTarget = nil
            correcting = true
            if !target.animated, phase == .animating { transition(.idle) }
            let moved = setOffset(y, animated: target.animated)
            if target.animated { transition(moved ? .animating : .idle) }
            if !target.animated {
                view.layoutIfNeeded()
                // Realize the destination, then align using its measured height.
                if let measured = view.collectionViewLayout.layoutAttributesForItem(at: path) {
                    setOffset(measured.frame.minY - inset.top - (height - measured.frame.height) * target.anchor.y)
                }
            }
            correcting = false
            captureAnchor()
            reportSoon()
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

        private func layoutFinished() {
            guard let view, !correcting, !applying else { return }
            correcting = true
            // UIKit may adjust contentOffset after self-sizing finishes. That is
            // not reader intent: keep the point captured by the last user scroll.
            let readingPointMoved: Bool
            if !parent.followsLatest, retainedTarget == nil, phase != .animating,
               !view.isTracking, !view.isDragging, !view.isDecelerating,
               let anchor, let path = dataSource.indexPath(for: anchor.id),
               let frame = view.layoutAttributesForItem(at: path)?.frame {
                readingPointMoved = abs(frame.minY - anchor.offset - view.contentOffset.y) > 0.5
            } else { readingPointMoved = false }
            let changed = lastSize != view.contentSize || lastBounds != view.bounds.size || needsRetention || readingPointMoved
            if changed {
                if parent.followsLatest, pendingTarget == nil,
                   !view.isTracking, !view.isDragging, !view.isDecelerating {
                    setOffset(view.contentSize.height - view.bounds.height + view.adjustedContentInset.bottom)
                } else if pendingTarget == nil, phase != .animating, let target = retainedTarget,
                          let path = dataSource.indexPath(for: target.id),
                          let attributes = view.collectionViewLayout.layoutAttributesForItem(at: path) {
                    let inset = view.adjustedContentInset
                    let height = view.bounds.height - inset.top - inset.bottom
                    setOffset(attributes.frame.minY - inset.top - (height - attributes.frame.height) * target.anchor.y)
                } else if pendingTarget == nil, phase != .animating, let anchor,
                          let path = dataSource.indexPath(for: anchor.id),
                          let attributes = view.collectionViewLayout.layoutAttributesForItem(at: path) {
                    setOffset(attributes.frame.minY - anchor.offset)
                }
            }
            lastSize = view.contentSize
            lastBounds = view.bounds.size
            needsRetention = false
            if changed { view.layoutIfNeeded() }
            correcting = false
            if pendingTarget != nil { performPendingScroll() }
            if anchor == nil { captureAnchor() }
            reportSoon()
        }

        private func captureAnchor() {
            guard let view else { return }
            let candidates = view.indexPathsForVisibleItems.compactMap { path -> (String, CGRect)? in
                guard let id = dataSource.itemIdentifier(for: path),
                      let frame = view.layoutAttributesForItem(at: path)?.frame else { return nil }
                return (id, frame)
            }.sorted { $0.1.minY < $1.1.minY }
            if let first = candidates.first(where: { $0.1.maxY > view.contentOffset.y }) {
                anchor = (first.0, first.1.minY - view.contentOffset.y)
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
            let anchorFrame = anchor.flatMap { value in dataSource.indexPath(for: value.id).flatMap { view.layoutAttributesForItem(at: $0)?.frame } }
            scrollDiagnostics.accessibilityLabel = "offset=\(view.contentOffset.y) size=\(view.contentSize.height) anchor=\(String(describing: anchor)) frame=\(String(describing: anchorFrame)) following=\(parent.followsLatest) phase=\(phase)"
            scrollDiagnostics.frame = mountedCounter.frame
            view.bringSubviewToFront(scrollDiagnostics)
        }
        #endif

        private func reportSoon() {
            #if DEBUG
            updateMountedCounter()
            #endif
            guard !reporting else { return }
            reporting = true
            DispatchQueue.main.async { [weak self] in
                guard let self, let view = self.view else { return }
                self.reporting = false
                var frames: [String: CGRect] = [:]
                for path in view.indexPathsForVisibleItems {
                    guard let id = self.dataSource.itemIdentifier(for: path),
                          let frame = view.layoutAttributesForItem(at: path)?.frame else { continue }
                    frames[id] = frame.offsetBy(dx: -view.contentOffset.x, dy: -view.contentOffset.y)
                }
                if self.reportedFrames != frames {
                    self.reportedFrames = frames
                    self.parent.onFrames(frames)
                }
                let metrics = NativeConversationScrollMetrics(contentOffset: view.contentOffset, contentSize: view.contentSize,
                                                                containerSize: view.bounds.size, contentInsets: view.adjustedContentInset)
                if self.reportedMetrics != metrics {
                    self.reportedMetrics = metrics
                    self.parent.onMetrics(metrics)
                }
            }
        }

        private func transition(_ next: ScrollPhase) {
            guard phase != next else { return }
            let previous = phase
            phase = next
            parent.onPhase(previous, next)
        }

        func collectionView(_ collectionView: UICollectionView, willDisplay cell: UICollectionViewCell, forItemAt indexPath: IndexPath) {
            (cell as? NativeTranscriptCell)?.visibility.visible = true
        }
        func collectionView(_ collectionView: UICollectionView, didEndDisplaying cell: UICollectionViewCell, forItemAt indexPath: IndexPath) {
            (cell as? NativeTranscriptCell)?.visibility.visible = false
        }

        func scrollViewDidScroll(_ scrollView: UIScrollView) {
            if scrollView.isDragging { transition(.interacting) }
            if !correcting, !applying, !needsRetention,
               lastSize == scrollView.contentSize, lastBounds == scrollView.bounds.size {
                if scrollView.isTracking || scrollView.isDragging || scrollView.isDecelerating {
                    captureAnchor()
                } else if phase != .animating, !parent.followsLatest {
                    view?.setNeedsLayout()
                }
            }
            reportSoon()
        }
        func scrollViewWillBeginDragging(_ scrollView: UIScrollView) {
            pendingTarget = nil
            retainedTarget = nil
            transition(.tracking)
        }
        func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate decelerate: Bool) {
            transition(decelerate ? .decelerating : .idle)
        }
        func scrollViewDidEndDecelerating(_ scrollView: UIScrollView) { transition(.idle) }
        func scrollViewDidEndScrollingAnimation(_ scrollView: UIScrollView) {
            transition(.idle)
            needsRetention = true
            layoutFinished()
        }
    }
}
