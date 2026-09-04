import {
  HostedToolsBrokerCore,
  hostedToolsAmbiguous,
  hostedToolsUnavailable,
  type HostedToolsBrokerCoreOptions,
  type HostedToolsBrokerCoreContext,
  type HostedToolsBrokerPersistence,
  type HostedToolsCallRow,
  type HostedToolsCallState,
  type HostedToolsLeasedAttachmentPolicy,
  type HostedToolsStateRow,
} from "nanocodex-tools/hosted";

export * from "nanocodex-tools/hosted";

export type HostedToolsBrokerContext = Pick<
  DurableObjectState,
  "acceptWebSocket" | "getWebSockets" | "storage"
>;

export type HostedToolsBrokerOptions = Readonly<
  Omit<HostedToolsBrokerCoreOptions, "persistence"> & {
    persistence?: HostedToolsBrokerPersistence;
  }
>;

/** Cloudflare Durable Object adapter for the platform-neutral Hosted Tools broker core. */
export class HostedToolsBroker extends HostedToolsBrokerCore {
  constructor(context: HostedToolsBrokerContext, options: HostedToolsBrokerOptions = {}) {
    const coreContext: HostedToolsBrokerCoreContext = {
      accept: (socket) => context.acceptWebSocket(socket as WebSocket, ["hosted-tools"]),
      sockets: () => context.getWebSockets("hosted-tools"),
      readAttachment: (socket) => (socket as WebSocket).deserializeAttachment(),
      writeAttachment: (socket, value) => (socket as WebSocket).serializeAttachment(value),
    };
    super(coreContext, {
      ...options,
      persistence: options.persistence ?? new SqlHostedToolsPersistence(context.storage),
    });
  }

  upgrade(
    sessionId: string,
    allowedMcpIds?: readonly string[],
    appToolCatalogDigest?: `0x${string}`,
    connectGrantId?: string,
    leasedAttachment?: HostedToolsLeasedAttachmentPolicy,
  ): Response {
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    this.accept(
      server,
      sessionId,
      allowedMcpIds,
      appToolCatalogDigest,
      connectGrantId,
      leasedAttachment,
    );
    return new Response(null, { status: 101, webSocket: client });
  }
}

class SqlHostedToolsPersistence implements HostedToolsBrokerPersistence {
  constructor(readonly storage: DurableObjectStorage) {}

  initialize(now: number): readonly HostedToolsStateRow[] {
    this.storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS hosted_tools_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        generation INTEGER NOT NULL DEFAULT 0,
        host_id TEXT,
        lease_id TEXT,
        lease_expires_at INTEGER NOT NULL DEFAULT 0,
        catalog_json TEXT
      );
      INSERT OR IGNORE INTO hosted_tools_state (singleton) VALUES (1);
      CREATE TABLE IF NOT EXISTS hosted_tools_routes (
        route_id TEXT PRIMARY KEY,
        generation INTEGER NOT NULL DEFAULT 0,
        host_id TEXT,
        lease_id TEXT,
        lease_expires_at INTEGER NOT NULL DEFAULT 0,
        catalog_json TEXT
      );
      INSERT OR IGNORE INTO hosted_tools_routes
        (route_id, generation, host_id, lease_id, lease_expires_at, catalog_json)
        SELECT '$legacy', generation, host_id, lease_id, lease_expires_at, catalog_json
        FROM hosted_tools_state
        WHERE singleton = 1;
      UPDATE hosted_tools_state
        SET host_id = NULL, lease_id = NULL, lease_expires_at = 0, catalog_json = NULL
        WHERE singleton = 1;
      CREATE TABLE IF NOT EXISTS hosted_tool_calls (
        call_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        source_call_id TEXT NOT NULL,
        host_id TEXT NOT NULL,
        lease_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        model TEXT NOT NULL,
        name TEXT NOT NULL,
        input_json TEXT NOT NULL,
        output_token_budget INTEGER NOT NULL,
        output_byte_budget INTEGER NOT NULL,
        deadline_at INTEGER NOT NULL,
        cancel_requested INTEGER NOT NULL CHECK (cancel_requested IN (0, 1)),
        state TEXT NOT NULL CHECK (
          state IN ('admitted', 'dispatched', 'completed', 'unavailable', 'ambiguous', 'cancelled')
        ),
        result_json TEXT,
        receipt_json TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      );
      CREATE UNIQUE INDEX IF NOT EXISTS hosted_tool_calls_source
        ON hosted_tool_calls(session_id, source_call_id);
      CREATE INDEX IF NOT EXISTS hosted_tool_calls_attachment
        ON hosted_tool_calls(lease_id, generation, state);
    `);
    return this.transaction(() => {
      const retired = this.states().filter((state) => state.lease_id !== null);
      this.storage.sql.exec(
        `UPDATE hosted_tool_calls SET state = 'unavailable', result_json = ?, updated_at = ?
         WHERE state = 'admitted'`,
        JSON.stringify(hostedToolsUnavailable("Hosted Tools lifecycle restarted before dispatch")),
        now,
      );
      this.storage.sql.exec(
        `UPDATE hosted_tool_calls SET state = 'ambiguous', result_json = ?, updated_at = ?
         WHERE state = 'dispatched'`,
        JSON.stringify(hostedToolsAmbiguous("Hosted Tools lifecycle restarted after dispatch")),
        now,
      );
      return retired;
    });
  }

  transaction<T>(callback: () => T): T { return this.storage.transactionSync(callback); }

  states(): readonly HostedToolsStateRow[] {
    return this.storage.sql.exec<HostedToolsStateRow>(
      `SELECT route_id, generation, host_id, lease_id, lease_expires_at, catalog_json
       FROM hosted_tools_routes ORDER BY route_id`,
    ).toArray();
  }

  state(routeId: string): HostedToolsStateRow | undefined {
    const row = this.storage.sql.exec<HostedToolsStateRow>(
      `SELECT route_id, generation, host_id, lease_id, lease_expires_at, catalog_json
       FROM hosted_tools_routes WHERE route_id = ?`,
      routeId,
    ).toArray()[0];
    return row;
  }

  replaceHost(row: HostedToolsStateRow): void {
    this.storage.sql.exec(
      `INSERT INTO hosted_tools_routes
         (route_id, generation, host_id, lease_id, lease_expires_at, catalog_json)
       VALUES (?, ?, ?, ?, ?, ?)
       ON CONFLICT(route_id) DO UPDATE SET
         generation = excluded.generation,
         host_id = excluded.host_id,
         lease_id = excluded.lease_id,
         lease_expires_at = excluded.lease_expires_at,
         catalog_json = excluded.catalog_json`,
      row.route_id,
      row.generation,
      row.host_id,
      row.lease_id,
      row.lease_expires_at,
      row.catalog_json,
    );
  }

  clearHost(leaseId: string, generation: number): void {
    this.storage.sql.exec(
      `UPDATE hosted_tools_routes
       SET host_id = NULL, lease_id = NULL, lease_expires_at = 0,
           catalog_json = NULL
       WHERE lease_id = ? AND generation = ?`,
      leaseId,
      generation,
    );
  }

  clearCatalog(leaseId: string, generation: number): void {
    this.storage.sql.exec(
      `UPDATE hosted_tools_routes
       SET catalog_json = NULL
       WHERE lease_id = ? AND generation = ?`,
      leaseId,
      generation,
    );
  }

  call(callId: string): HostedToolsCallRow | undefined {
    return this.storage.sql.exec<HostedToolsCallRow>(
      `SELECT call_id, session_id, source_call_id, host_id, lease_id, generation,
              model, name, input_json, output_token_budget, output_byte_budget,
              deadline_at, cancel_requested, state, result_json, receipt_json
       FROM hosted_tool_calls WHERE call_id = ?`,
      callId,
    ).toArray()[0];
  }

  callBySource(sessionId: string, sourceCallId: string): HostedToolsCallRow | undefined {
    return this.storage.sql.exec<HostedToolsCallRow>(
      `SELECT call_id, session_id, source_call_id, host_id, lease_id, generation,
              model, name, input_json, output_token_budget, output_byte_budget,
              deadline_at, cancel_requested, state, result_json, receipt_json
       FROM hosted_tool_calls WHERE session_id = ? AND source_call_id = ?`,
      sessionId,
      sourceCallId,
    ).toArray()[0];
  }

  insertCall(row: HostedToolsCallRow, now: number): void {
    this.storage.sql.exec(
      `INSERT INTO hosted_tool_calls
         (call_id, session_id, source_call_id, host_id, lease_id, generation,
          model, name, input_json, output_token_budget, output_byte_budget, deadline_at,
          cancel_requested, state, result_json, receipt_json, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      row.call_id,
      row.session_id,
      row.source_call_id,
      row.host_id,
      row.lease_id,
      row.generation,
      row.model,
      row.name,
      row.input_json,
      row.output_token_budget,
      row.output_byte_budget,
      row.deadline_at,
      row.cancel_requested,
      row.state,
      row.result_json,
      row.receipt_json,
      now,
      now,
    );
  }

  markCancelRequested(callId: string, now: number): HostedToolsCallRow | undefined {
    this.storage.sql.exec(
      `UPDATE hosted_tool_calls SET cancel_requested = 1, updated_at = ?
       WHERE call_id = ? AND state = 'dispatched'`,
      now,
      callId,
    );
    return this.call(callId);
  }

  transitionCall(
    callId: string,
    from: readonly HostedToolsCallState[],
    state: HostedToolsCallState,
    resultJson: string,
    now: number,
  ): HostedToolsCallRow | undefined {
    if (from.length === 0) return this.call(callId);
    const placeholders = from.map(() => "?").join(", ");
    this.storage.sql.exec(
      `UPDATE hosted_tool_calls SET state = ?, result_json = ?, updated_at = ?
       WHERE call_id = ? AND state IN (${placeholders})`,
      state,
      resultJson || null,
      now,
      callId,
      ...from,
    );
    return this.call(callId);
  }

  recordLateReceipt(callId: string, receiptJson: string, now: number): HostedToolsCallRow | undefined {
    this.storage.sql.exec(
      `UPDATE hosted_tool_calls SET receipt_json = ?, updated_at = ?
       WHERE call_id = ? AND state = 'ambiguous' AND receipt_json IS NULL`,
      receiptJson,
      now,
      callId,
    );
    return this.call(callId);
  }

  markGenerationAmbiguous(leaseId: string, generation: number, resultJson: string, now: number): void {
    this.storage.sql.exec(
      `UPDATE hosted_tool_calls SET state = 'ambiguous', result_json = ?, updated_at = ?
       WHERE lease_id = ? AND generation = ? AND state = 'dispatched'`,
      resultJson,
      now,
      leaseId,
      generation,
    );
  }

  activeCallCount(leaseId: string, generation: number): number {
    return Number(this.storage.sql.exec<{ count: number }>(
      `SELECT COUNT(*) AS count FROM hosted_tool_calls
       WHERE lease_id = ? AND generation = ? AND state IN ('admitted', 'dispatched')`,
      leaseId,
      generation,
    ).toArray()[0]?.count ?? 0);
  }

  generationCallCount(leaseId: string, generation: number): number {
    return Number(this.storage.sql.exec<{ count: number }>(
      `SELECT COUNT(*) AS count FROM hosted_tool_calls
       WHERE lease_id = ? AND generation = ?`,
      leaseId,
      generation,
    ).toArray()[0]?.count ?? 0);
  }

  pruneReceipts(limit: number): void {
    this.storage.sql.exec(
      `DELETE FROM hosted_tool_calls WHERE call_id IN (
         SELECT call_id FROM hosted_tool_calls
         WHERE state NOT IN ('admitted', 'dispatched')
           AND NOT EXISTS (
             SELECT 1 FROM hosted_tools_routes
             WHERE hosted_tools_routes.lease_id = hosted_tool_calls.lease_id
               AND hosted_tools_routes.generation = hosted_tool_calls.generation
           )
         ORDER BY updated_at DESC, call_id DESC LIMIT -1 OFFSET ?
       )`,
      limit,
    );
  }
}
