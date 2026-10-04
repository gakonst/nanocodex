import Foundation
import GRDB

/// Durable command journal. Each account's complete state is committed in one
/// SQLite transaction, so steering and its source cannot be torn apart by death.
public final class MobileOutboxStore: @unchecked Sendable {
    public struct Snapshot: Codable, Equatable, Sendable {
        public var pending: [PendingMessage]
        public var cancellations: [PendingTurnCancellation]
        public var steeringTransfers: [SteeringTransfer]
        public var pendingCreations: Set<String>
        /// Captured opt-in for each pending creation, independent of the composer.
        public var asyncCodeModeCreations: Set<String>

        public init(pending: [PendingMessage] = [], cancellations: [PendingTurnCancellation] = [], steeringTransfers: [SteeringTransfer] = [], pendingCreations: Set<String> = [], asyncCodeModeCreations: Set<String> = []) {
            self.pending = pending
            self.cancellations = cancellations
            self.steeringTransfers = steeringTransfers
            self.pendingCreations = pendingCreations
            self.asyncCodeModeCreations = asyncCodeModeCreations
        }

        private enum CodingKeys: String, CodingKey {
            case pending, cancellations, steeringTransfers, pendingCreations, asyncCodeModeCreations
        }

        public init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            pending = try values.decode([PendingMessage].self, forKey: .pending)
            cancellations = try values.decode([PendingTurnCancellation].self, forKey: .cancellations)
            steeringTransfers = try values.decode([SteeringTransfer].self, forKey: .steeringTransfers)
            pendingCreations = try values.decode(Set<String>.self, forKey: .pendingCreations)
            // Outboxes written before this option retain default creation behavior.
            asyncCodeModeCreations = try values.decodeIfPresent(Set<String>.self, forKey: .asyncCodeModeCreations) ?? []
        }
    }

    private let database: DatabaseQueue
    private static let legacyPrefixes = ["inbox.pending.", "inbox.cancellations.", "inbox.steering.", "inbox.creations."]

    public init(path: String) throws {
        var configuration = Configuration()
        // A successful command checkpoint must survive process/OS interruption.
        configuration.prepareDatabase { db in try db.execute(sql: "PRAGMA synchronous = FULL") }
        database = try DatabaseQueue(path: path, configuration: configuration)
        var migrator = DatabaseMigrator()
        migrator.registerMigration("mobile-outbox-v1") { db in
            try db.execute(sql: "CREATE TABLE mobile_outbox (scope TEXT PRIMARY KEY NOT NULL, snapshot BLOB NOT NULL)")
        }
        try migrator.migrate(database)
    }

    public static func applicationStore() throws -> MobileOutboxStore {
        let directory = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            .appendingPathComponent("MobileOutbox", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return try MobileOutboxStore(path: directory.appendingPathComponent("outbox.sqlite").path)
    }

    /// The row itself is the migration marker, including an empty account. Decode
    /// every legacy value before committing; malformed data remains recoverable.
    /// Defaults cleanup follows commit and is safely repeated after a crash.
    public func restore(scope: String, defaults: UserDefaults = .standard) throws -> Snapshot {
        let snapshot = try database.write { db -> Snapshot in
            if let data = try Data.fetchOne(db, sql: "SELECT snapshot FROM mobile_outbox WHERE scope = ?", arguments: [scope]) {
                return try JSONDecoder().decode(Snapshot.self, from: data)
            }
            func decode<T: Decodable>(_ type: T.Type, key: String, fallback: T) throws -> T {
                guard let value = defaults.object(forKey: key + scope) else { return fallback }
                guard let data = value as? Data else { throw CocoaError(.coderReadCorrupt) }
                return try JSONDecoder().decode(type, from: data)
            }
            let creationValue = defaults.object(forKey: "inbox.creations." + scope)
            if creationValue != nil, !(creationValue is [String]) { throw CocoaError(.coderReadCorrupt) }
            let snapshot = Snapshot(
                pending: try decode([PendingMessage].self, key: "inbox.pending.", fallback: []),
                cancellations: try decode([PendingTurnCancellation].self, key: "inbox.cancellations.", fallback: []),
                steeringTransfers: try decode([SteeringTransfer].self, key: "inbox.steering.", fallback: []),
                pendingCreations: Set(creationValue as? [String] ?? []))
            try db.execute(sql: "INSERT INTO mobile_outbox(scope, snapshot) VALUES (?, ?)", arguments: [scope, try JSONEncoder().encode(snapshot)])
            return snapshot
        }
        for prefix in Self.legacyPrefixes { defaults.removeObject(forKey: prefix + scope) }
        return snapshot
    }

    /// Synchronous commit is intentional: returning success establishes the
    /// durability fence before a caller may submit an externally visible command.
    public func save(_ snapshot: Snapshot, scope: String) throws {
        let data = try JSONEncoder().encode(snapshot)
        try database.write { db in
            // Never overwrite an account whose migration has not succeeded.
            try db.execute(sql: "UPDATE mobile_outbox SET snapshot = ? WHERE scope = ?", arguments: [data, scope])
            guard db.changesCount == 1 else { throw CocoaError(.coderReadCorrupt) }
        }
    }
}
