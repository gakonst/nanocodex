import Foundation
import InboxCore

public struct HandLocationSnapshot: Codable, Sendable, Equatable {
    public let latitude: Double
    public let longitude: Double
    public let timestamp: Date
    public let horizontalAccuracy: Double
    public let approximate: Bool

    func isUsable(now: Date, maxAge: Double, reducedAccuracy: Bool) -> Bool {
        latitude.isFinite && longitude.isFinite && (-90...90).contains(latitude) && (-180...180).contains(longitude)
            && horizontalAccuracy.isFinite && (0...100_000).contains(horizontalAccuracy)
            && maxAge.isFinite && (0...300).contains(maxAge)
            && (-5...maxAge).contains(now.timeIntervalSince(timestamp))
            && (!reducedAccuracy || (approximate && horizontalAccuracy >= 1000))
    }

    /// Wire value for x-nanocodex-client-context.location and current_location.
    public var json: JSON {
        .object(["latitude": .number(latitude), "longitude": .number(longitude),
                 "accuracy_meters": .number(horizontalAccuracy),
                 "timestamp_ms": .number(timestamp.timeIntervalSince1970 * 1000),
                 "approximate": .bool(approximate)])
    }
}

#if os(iOS)
import CoreLocation

/// Shared by device tools and prompt context. Never requests authorization.
@MainActor
public final class HandLocationProvider: NSObject, @preconcurrency CLLocationManagerDelegate {
    public static let shared = HandLocationProvider()
    private let manager = CLLocationManager()
    private var latestSnapshot: HandLocationSnapshot?
    private var waiters: [UUID: CheckedContinuation<HandLocationSnapshot, Error>] = [:]
    private var timers: [UUID: Task<Void, Never>] = [:]
    public override init() {
        super.init()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyHundredMeters
    }
    private var authorized: Bool {
        manager.authorizationStatus == .authorizedWhenInUse || manager.authorizationStatus == .authorizedAlways
    }
    /// Read a recent fix only while current authorization still permits access.
    public func cachedSnapshot(maxAgeSeconds: Double = 300) -> HandLocationSnapshot? {
        guard authorized, maxAgeSeconds.isFinite, (0...300).contains(maxAgeSeconds),
              let snapshot = latestSnapshot,
              snapshot.isUsable(now: Date(), maxAge: maxAgeSeconds, reducedAccuracy: manager.accuracyAuthorization == .reducedAccuracy) else { return nil }
        return snapshot
    }
    public func currentSnapshot(timeoutSeconds: Double = 8) async throws -> HandLocationSnapshot {
        guard timeoutSeconds.isFinite, (1...10).contains(timeoutSeconds) else { throw HandFailure.invalidInput }
        guard authorized else { throw HandFailure.contextAccess("Location permission is not granted. Enable it in Settings before using location.") }
        try Task.checkCancellation()
        let id = UUID()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                waiters[id] = continuation
                timers[id] = Task { [weak self] in
                    do { try await Task.sleep(nanoseconds: UInt64(timeoutSeconds * 1_000_000_000)) }
                    catch { return }
                    self?.finish(id, result: .failure(HandFailure.contextAccess("Location fix timed out.")))
                }
                manager.startUpdatingLocation()
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.finish(id, result: .failure(CancellationError())) }
        }
    }
    private func finish(_ id: UUID, result: Result<HandLocationSnapshot, Error>) {
        timers.removeValue(forKey: id)?.cancel()
        waiters.removeValue(forKey: id)?.resume(with: result)
        if waiters.isEmpty { manager.stopUpdatingLocation() }
    }
    public func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        if !authorized {
            latestSnapshot = nil
            for id in Array(waiters.keys) { finish(id, result: .failure(HandFailure.contextAccess("Location permission is not granted."))) }
        }
    }
    public func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard authorized, let fix = locations.last, fix.horizontalAccuracy.isFinite, (0...100_000).contains(fix.horizontalAccuracy),
              CLLocationCoordinate2DIsValid(fix.coordinate), (-5...30).contains(Date().timeIntervalSince(fix.timestamp)) else { return }
        let snapshot = HandLocationSnapshot(latitude: fix.coordinate.latitude, longitude: fix.coordinate.longitude,
            timestamp: fix.timestamp, horizontalAccuracy: fix.horizontalAccuracy, approximate: manager.accuracyAuthorization == .reducedAccuracy)
        guard snapshot.isUsable(now: Date(), maxAge: 30, reducedAccuracy: manager.accuracyAuthorization == .reducedAccuracy) else { return }
        latestSnapshot = snapshot
        for id in Array(waiters.keys) { finish(id, result: .success(snapshot)) }
    }
    public func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        // A temporarily unavailable fix can recover until each caller's timeout.
        if let error = error as? CLError, error.code == .locationUnknown { return }
        for id in Array(waiters.keys) { finish(id, result: .failure(HandFailure.contextAccess("Unable to obtain a location fix."))) }
    }
}
#endif
