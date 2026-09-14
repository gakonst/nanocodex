#if os(macOS)
import XCTest
@testable import NanocodexRemote

private final class RecoveryCapture: RemoteCapture, @unchecked Sendable {
    var onFailure: @Sendable (Error) -> Void = { _ in }
    @MainActor var stops = 0
    @MainActor func stop() async { stops += 1 }
}

@MainActor private final class RecoveryInput: RemoteInputInjector {
    var controlAllowed = true
    var releases = 0
    var applied = 0
    func apply(_ event: RemoteInput) throws { applied += 1 }
    func releaseAll() { releases += 1 }
}

@MainActor private final class RecoverySocket: RemoteHostSignaling {
    var onMessage: (RemoteMessage) -> Void = { _ in }
    var onClose: (Error?) -> Void = { _ in }
    var messages: [RemoteMessage] = []
    var closed = false
    var connectError: Error?
    let generation = UUID().uuidString
    func connect(hand: RemoteHand?) throws {
        if let connectError { throw connectError }
        onMessage(.init(type: "ready"))
    }
    func send(_ message: RemoteMessage) {
        messages.append(message)
        if message.type == "catalog" {
            var reply = RemoteMessage(type: "published"); reply.generation = generation
            onMessage(reply)
        }
    }
    func close(error: Error?) { closed = true; onClose(error) }
}

final class RemoteHostRecoveryTests: XCTestCase {
    private func service() throws -> RemoteService {
        try RemoteService(origin: URL(string: "https://recovery.test")!) { _ in }
    }

    @MainActor private func publish(_ host: RemoteMacHost, service: RemoteService,
                                    capture: RecoveryCapture, input: RecoveryInput? = nil,
                                    kind: RemoteSurface.Kind = .desktop, machine: String = "owned-mac") async {
        let surface = RemoteSurface(id: "display-42", name: "Selected display", kind: kind,
            width: 1600, height: 900, controllable: true)
        let input = input ?? RecoveryInput()
        await host.publish(service: service, machineID: machine, name: "My Mac", surface: surface) { _ in (capture, input) }
    }

    @MainActor func testNetworkReconnectRetainsCaptureAndIdentityButReleasesInput() async throws {
        let service = try service(), host = RemoteMacHost(), capture = RecoveryCapture(), input = RecoveryInput()
        defer { service.close() }
        var sockets: [RecoverySocket] = [], checks = 0
        host.makeSignaling = { _ in let socket = RecoverySocket(); sockets.append(socket); return socket }
        host.checkAuthorization = { current in XCTAssertTrue(current === service); checks += 1 }
        host.recoveryDelay = { _ in .milliseconds(10) }
        await publish(host, service: service, capture: capture, input: input)
        XCTAssertTrue(host.sharing)
        let first = try XCTUnwrap(sockets.first), lateMessage = first.onMessage, lateClose = first.onClose
        var call = RemoteMessage(type: "agent_call")
        call.requestID = UUID().uuidString; call.agentID = "fixture"; call.surfaceID = "display-42"
        call.generation = first.generation; call.deadlineAt = Date().timeIntervalSince1970 * 1000 + 5_000
        call.input = RemoteAgentInput(action: "drag", x: 0.2, y: 0.2, endX: 0.8, endY: 0.8, durationMs: 1_000)
        first.onMessage(call)
        try await eventually { input.applied > 0 }
        let releases = input.releases
        first.onClose(NSError(domain: NSPOSIXErrorDomain, code: 57)) // Socket is not connected.
        XCTAssertTrue(host.reconnecting); XCTAssertFalse(host.sharing)
        XCTAssertEqual(host.surface?.id, "display-42"); XCTAssertEqual(host.viewerCount, 0)
        XCTAssertGreaterThan(input.releases, releases)
        XCTAssertEqual(capture.stops, 0)
        try await eventually { sockets.count == 2 && host.sharing }
        XCTAssertEqual(checks, 1); XCTAssertFalse(host.reconnecting)
        let catalog = try XCTUnwrap(sockets[1].messages.first { $0.type == "catalog" })
        XCTAssertEqual(catalog.machineID, "owned-mac")
        XCTAssertEqual(catalog.surfaces?.first?.id, "display-42")
        var stale = RemoteMessage(type: "published"); stale.generation = first.generation
        lateMessage(stale); lateClose(RemoteError.unauthorized)
        XCTAssertTrue(host.sharing); XCTAssertEqual(capture.stops, 0)
        await host.stop()
        XCTAssertEqual(capture.stops, 1); XCTAssertFalse(host.sharing); XCTAssertFalse(host.reconnecting)
    }

    @MainActor func testStopAndAccountReplacementFenceAnOutstandingAuthorization() async throws {
        let firstService = try service(), nextService = try service(), host = RemoteMacHost()
        defer { firstService.close(); nextService.close() }
        let oldCapture = RecoveryCapture(), nextCapture = RecoveryCapture()
        var sockets: [RecoverySocket] = []
        var pending: CheckedContinuation<Void, Never>?
        host.makeSignaling = { _ in let socket = RecoverySocket(); sockets.append(socket); return socket }
        host.recoveryDelay = { _ in .milliseconds(1) }
        host.checkAuthorization = { _ in await withCheckedContinuation { pending = $0 } }
        await publish(host, service: firstService, capture: oldCapture)
        sockets[0].onClose(URLError(.networkConnectionLost))
        try await eventually { pending != nil }
        await host.stop()
        XCTAssertEqual(oldCapture.stops, 1); XCTAssertFalse(host.reconnecting)
        await publish(host, service: nextService, capture: nextCapture, machine: "replacement-account-mac")
        pending?.resume(); pending = nil
        await Task.yield()
        XCTAssertEqual(sockets.count, 2)
        XCTAssertTrue(host.sharing); XCTAssertEqual(nextCapture.stops, 0)
        XCTAssertEqual(sockets[1].messages.first?.machineID, "replacement-account-mac")
        await host.stop()
    }

    @MainActor func testUnauthorizedRecoveryStopsBeforeOpeningAnotherSocket() async throws {
        let service = try service(), host = RemoteMacHost(), capture = RecoveryCapture()
        defer { service.close() }
        var sockets: [RecoverySocket] = []
        host.makeSignaling = { _ in let socket = RecoverySocket(); sockets.append(socket); return socket }
        host.recoveryDelay = { _ in .milliseconds(1) }
        host.checkAuthorization = { _ in throw RemoteError.unauthorized }
        await publish(host, service: service, capture: capture)
        sockets[0].onClose(URLError(.badServerResponse))
        try await eventually { capture.stops == 1 }
        XCTAssertFalse(host.reconnecting); XCTAssertFalse(host.sharing)
        XCTAssertEqual(host.status, RemoteError.unauthorized.localizedDescription)
        XCTAssertEqual(sockets.count, 1)
        await host.stop()
    }

    @MainActor func testCaptureFailureDuringReconnectRemainsTerminal() async throws {
        let service = try service(), host = RemoteMacHost(), capture = RecoveryCapture()
        defer { service.close() }
        let socket = RecoverySocket()
        host.makeSignaling = { _ in socket }
        host.recoveryDelay = { _ in .seconds(1) }
        await publish(host, service: service, capture: capture)
        socket.onClose(URLError(.notConnectedToInternet))
        XCTAssertTrue(host.reconnecting)
        capture.onFailure(RemoteError.geometryChanged)
        try await eventually { capture.stops == 1 }
        XCTAssertFalse(host.reconnecting); XCTAssertFalse(host.sharing)
        XCTAssertEqual(host.status, RemoteError.geometryChanged.localizedDescription)
        await host.stop()
    }

    @MainActor func testPhonePublicationIsNeverAutomaticallyRestarted() async throws {
        let service = try service(), host = RemoteMacHost(), capture = RecoveryCapture()
        defer { service.close() }
        let socket = RecoverySocket()
        host.makeSignaling = { _ in socket }
        host.checkAuthorization = { _ in XCTFail("Phone host must not enter Mac recovery") }
        await publish(host, service: service, capture: capture, kind: .phone)
        socket.onClose(URLError(.networkConnectionLost))
        try await eventually { capture.stops == 1 }
        XCTAssertFalse(host.reconnecting); XCTAssertFalse(host.sharing)
        await host.stop()
    }

    @MainActor func testExplicitAuthenticationAndPermissionFailuresDoNotScheduleRecovery() async throws {
        let service = try service()
        defer { service.close() }
        for error: Error in [RemoteError.unauthorized, RemoteError.screenPermission, RemoteError.inputPermission,
                             RemoteError.geometryChanged, URLError(.cancelled)] {
            let host = RemoteMacHost(), capture = RecoveryCapture(), socket = RecoverySocket()
            host.makeSignaling = { _ in socket }
            host.recoveryDelay = { _ in XCTFail("Terminal failures must not schedule recovery"); return .zero }
            await publish(host, service: service, capture: capture)
            socket.onClose(error)
            try await eventually { capture.stops == 1 }
            XCTAssertFalse(host.reconnecting); XCTAssertFalse(host.sharing)
            await host.stop()
        }
    }

    @MainActor func testInitialSocketFailureRetriesWithBoundedBackoff() async throws {
        let service = try service(), host = RemoteMacHost(), capture = RecoveryCapture()
        defer { service.close() }
        var sockets: [RecoverySocket] = [], attempts: [Int] = []
        XCTAssertEqual(host.recoveryDelay(0), .seconds(1))
        XCTAssertEqual(host.recoveryDelay(3), .seconds(8))
        XCTAssertEqual(host.recoveryDelay(20), .seconds(15))
        host.makeSignaling = { _ in
            let socket = RecoverySocket()
            if sockets.count < 2 { socket.connectError = URLError(.cannotConnectToHost) }
            sockets.append(socket); return socket
        }
        host.checkAuthorization = { _ in }
        host.recoveryDelay = { attempt in attempts.append(attempt); return .milliseconds(1) }
        await publish(host, service: service, capture: capture)
        try await eventually { host.sharing }
        XCTAssertEqual(attempts, [0, 1]); XCTAssertEqual(sockets.count, 3)
        XCTAssertEqual(capture.stops, 0)
        await host.stop()
    }

    @MainActor func testAutomaticSharingStartsAndRecoversCaptureWithRetainedDisplayAndIdentity() async throws {
        let service = try service(), host = RemoteMacHost(), defaults = try automaticDefaults()
        defer { service.close() }
        addTeardownBlock { await host.stop() }
        defaults.set("existing-installation", forKey: "nanocodex.remote.machine-id")
        defaults.set("display-42", forKey: "nanocodex.remote.display-id")
        let selected = automaticSurface(), input = RecoveryInput()
        var captures: [RecoveryCapture] = [], sockets: [RecoverySocket] = [], authorized = 0
        host.automaticSharingInterval = .milliseconds(10)
        host.macSurfaces = { [self.automaticSurface("display-99"), selected] }
        host.checkAuthorization = { current in XCTAssertTrue(current === service); authorized += 1 }
        host.prepareMacCapture = { _, surfaceID in
            XCTAssertEqual(surfaceID, selected.id)
            let capture = RecoveryCapture(); captures.append(capture); return (capture, input)
        }
        host.makeSignaling = { _ in let socket = RecoverySocket(); sockets.append(socket); return socket }
        host.configureAutomaticSharing(service: service, defaults: defaults)
        XCTAssertTrue(host.automaticSharingEnabled)
        try await eventually { host.sharing && captures.count == 1 }
        let first = captures[0], staleFailure = first.onFailure
        first.onFailure(RemoteError.geometryChanged)
        try await eventually { host.sharing && captures.count == 2 }
        XCTAssertEqual(first.stops, 1); XCTAssertEqual(captures[1].stops, 0)
        XCTAssertEqual(authorized, 2); XCTAssertEqual(sockets.count, 2)
        for socket in sockets {
            let catalog = try XCTUnwrap(socket.messages.first { $0.type == "catalog" })
            XCTAssertEqual(catalog.machineID, "existing-installation")
            XCTAssertEqual(catalog.surfaces?.first?.id, selected.id)
        }
        staleFailure(RemoteError.screenPermission)
        try await Task.sleep(for: .milliseconds(35))
        XCTAssertTrue(host.sharing); XCTAssertEqual(captures.count, 2)
        XCTAssertEqual(defaults.string(forKey: "nanocodex.remote.display-id"), selected.id)
        await host.stop()
    }

    @MainActor func testExplicitStopPersistsAutomaticSharingOptOutUntilEnabledAgain() async throws {
        let service = try service(), host = RemoteMacHost(), defaults = try automaticDefaults()
        defer { service.close() }
        addTeardownBlock { await host.stop() }
        let surface = automaticSurface(), capture = RecoveryCapture()
        host.automaticSharingInterval = .milliseconds(5)
        host.macSurfaces = { [surface] }
        host.checkAuthorization = { _ in }
        host.prepareMacCapture = { _, _ in (capture, RecoveryInput()) }
        host.makeSignaling = { _ in RecoverySocket() }
        host.configureAutomaticSharing(service: service, defaults: defaults)
        try await eventually { host.sharing }
        await host.stopSharing()
        XCTAssertFalse(host.automaticSharingEnabled); XCTAssertFalse(host.sharing)
        XCTAssertEqual(capture.stops, 1)
        XCTAssertEqual(defaults.object(forKey: "nanocodex.remote.automatic-sharing") as? Bool, false)

        let relaunched = RemoteMacHost()
        addTeardownBlock { await relaunched.stop() }
        var enumerations = 0
        relaunched.automaticSharingInterval = .milliseconds(5)
        relaunched.macSurfaces = { enumerations += 1; return [surface] }
        relaunched.checkAuthorization = { _ in }
        relaunched.prepareMacCapture = { _, _ in (RecoveryCapture(), RecoveryInput()) }
        relaunched.makeSignaling = { _ in RecoverySocket() }
        relaunched.configureAutomaticSharing(service: service, defaults: defaults)
        try await Task.sleep(for: .milliseconds(35))
        XCTAssertFalse(relaunched.automaticSharingEnabled); XCTAssertFalse(relaunched.sharing)
        XCTAssertEqual(enumerations, 0)
        await relaunched.setAutomaticSharingEnabled(true)
        try await eventually { relaunched.sharing }
        XCTAssertTrue(defaults.bool(forKey: "nanocodex.remote.automatic-sharing"))
        await relaunched.stop()
    }

    @MainActor func testAppShutdownStopsSupervisorWithoutDisablingFutureAutomaticSharing() async throws {
        let service = try service(), host = RemoteMacHost(), defaults = try automaticDefaults()
        defer { service.close() }
        addTeardownBlock { await host.stop() }
        let surface = automaticSurface()
        var captures: [RecoveryCapture] = []
        host.automaticSharingInterval = .milliseconds(5)
        host.macSurfaces = { [surface] }
        host.checkAuthorization = { _ in }
        host.prepareMacCapture = { _, _ in
            let capture = RecoveryCapture(); captures.append(capture); return (capture, RecoveryInput())
        }
        host.makeSignaling = { _ in RecoverySocket() }
        host.configureAutomaticSharing(service: service, defaults: defaults)
        try await eventually { host.sharing }
        await host.setAutomaticSharingEnabled(true)
        await host.stop()
        try await Task.sleep(for: .milliseconds(35))
        XCTAssertFalse(host.sharing); XCTAssertEqual(captures.count, 1); XCTAssertEqual(captures[0].stops, 1)
        XCTAssertTrue(host.automaticSharingEnabled)
        XCTAssertTrue(defaults.bool(forKey: "nanocodex.remote.automatic-sharing"))
        host.configureAutomaticSharing(service: service, defaults: defaults)
        try await eventually { host.sharing && captures.count == 2 }
        await host.stop()
    }

    @MainActor func testAutomaticSharingStopFencesPendingAuthorization() async throws {
        let service = try service()
        defer { service.close() }
        for optOut in [false, true] {
            let host = RemoteMacHost(), defaults = try automaticDefaults(), surface = automaticSurface()
            addTeardownBlock { await host.stop() }
            var pending: CheckedContinuation<Void, Never>?, returned = false, preparations = 0, sockets = 0
            host.automaticSharingInterval = .milliseconds(5)
            host.macSurfaces = { [surface] }
            host.checkAuthorization = { _ in
                await withCheckedContinuation { pending = $0 }
                returned = true
            }
            host.prepareMacCapture = { _, _ in
                preparations += 1; return (RecoveryCapture(), RecoveryInput())
            }
            host.makeSignaling = { _ in sockets += 1; return RecoverySocket() }
            host.configureAutomaticSharing(service: service, defaults: defaults)
            try await eventually { pending != nil }
            if optOut { await host.stopSharing() } else { await host.stop() }
            pending?.resume(); pending = nil
            try await eventually { returned }
            try await Task.sleep(for: .milliseconds(25))
            XCTAssertEqual(preparations, 0); XCTAssertEqual(sockets, 0); XCTAssertFalse(host.sharing)
            XCTAssertEqual(host.automaticSharingEnabled, !optOut)
        }
    }

    @MainActor func testAutomaticSharingPermissionDenialDefersCaptureAndRetriesAfterGrant() async throws {
        let service = try service(), host = RemoteMacHost(), defaults = try automaticDefaults()
        defer { service.close() }
        addTeardownBlock { await host.stop() }
        let surface = automaticSurface()
        var permissionGranted = false, enumerations = 0, authorizations = 0, preparations = 0
        host.automaticSharingInterval = .milliseconds(5)
        // The injected equivalent of MacScreen.surfaces only checks permission;
        // neither the test nor the supervisor asks macOS to display a prompt.
        host.macSurfaces = {
            enumerations += 1
            guard permissionGranted else { throw RemoteError.screenPermission }
            return [surface]
        }
        host.checkAuthorization = { _ in authorizations += 1 }
        host.prepareMacCapture = { _, _ in
            preparations += 1; return (RecoveryCapture(), RecoveryInput())
        }
        host.makeSignaling = { _ in RecoverySocket() }
        host.configureAutomaticSharing(service: service, defaults: defaults)
        try await eventually { enumerations >= 2 }
        XCTAssertTrue(host.automaticSharingEnabled); XCTAssertFalse(host.sharing)
        XCTAssertEqual(host.status, RemoteError.screenPermission.localizedDescription)
        XCTAssertEqual(authorizations, 0); XCTAssertEqual(preparations, 0)
        permissionGranted = true
        try await eventually { host.sharing }
        XCTAssertEqual(authorizations, 1); XCTAssertEqual(preparations, 1)
        await host.stop()
    }

    @MainActor func testAutomaticSharingDoesNotRestartUnauthorizedOrInvalidPublications() async throws {
        let service = try service()
        defer { service.close() }
        let failures: [Error] = [RemoteError.unauthorized, RemoteError.invalidMessage,
            DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "Invalid fixture message"))]
        for failure in failures {
            let host = RemoteMacHost(), defaults = try automaticDefaults(), capture = RecoveryCapture()
            addTeardownBlock { await host.stop() }
            let surface = automaticSurface()
            var authorizations = 0, preparations = 0, sockets: [RecoverySocket] = []
            host.automaticSharingInterval = .milliseconds(5)
            host.macSurfaces = { [surface] }
            host.checkAuthorization = { _ in authorizations += 1 }
            host.prepareMacCapture = { _, _ in
                preparations += 1; return (capture, RecoveryInput())
            }
            host.makeSignaling = { _ in let socket = RecoverySocket(); sockets.append(socket); return socket }
            host.configureAutomaticSharing(service: service, defaults: defaults)
            try await eventually { host.sharing }
            sockets[0].onClose(failure)
            try await eventually { capture.stops == 1 }
            try await Task.sleep(for: .milliseconds(35))
            XCTAssertTrue(host.automaticSharingEnabled)
            XCTAssertFalse(host.sharing); XCTAssertFalse(host.reconnecting)
            XCTAssertEqual(host.status, failure.localizedDescription)
            XCTAssertEqual(authorizations, 1); XCTAssertEqual(preparations, 1); XCTAssertEqual(sockets.count, 1)
            await host.stop()
        }
    }

    @MainActor func testReplacedAutomaticPublisherRetiresUntilExplicitlyEnabledAgain() async throws {
        let service = try service(), host = RemoteMacHost(), defaults = try automaticDefaults()
        defer { service.close() }
        addTeardownBlock { await host.stop() }
        let surface = automaticSurface(), input = RecoveryInput()
        var captures: [RecoveryCapture] = [], sockets: [RecoverySocket] = []
        host.automaticSharingInterval = .milliseconds(5)
        host.recoveryDelay = { _ in XCTFail("A replaced publisher must not reclaim the screen"); return .zero }
        host.macSurfaces = { [surface] }
        host.checkAuthorization = { _ in }
        host.prepareMacCapture = { _, _ in
            let capture = RecoveryCapture(); captures.append(capture); return (capture, input)
        }
        host.makeSignaling = { _ in let socket = RecoverySocket(); sockets.append(socket); return socket }
        host.configureAutomaticSharing(service: service, defaults: defaults)
        try await eventually { host.sharing }
        let staleClose = sockets[0].onClose, releases = input.releases
        staleClose(RemoteError.hostReplaced)
        try await eventually { captures[0].stops == 1 }
        try await Task.sleep(for: .milliseconds(40))
        XCTAssertFalse(host.sharing); XCTAssertFalse(host.reconnecting)
        XCTAssertTrue(host.automaticSharingEnabled)
        XCTAssertEqual(host.status, RemoteError.hostReplaced.localizedDescription)
        XCTAssertEqual(sockets.count, 1); XCTAssertEqual(captures.count, 1)
        XCTAssertGreaterThan(input.releases, releases)

        await host.setAutomaticSharingEnabled(true)
        try await eventually { host.sharing && sockets.count == 2 }
        XCTAssertEqual(captures.count, 2)
        staleClose(RemoteError.hostReplaced)
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertTrue(host.sharing); XCTAssertEqual(captures[1].stops, 0)
        await host.stop()
    }

    @MainActor private func automaticDefaults() throws -> UserDefaults {
        let suite = "nanocodex.remote.automatic-test.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        addTeardownBlock { defaults.removePersistentDomain(forName: suite) }
        return defaults
    }

    @MainActor private func automaticSurface(_ id: String = "display-42") -> RemoteSurface {
        RemoteSurface(id: id, name: "Selected display", kind: .desktop, width: 1600, height: 900, controllable: true)
    }

    @MainActor private func eventually(_ predicate: () -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(2)
        while ContinuousClock.now < deadline {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("Host recovery did not reach the expected state")
        throw RemoteError.unavailable
    }
}
#endif
