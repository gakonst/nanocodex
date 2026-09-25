import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import type { Workspace } from "nanocodex-tools";

const MAX_BODY = 1_000_000;
const MAX_FILE = 600_000;
const INPUT_PATH = /^\/inputs\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12})\/([A-Za-z0-9][A-Za-z0-9._-]{0,127})$/i;
const failure = (error: string, status: number) => Response.json({ error }, { status, headers: { "cache-control": "no-store" } });
type Receipt = { sha256: string; size: number; ready: number };

/** Server-owned names and durable quota reservations survive an interrupted R2 PUT. */
export class ConnectInputs {
  readonly #writes = new Set<Promise<void>>();
  async drain(): Promise<void> { await Promise.allSettled([...this.#writes]); }
  constructor(readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_connect_inputs (
      grant_id TEXT NOT NULL, generation TEXT NOT NULL, name TEXT NOT NULL,
      sha256 TEXT NOT NULL, size INTEGER NOT NULL, ready INTEGER NOT NULL,
      PRIMARY KEY(grant_id,generation,name))`);
  }
  async put(request: Request, grantId: string, workspace: Workspace, isActive: () => boolean = () => true): Promise<Response> {
    if (request.method !== "PUT") return failure("method_not_allowed", 405);
    const url = new URL(request.url), match = INPUT_PATH.exec(url.pathname);
    if (!match || url.search || !/^0x[0-9a-f]{64}$/.test(grantId)) return failure("invalid_input_path", 400);
    if (request.headers.get("content-type")?.split(";")[0]?.trim() !== "application/json") return failure("invalid_content_type", 415);
    const declared = request.headers.get("content-length");
    if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > MAX_BODY)) return failure("input_too_large", 413);
    const reader = request.body?.getReader();
    if (!reader) return failure("invalid_input", 400);
    let value: unknown;
    try {
      let size = 0, text = "";
      const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) break;
        size += chunk.value.byteLength;
        if (size > MAX_BODY) { void reader.cancel().catch(() => {}); return failure("input_too_large", 413); }
        text += decoder.decode(chunk.value, { stream: true });
      }
      value = JSON.parse(text + decoder.decode());
    } catch { return failure("invalid_input", 400); }
    finally { reader.releaseLock(); }
    if (!value || typeof value !== "object" || Array.isArray(value)) return failure("invalid_input", 400);
    const body = value as Record<string, unknown>;
    if (Object.keys(body).length !== 2 || typeof body.data_base64 !== "string" || typeof body.sha256 !== "string"
      || !/^[0-9a-f]{64}$/.test(body.sha256)) return failure("invalid_input", 400);
    if (body.data_base64.length > Math.ceil(MAX_FILE / 3) * 4) return failure("input_too_large", 413);
    const bytes = Buffer.from(body.data_base64, "base64");
    if (bytes.byteLength > MAX_FILE) return failure("input_too_large", 413);
    if (bytes.toString("base64") !== body.data_base64 || createHash("sha256").update(bytes).digest("hex") !== body.sha256)
      return failure("invalid_input_digest_or_encoding", 400);
    if (!isActive()) return failure("agent_unavailable", 409);
    const [, generation, name] = match;
    let created = false;
    const reservation = this.storage.transactionSync(() => {
      const existing = this.storage.sql.exec<Receipt>(
        "SELECT sha256,size,ready FROM managed_connect_inputs WHERE grant_id=? AND generation=? AND name=?", grantId, generation!, name!).toArray()[0];
      if (existing) return existing;
      for (const [clause, args, countLimit, byteLimit] of [
        ["WHERE grant_id=? AND generation=?", [grantId, generation!], 8, 4_800_000],
        ["WHERE grant_id=?", [grantId], 256, 30_000_000],
        ["", [], 1024, 120_000_000],
      ] as const) {
        const used = this.storage.sql.exec<{ count: number; size: number }>(
          `SELECT COUNT(*) AS count, COALESCE(SUM(size),0) AS size FROM managed_connect_inputs ${clause}`, ...args).one();
        if (used.count >= countLimit || used.size + bytes.byteLength > byteLimit) return undefined;
      }
      this.storage.sql.exec("INSERT INTO managed_connect_inputs VALUES (?,?,?,?,?,0)", grantId, generation!, name!, body.sha256 as string, bytes.byteLength);
      created = true;
      return { sha256: body.sha256 as string, size: bytes.byteLength, ready: 0 };
    });
    if (!reservation) return failure("input_quota_exceeded", 413);
    if (reservation.sha256 !== body.sha256 || reservation.size !== bytes.byteLength) return failure("input_conflict", 409);
    const path = `/brain/connect/${grantId}/inputs/${generation}/${name}`;
    // Re-materialize accepted bytes on identical retries: execution tools may
    // have changed the live brain file since the original upload.
    {
      const writing = workspace.writeFile(path, bytes);
      this.#writes.add(writing);
      try {
        await writing;
        if (!isActive()) return failure("agent_unavailable", 409);
        this.storage.sql.exec("UPDATE managed_connect_inputs SET ready=1 WHERE grant_id=? AND generation=? AND name=?", grantId, generation!, name!);
      } catch { return failure("input_write_failed", 503); }
      finally { this.#writes.delete(writing); }
    }
    return Response.json({ path, sha256: reservation.sha256, size: reservation.size }, {
      status: created ? 201 : 200, headers: { "cache-control": "no-store" },
    });
  }
}
