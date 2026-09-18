import XCTest
@testable import NanocodexRemote

final class RemoteCanvasFitTests: XCTestCase {
    func testWholeFrameFitsAfterResize() {
        for surface in [CGSize(width: 1920, height: 1200), CGSize(width: 1080, height: 1920), CGSize(width: 3024, height: 1964)] {
            for bounds in [CGRect(x: 0, y: 0, width: 1200, height: 700), CGRect(x: 0, y: 0, width: 800, height: 1000)] {
                let rect = fitted(surface, in: bounds)
                XCTAssertGreaterThanOrEqual(rect.minX, bounds.minX - 0.001)
                XCTAssertGreaterThanOrEqual(rect.minY, bounds.minY - 0.001)
                XCTAssertLessThanOrEqual(rect.maxX, bounds.maxX + 0.001)
                XCTAssertLessThanOrEqual(rect.maxY, bounds.maxY + 0.001)
                XCTAssertEqual(rect.width / rect.height, surface.width / surface.height, accuracy: 0.0001)
            }
        }
    }
    #if os(macOS)
    @MainActor func testViewportResizesToPhysicalClipFrame() {
        let viewport = MacRemoteViewport(viewer: RemoteViewer())
        for size in [CGSize(width: 1200, height: 800), CGSize(width: 700, height: 350), CGSize(width: 1500, height: 1000)] {
            viewport.frame = CGRect(origin: .zero, size: size)
            viewport.layoutSubtreeIfNeeded()
            XCTAssertEqual(viewport.canvas.frame.size, viewport.contentView.frame.size)
            XCTAssertEqual(viewport.magnification, 1)
        }
    }
    #endif
}
