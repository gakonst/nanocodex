import XCTest
@testable import InboxCore

final class AppUpdateTests: XCTestCase {
    func feed(build: String = "100", bundle: String = "xyz.paradigm.centaur", url: String? = nil, date: String = "2026-09-20T03:58:02.077734+00:00") throws -> AppUpdate {
        let data = try JSONSerialization.data(withJSONObject: ["version": "0.1.0", "build": build, "bundle_id": bundle, "manifest_url": url ?? "https://nanocodex-ios-updates.gakonst.workers.dev/builds/\(build)/manifest.plist", "published_at": date])
        return try JSONDecoder().decode(AppUpdate.self, from: data)
    }
    func testNumericComparisonAndNoDowngrade() throws {
        XCTAssertTrue(try AppUpdate.isNewer("100", than: "99"))
        XCTAssertTrue(try AppUpdate.isNewer("999999999999999999999999999", than: "100"))
        XCTAssertFalse(try AppUpdate.isNewer("00100", than: "100"))
        XCTAssertNil(try feed().installationURL(installedBuild: "101"))
        XCTAssertNil(try feed().installationURL(installedBuild: "100"))
        for bad in ["", "1.2", "-1", "1/2", "１２"] {
            XCTAssertThrowsError(try AppUpdate.isNewer(bad, than: "99"))
        }
    }
    func testPinnedManifestAndBundle() throws {
        XCTAssertThrowsError(try feed(bundle: "wrong").validate())
        for url in ["http://nanocodex-ios-updates.gakonst.workers.dev/builds/100/manifest.plist", "https://evil.example/manifest.plist", "https://nanocodex-ios-updates.gakonst.workers.dev/builds/99/manifest.plist", "https://nanocodex-ios-updates.gakonst.workers.dev/builds/100/manifest.plist?x=1"] {
            XCTAssertThrowsError(try feed(url: url).validate())
        }
        XCTAssertThrowsError(try feed(date: "bad").validate())
        try feed().validate()
        try feed(date: "2026-09-20T03:58:02Z").validate()
    }
    func testInstallerURLPreservesManifest() throws {
        let update = try feed()
        let url = try XCTUnwrap(update.installationURL(installedBuild: "99"))
        XCTAssertEqual(url.scheme, "itms-services")
        let query = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems)
        XCTAssertEqual(query.first { $0.name == "url" }?.value, update.manifestURL)
        XCTAssertEqual(query.first { $0.name == "action" }?.value, "download-manifest")
    }
}
