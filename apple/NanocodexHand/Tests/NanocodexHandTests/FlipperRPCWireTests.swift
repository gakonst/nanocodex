import Foundation
import XCTest
@testable import NanocodexHand

final class FlipperRPCWireTests: XCTestCase {
    func testLengthDelimitedRequestsSurviveArbitraryBLEChunks() throws {
        let requests = FlipperRPCWire.requestFrames(commandID: 41, operation: .write(
            path: "/ext/test.bin",
            data: Data(repeating: 0xa5, count: 1_025)
        ))
        XCTAssertEqual(requests.count, 3)

        var stream = requests.reduce(into: Data()) { $0.append($1) }
        let tail = Data(stream.suffix(7))
        stream.removeLast(7)
        let first = try FlipperRPCWire.takeMessages(from: &stream)
        XCTAssertEqual(first.count, 2)
        XCTAssertFalse(stream.isEmpty, "The incomplete final RPC message must remain buffered")
        stream.append(tail)
        let final = try FlipperRPCWire.takeMessages(from: &stream)
        XCTAssertEqual(final.count, 1)
        XCTAssertTrue(stream.isEmpty)

        let frames = try (first + final).map(FlipperRPCWire.decodeMain)
        XCTAssertTrue(frames.allSatisfy { $0.commandID == 41 })
        XCTAssertEqual(frames.map(\.hasNext), [true, true, false])
    }

    func testDecodesChunkedDeviceInfoListAndReadResponses() throws {
        let firstInfo = main(commandID: 7, hasNext: true, field: 33, payload: FlipperRPCWire.message([
            FlipperRPCWire.bytesField(1, Data("hardware_model".utf8)),
            FlipperRPCWire.bytesField(2, Data("f7".utf8))
        ]))
        let secondInfo = main(commandID: 7, field: 33, payload: FlipperRPCWire.message([
            FlipperRPCWire.bytesField(1, Data("firmware_version".utf8)),
            FlipperRPCWire.bytesField(2, Data("1.4.2".utf8))
        ]))
        XCTAssertEqual(
            try FlipperRPCWire.result(from: [firstInfo, secondInfo], for: .deviceInfo),
            .deviceInfo([("hardware_model", "f7"), ("firmware_version", "1.4.2")])
        )

        let file = FlipperRPCWire.message([
            FlipperRPCWire.varintField(1, 0),
            FlipperRPCWire.bytesField(2, Data("remote.ir".utf8)),
            FlipperRPCWire.varintField(3, 123),
            FlipperRPCWire.bytesField(5, Data("abc".utf8))
        ])
        let list = main(commandID: 8, field: 8, payload: FlipperRPCWire.message([FlipperRPCWire.bytesField(1, file)]))
        XCTAssertEqual(
            try FlipperRPCWire.result(from: [list], for: .list(path: "/ext/infrared")),
            .files([.init(name: "remote.ir", isDirectory: false, size: 123, md5: "abc")])
        )

        let readFile = FlipperRPCWire.message([FlipperRPCWire.bytesField(4, Data([1, 2, 3]))])
        let read = main(commandID: 9, field: 10, payload: FlipperRPCWire.message([FlipperRPCWire.bytesField(1, readFile)]))
        XCTAssertEqual(try FlipperRPCWire.readDataByteCount(in: read), 3)
        XCTAssertEqual(
            try FlipperRPCWire.result(from: [read], for: .read(path: "/ext/test.bin")),
            .data(Data([1, 2, 3]))
        )
    }

    func testDecodesScreenFrameIntoAgentReadableBraille() throws {
        var pixels = Data(repeating: 0, count: 1_024)
        pixels[0] = 1
        let frame = main(commandID: 0, field: 22, payload: FlipperRPCWire.message([
            FlipperRPCWire.bytesField(1, pixels), FlipperRPCWire.varintField(2, 0)
        ]))
        let screen = try XCTUnwrap(FlipperRPCWire.screen(from: frame))
        XCTAssertEqual(screen.orientation, 0)
        XCTAssertEqual(screen.braille.first, "⠁")
        XCTAssertEqual(screen.braille.split(separator: "\n").count, 16)
    }

    private func main(commandID: UInt64, hasNext: Bool = false, field: Int, payload: Data) -> FlipperRPCFrame {
        let data = FlipperRPCWire.message([
            FlipperRPCWire.varintField(1, commandID),
            hasNext ? FlipperRPCWire.varintField(3, 1) : Data(),
            FlipperRPCWire.bytesField(field, payload)
        ])
        return try! FlipperRPCWire.decodeMain(data)
    }
}
