import Combine
import CoreGraphics

/// Pixel delivery is independent of ObservableObject invalidation. Native video
/// surfaces subscribe directly; headers, menus and chat do not rebuild per frame.
@MainActor @propertyWrapper
public struct RemoteFramePublication {
    private let subject: CurrentValueSubject<CGImage?, Never>
    public init(wrappedValue: CGImage?) { subject = .init(wrappedValue) }
    public var wrappedValue: CGImage? {
        get { subject.value }
        nonmutating set { subject.send(newValue) }
    }
    public var projectedValue: AnyPublisher<CGImage?, Never> { subject.eraseToAnyPublisher() }
}
