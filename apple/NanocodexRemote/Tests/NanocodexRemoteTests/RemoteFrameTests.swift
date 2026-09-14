import XCTest
import ImageIO
@testable import NanocodexRemote

final class RemoteFrameTests: XCTestCase {
    private func message() throws -> RemoteMessage {
        let context = try XCTUnwrap(CGContext(data: nil, width: 3, height: 2, bitsPerComponent: 8,
            bytesPerRow: 12, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        context.setFillColor(CGColor(red: 0, green: 0, blue: 1, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: 3, height: 2))
        let bytes = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(bytes, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try XCTUnwrap(context.makeImage()), nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        var frame = RemoteMessage(type: "frame")
        frame.jpeg = (bytes as Data).base64EncodedString(); frame.width = 3; frame.height = 2
        return frame
    }

    func testDecodesOnlyBoundedJPEGWithMatchingDimensions() throws {
        let valid = try message()
        let image = try RemoteFrame.decode(valid)
        XCTAssertEqual(image.width, 3); XCTAssertEqual(image.height, 2)
        for dimension in [0, 4, 1281] {
            var invalid = valid; invalid.width = dimension
            XCTAssertThrowsError(try RemoteFrame.decode(invalid))
        }
        var invalid = valid; invalid.jpeg = String(repeating: "A", count: 700_001)
        XCTAssertThrowsError(try RemoteFrame.decode(invalid))
        invalid.jpeg = Data([0xff, 0xd8, 0xff]).base64EncodedString()
        XCTAssertThrowsError(try RemoteFrame.decode(invalid))
    }

    func testFramesCatalogIsExplicitAndRelayUsesTheExistingControlEnvelope() throws {
        let json = #"{"id":"desktop","machine_id":"cf:test","machine_name":"Sandbox","name":"Desktop","kind":"desktop","width":1600,"height":900,"controllable":true,"generation":"test","transport":"frames-v1"}"#
        let hand = try JSONDecoder().decode(RemoteHand.self, from: Data(json.utf8))
        XCTAssertEqual(hand.transport, .frames)
        var message = RemoteMessage(type: "control")
        message.data = .control(.init(type: .acquire))
        let value = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(message)) as? [String: Any])
        XCTAssertEqual((value["data"] as? [String: Any])?["type"] as? String, "acquire")
        XCTAssertNil(value["viewer_id"])
    }
}
