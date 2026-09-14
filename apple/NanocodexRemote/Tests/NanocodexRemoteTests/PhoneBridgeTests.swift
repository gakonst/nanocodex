#if os(macOS)
import XCTest
@testable import NanocodexRemote

final class PhoneBridgeTests: XCTestCase {
    @MainActor func testOwnedPhoneBridgeStartsAndReleasesItsPorts() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let executable = environment["NANOCODEX_TEST_PHONE_HELPER"],
              let runner = environment["NANOCODEX_TEST_PHONE_RUNNER"],
              let deviceID = environment["NANOCODEX_TEST_PHONE_UDID"] else {
            throw XCTSkip("Requires a signed developer runner, paired phone, and built companion")
        }
        let phones = try await PhoneBridge.devices(executable: URL(fileURLWithPath: executable))
        XCTAssertTrue(phones.contains(where: { $0.id == deviceID }))
        let bridge = PhoneBridge(executable: URL(fileURLWithPath: executable))
        do {
            try await bridge.start(.init(deviceID: deviceID, runner: URL(fileURLWithPath: runner)))
            let phone = try await PhoneInput.connect(port: 18100)
            XCTAssertGreaterThan(phone.size.width, 0)
            XCTAssertGreaterThan(phone.size.height, phone.size.width)
            phone.releaseAll()
            await bridge.stop()
            var request = URLRequest(url: URL(string: "http://127.0.0.1:18100/status")!)
            request.timeoutInterval = 1
            do {
                _ = try await URLSession.shared.data(for: request)
                XCTFail("Stopping the owned bridge must release its localhost listener")
            } catch { /* expected: no bridge listener remains */ }
        } catch { await bridge.stop(); throw error }
    }
}
#endif
