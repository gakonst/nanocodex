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
    override func prepareForReuse() {
        super.prepareForReuse()
        visibility.visible = false
    }
}

@MainActor
final class NativeConversationScrollProxy {
    fileprivate var scroll: ((String, CGFloat) -> Void)?
    fileprivate var follow: ((Bool) -> Void)?

    // Reading positions are exact viewport points, never fractions of a
    // self-sizing row or the keyboard-dependent viewport height.
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
    final class Coordinator: NSObject, UICollectionViewDelegate {
        var parent: NativeConversationTranscript
        private weak var view: TranscriptCollectionView?
        private var dataSource: UICollectionViewDiffableDataSource<Int, String>!
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
        private var correcting = false
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
                guard let hosted = self?.hostedRows[id] else { return }
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
            parent.proxy.scroll = { [weak self] id, offset in
                self?.requestScroll(id, offset: offset)
            }
            parent.proxy.follow = { [weak self] animated in self?.requestFollowLatest(animated: animated) }
            let newIDs = next.rows.map(\.id)
            assert(Set(newIDs).count == newIDs.count, "Transcript row IDs must be unique")
            let insetChanged = view.contentInset.bottom != next.bottomInset
            let structural = ids != newIDs
            let contentChanged = next.rows.contains { hostedRows[$0.id]?.revision != $0.revision }
            if structural { retainSurvivingReadingPoint(in: Set(newIDs)) }
            // Update observable models in place. Only insertion/removal/reorder
            // goes through diffable; self-sizing tracks visible content changes.
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
            }
            view.contentInset.bottom = next.bottomInset
            view.verticalScrollIndicatorInsets.bottom = next.bottomInset
            if !structural {
                if contentChanged || insetChanged { view.setNeedsLayout() }
                return
            }
            ids = newIDs
            transcriptRowCount = newIDs.filter { $0 != "latest" && $0 != "transcript-header" }.count
            var snapshot = NSDiffableDataSourceSnapshot<Int, String>()
            snapshot.appendSections([0])
            snapshot.appendItems(newIDs)
            applying = true
            dataSource.apply(snapshot, animatingDifferences: false) { [weak self] in
                guard let self else { return }
                self.applying = false
                self.view?.layoutIfNeeded()
                self.layoutFinished()
                if let update = self.queuedUpdate {
                    self.queuedUpdate = nil
                    self.update(update)
                }
            }
        }

        private func requestFollowLatest(animated: Bool) {
            viewport = .following
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

        private func targetOffset(_ id: String, offset: CGFloat, in view: TranscriptCollectionView) -> CGFloat? {
            guard let path = dataSource.indexPath(for: id),
                  let frame = view.collectionViewLayout.layoutAttributesForItem(at: path)?.frame else { return nil }
            return frame.minY - offset
        }

        private func layoutFinished() {
            guard let view, !correcting, !applying else { return }
            correcting = true
            switch viewport {
            case .following:
                if phase != .animating, !view.isTracking, !view.isDragging, !view.isDecelerating {
                    setOffset(view.contentSize.height - view.bounds.height + view.adjustedContentInset.bottom)
                }
            case let .reading(id, offset):
                if phase != .animating, !view.isTracking, !view.isDragging, !view.isDecelerating,
                   let path = dataSource.indexPath(for: id),
                   let frame = view.collectionViewLayout.layoutAttributesForItem(at: path)?.frame {
                    setOffset(frame.minY - offset)
                }
            case let .target(id, offset, pending):
                if pending, let y = targetOffset(id, offset: offset, in: view) {
                    viewport = .target(id: id, offset: offset, pending: false)
                    if phase == .animating { transition(.idle) }
                    setOffset(y)
                    view.layoutIfNeeded()
                    // Estimated heights can change when the destination is
                    // realized. Align it once more at its measured height.
                    if let measured = targetOffset(id, offset: offset, in: view) { setOffset(measured) }
                } else if !pending, phase != .animating, !view.isTracking, !view.isDragging, !view.isDecelerating,
                          let y = targetOffset(id, offset: offset, in: view) {
                    setOffset(y)
                }
            }
            correcting = false
            reportSoon()
        }

        private func visibleReadingPoint(among survivors: Set<String>? = nil) -> (id: String, offset: CGFloat)? {
            guard let view else { return nil }
            return view.indexPathsForVisibleItems.compactMap { path -> (id: String, frame: CGRect)? in
                guard let id = dataSource.itemIdentifier(for: path),
                      survivors?.contains(id) ?? true,
                      let frame = view.layoutAttributesForItem(at: path)?.frame,
                      frame.maxY > view.contentOffset.y else { return nil }
                return (id, frame)
            }.min(by: { $0.frame.minY < $1.frame.minY }).map { ($0.id, $0.frame.minY - view.contentOffset.y) }
        }

        private func captureReadingPoint() {
            if let point = visibleReadingPoint() { viewport = .reading(id: point.id, offset: point.offset) }
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
            captureReadingPoint()
            transition(.tracking)
        }
        func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate decelerate: Bool) {
            transition(decelerate ? .decelerating : .idle)
        }
        func scrollViewDidEndDecelerating(_ scrollView: UIScrollView) { transition(.idle) }
        func scrollViewDidEndScrollingAnimation(_ scrollView: UIScrollView) {
            transition(.idle)
            layoutFinished()
        }
    }
}
