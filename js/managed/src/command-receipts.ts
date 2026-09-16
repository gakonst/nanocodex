import { createHash } from "node:crypto";

type Receipt = { id: string; turn_id: string; authority: string; fingerprint: string; status: number | null; body: string | null };
const response = (body: unknown, status = 200) => Response.json(body, { status, headers: { "cache-control": "no-store" } });

/** Retains dispatch intent before effects. An interrupted intent is never executed twice. */
export class CommandReceipts {
  readonly #running = new Map<string, Promise<{ status: number; body: string }>>();
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_command_receipts (
      id TEXT PRIMARY KEY, turn_id TEXT NOT NULL, authority TEXT NOT NULL,
      fingerprint TEXT NOT NULL, status INTEGER, body TEXT
    )`);
  }

  #get(id: string): Receipt | undefined {
    return this.storage.sql.exec<Receipt>("SELECT * FROM managed_command_receipts WHERE id = ?", id).toArray()[0];
  }

  status(turn: string, id: string, authority: string): Response {
    const retained = this.#get(id);
    if (!retained || retained.turn_id !== turn || retained.authority !== authority) {
      return response({ error: "command_not_found" }, 404);
    }
    return response({ request_id: id, turn_id: turn,
      status: retained.status === null ? (this.#running.has(id) ? "pending" : "unknown")
        : retained.status < 300 ? "accepted" : retained.status < 500 ? "rejected" : "unknown",
      response_status: retained.status, response: retained.body === null ? null : JSON.parse(retained.body) });
  }

  async run(turn: string, id: string, authority: string, method: string, input: unknown, execute: () => Promise<Response>): Promise<Response> {
    if (!/^[A-Za-z0-9._:-]{1,128}$/.test(id)) return response({ error: "invalid_command_id" }, 400);
    const fingerprint = createHash("sha256").update(JSON.stringify({ turn, method, input })).digest("hex");
    const retained = this.#get(id);
    if (retained) {
      if (retained.authority !== authority) return response({ error: "turn_authority_mismatch" }, 403);
      if (retained.fingerprint !== fingerprint) return response({ error: "request_id_conflict" }, 409);
      const running = this.#running.get(id);
      if (running) { const done = await running; return new Response(done.body, { status: done.status, headers: { "content-type": "application/json" } }); }
      if (retained.status !== null && retained.body !== null) return new Response(retained.body, { status: retained.status, headers: { "content-type": "application/json" } });
      return response({ error: "command_delivery_unknown", request_id: id, turn_id: turn }, 409);
    }
    const count = this.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_command_receipts").one().count;
    if (count >= 65536) return response({ error: "command_capacity" }, 429);
    this.storage.sql.exec("INSERT INTO managed_command_receipts (id, turn_id, authority, fingerprint) VALUES (?, ?, ?, ?)", id, turn, authority, fingerprint);
    const work = (async () => {
      // Commit dispatch intent before effects; concurrent duplicates join this promise.
      await this.storage.sync();
      const result = await execute();
      const body = await result.text();
      // Ambiguous failures remain fenced as unknown; replay never calls execute again.
      if (method === "steer" && result.status === 503 && JSON.parse(body).error === "turn_recovering") {
        // The service emits this only before calling turn.steer. A retry is safe.
        this.storage.sql.exec("DELETE FROM managed_command_receipts WHERE id = ?", id);
      } else if (result.status < 500) this.storage.sql.exec("UPDATE managed_command_receipts SET status = ?, body = ? WHERE id = ?", result.status, body, id);
      await this.storage.sync();
      return { status: result.status, body };
    })();
    this.#running.set(id, work);
    try { const done = await work; return new Response(done.body, { status: done.status, headers: { "content-type": "application/json" } }); }
    finally { this.#running.delete(id); }
  }
}
