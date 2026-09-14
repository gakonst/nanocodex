import ServiceManagement
import XCTest
@testable import Nanocodex

@MainActor
final class LaunchAtLoginTests: XCTestCase {
    private final class FakeService: LoginItemService {
        var status: SMAppService.Status = .notRegistered
        var registrations = 0
        var unregistrations = 0
        var settingsOpened = 0
        var failure: Error?
        func register() throws {
            registrations += 1
            if let failure { throw failure }
            status = .enabled
        }
        func unregister() throws {
            unregistrations += 1
            if let failure { throw failure }
            status = .notRegistered
        }
        func openLoginItems() { settingsOpened += 1 }
    }

    private func preferences() -> UserDefaults {
        let suite = "nanocodex-launch-at-login-test-" + UUID().uuidString
        let preferences = UserDefaults(suiteName: suite)!
        addTeardownBlock { preferences.removePersistentDomain(forName: suite) }
        return preferences
    }

    func testInstalledAppDefaultsOnOnceAndPreservesLaterOSRemoval() async {
        let service = FakeService(), preferences = preferences()
        let launch = LaunchAtLogin(service: service, preferences: preferences)
        launch.start(); launch.start()
        XCTAssertTrue(launch.isEnabled)
        XCTAssertEqual(service.registrations, 1)
        XCTAssertTrue(preferences.bool(forKey: LaunchAtLogin.preferenceKey))
        service.status = .notRegistered
        let reopened = LaunchAtLogin(service: service, preferences: preferences)
        reopened.start()
        XCTAssertFalse(reopened.isEnabled)
        XCTAssertEqual(service.registrations, 1)
    }

    func testAppOptOutSurvivesRestartAndExplicitToggleControlsRegistration() async {
        let service = FakeService(), preferences = preferences()
        preferences.set(false, forKey: LaunchAtLogin.preferenceKey)
        let launch = LaunchAtLogin(service: service, preferences: preferences)
        launch.start()
        XCTAssertEqual(service.registrations, 0)
        launch.setEnabled(true)
        XCTAssertTrue(launch.isEnabled)
        launch.setEnabled(false)
        XCTAssertFalse(launch.isEnabled)
        XCTAssertFalse(preferences.bool(forKey: LaunchAtLogin.preferenceKey))
        XCTAssertEqual(service.unregistrations, 1)
        LaunchAtLogin(service: service, preferences: preferences).start()
        XCTAssertEqual(service.registrations, 1)
    }

    func testFirstInstalledLaunchRegistersRecoverableNotFoundWithoutOverridingAnOptOut() async {
        let service = FakeService(), preferences = preferences()
        service.status = .notFound
        let launch = LaunchAtLogin(service: service, preferences: preferences)
        launch.start()
        XCTAssertTrue(launch.isEnabled)
        XCTAssertEqual(service.registrations, 1)
        XCTAssertTrue(preferences.bool(forKey: LaunchAtLogin.preferenceKey))
        preferences.set(false, forKey: LaunchAtLogin.preferenceKey)
        service.status = .notFound
        LaunchAtLogin(service: service, preferences: preferences).start()
        XCTAssertEqual(service.registrations, 1)
        XCTAssertFalse(preferences.bool(forKey: LaunchAtLogin.preferenceKey))
    }

    func testOSApprovalRequiresAnExplicitSettingsActionWithoutReregistering() async {
        let service = FakeService(), preferences = preferences()
        service.status = .requiresApproval
        let launch = LaunchAtLogin(service: service, preferences: preferences)
        launch.start()
        XCTAssertTrue(launch.requiresApproval)
        XCTAssertFalse(launch.isEnabled)
        XCTAssertEqual(service.registrations, 0)
        XCTAssertEqual(service.settingsOpened, 0)
        launch.setEnabled(true)
        XCTAssertEqual(service.registrations, 0)
        XCTAssertEqual(service.settingsOpened, 1)
        service.status = .enabled
        launch.refresh()
        XCTAssertTrue(launch.isEnabled)
        XCTAssertFalse(launch.requiresApproval)
    }

    func testFailedDefaultRegistrationReportsActualStateAndDoesNotRetryOnStartup() async {
        let service = FakeService(), preferences = preferences()
        service.failure = NSError(domain: "LoginItemTest", code: 1, userInfo: [NSLocalizedDescriptionKey: "Registration unavailable"])
        let launch = LaunchAtLogin(service: service, preferences: preferences)
        launch.start()
        XCTAssertFalse(launch.isEnabled)
        XCTAssertEqual(launch.error, "Registration unavailable")
        LaunchAtLogin(service: service, preferences: preferences).start()
        XCTAssertEqual(service.registrations, 1)
        service.failure = nil
        launch.setEnabled(true)
        XCTAssertTrue(launch.isEnabled)
        XCTAssertNil(launch.error)
    }

    func testOnlySignedInstalledAppWithoutTestOrIsolatedRuntimeCanRegister() async {
        func eligible(_ path: String = "/Applications/Nanocodex.app", isolated: Bool = false,
                      environment: [String: String] = [:], signed: Bool = true) -> Bool {
            LaunchAtLogin.allowsRegistration(bundleURL: URL(fileURLWithPath: path),
                bundleIdentifier: "xyz.paradigm.nanocodex.macos", isolatedSession: isolated,
                environment: environment, signed: signed)
        }
        XCTAssertTrue(eligible())
        XCTAssertFalse(eligible("/tmp/Nanocodex.app"))
        XCTAssertFalse(eligible("/Applications/Debug/Nanocodex.app"))
        XCTAssertFalse(eligible(isolated: true))
        XCTAssertFalse(eligible(signed: false))
        for key in ["NANOCODEX_DESKTOP_DATA", "XCTestConfigurationFilePath", "XCTestBundlePath", "XCODE_RUNNING_FOR_PREVIEWS"] {
            XCTAssertFalse(eligible(environment: [key: "test"]))
        }
    }

    func testIsolatedAppModelCannotRegisterOrChangeRealLoginPreference() async {
        let preferences = preferences()
        let model = AppModel(runtimeDirectory: "/tmp/nanocodex-login-test-" + UUID().uuidString,
            backgroundPreferences: preferences)
        model.launchAtLogin.start()
        model.launchAtLogin.setEnabled(true)
        XCTAssertFalse(model.launchAtLogin.isAvailable)
        XCTAssertNil(preferences.object(forKey: LaunchAtLogin.preferenceKey))
    }
}
