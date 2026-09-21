#if os(iOS)
import XCTest
import UIKit
import UniformTypeIdentifiers
@testable import NanocodexUI

@MainActor
final class ChatComposerPasteTests: XCTestCase {
    override func tearDown() {
        UIPasteboard.general.items = []
        super.tearDown()
    }

    func testImageOnlyPasteBecomesAttachmentWithoutReplacingDraft() {
        let view = ComposerTextView()
        view.text = "Look at this screenshot"
        let image = UIGraphicsImageRenderer(size: CGSize(width: 8, height: 8)).image { context in
            UIColor.blue.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 8, height: 8))
        }
        UIPasteboard.general.setData(image.pngData()!, forPasteboardType: UTType.png.identifier)
        var received: [NSItemProvider] = []
        view.onPasteImages = { received = $0 }
        XCTAssertTrue(view.canPerformAction(#selector(UIResponderStandardEditActions.paste(_:)), withSender: nil))
        view.paste(nil)
        XCTAssertEqual(received.count, 1)
        XCTAssertTrue(received[0].hasItemConformingToTypeIdentifier(UTType.png.identifier))
        XCTAssertEqual(view.text, "Look at this screenshot")
    }

    func testTextPasteIsNotConsumedByAttachmentHandler() {
        let view = ComposerTextView()
        view.text = "Before "
        view.onPasteImages = { _ in XCTFail("Plain text must not become an attachment") }
        XCTAssertFalse(view.handleImagePaste([NSItemProvider(object: "after" as NSString)]))
        XCTAssertEqual(view.text, "Before ")
    }
}
#endif
