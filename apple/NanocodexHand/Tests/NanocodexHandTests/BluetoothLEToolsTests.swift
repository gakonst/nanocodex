import Foundation
import InboxCore
import XCTest
@testable import NanocodexHand

final class BluetoothLEToolsTests: XCTestCase {
    func testCatalogCoversGattLifecycleWithoutDeviceSpecificProfiles() {
        let tools = BluetoothLETools.catalog { name, description, properties, required in
            .object(["name": .string(name), "description": .string(description),
                     "properties": .object(properties), "required": .array(required.map(JSON.string))])
        }
        XCTAssertEqual(Set(tools.map { $0["name"].string }), BluetoothLETools.names)
        XCTAssertTrue(tools.allSatisfy { !$0["description"].string.isEmpty })
    }
}
