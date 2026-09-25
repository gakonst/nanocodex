import { createHash } from "node:crypto";
import { Buffer } from "node:buffer";

type File = { path: string; sha256: string; size: number; data_base64?: string };
type Bundle = { turn_id: string; revision: number; files: File[] };
const headers = { "cache-control": "private, no-store" };
const response = (status: number, error?: string) => error
  ? Response.json({ error }, { status, headers }) : new Response(null, { status, headers });
const exactKeys = (v: object, keys: string[]) => Object.keys(v).length === keys.length && keys.every(k => Object.hasOwn(v, k));
const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);

/** One validated bundle per turn; a torn producer write cannot replace it. */
export class OutputCheckpoints {
  constructor(readonly storage: DurableObjectStorage) {
    storage.sql.exec("CREATE TABLE IF NOT EXISTS managed_output_checkpoints (turn_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, body TEXT NOT NULL)");
    storage.sql.exec("CREATE TABLE IF NOT EXISTS managed_output_checkpoint_chunks (turn_id TEXT NOT NULL, ordinal INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY(turn_id,ordinal))");
  }
  async get(request: Request, grantId: string | undefined, bucket: R2Bucket, agentId: string,
    isActive: () => boolean = () => true): Promise<Response> {
    if (request.method !== "GET") return response(405, "method_not_allowed");
    const url = new URL(request.url), turn = url.searchParams.get("turn_id"), after = url.searchParams.get("after");
    if (url.pathname !== "/checkpoints" || !turn || !/^[A-Za-z0-9._:-]{1,128}$/.test(turn) || turn === "." || turn === ".."
      || url.searchParams.getAll("turn_id").length !== 1 || url.searchParams.getAll("after").length > 1
      || [...url.searchParams.keys()].some(k => k !== "turn_id" && k !== "after")
      || (after !== null && (!/^(0|[1-9][0-9]*)$/.test(after) || !Number.isSafeInteger(Number(after)))))
      return response(400, "invalid_checkpoint_query");
    const owner = () => this.storage.sql.exec<{ grant_id: string | null }>(
      "SELECT grant_id FROM managed_turn_file_owners WHERE turn_id=?", turn).toArray()[0];
    const retained = owner();
    if (!isActive() || !retained || (grantId !== undefined && retained.grant_id !== grantId)) return response(404, "not_found");
    const allowed = () => isActive() && owner()?.grant_id === retained.grant_id;
    const latest = () => this.storage.sql.exec<{ revision: number; body: string }>(
      "SELECT revision,body FROM managed_output_checkpoints WHERE turn_id=?", turn).toArray()[0];
    const root = retained.grant_id ? `connect/${retained.grant_id}/outputs/${turn}` : "outputs";
    const prefix = `brains/${agentId}/${root}/checkpoints/`;
    try {
      const manifest = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(await readBounded(bucket, prefix + "latest.json", 16_384)));
      if (record(manifest) && exactKeys(manifest, ["revision", "files"])
        && Number.isSafeInteger(manifest.revision) && Number(manifest.revision) > 0
        && Number(manifest.revision) > (latest()?.revision ?? 0)
        && Array.isArray(manifest.files) && manifest.files.length >= 1 && manifest.files.length <= 8) {
        const revision = Number(manifest.revision), paths = new Set<string>(); let total = 0;
        const files: File[] = [];
        for (const file of manifest.files) {
          if (!record(file) || !exactKeys(file, ["path", "sha256", "size"])
            || typeof file.path !== "string" || !new RegExp(`^r${revision}/[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`).test(file.path)
            || paths.has(file.path) || typeof file.sha256 !== "string" || !/^[a-f0-9]{64}$/.test(file.sha256)
            || !Number.isSafeInteger(file.size) || Number(file.size) < 0 || Number(file.size) > 1_000_000)
            throw new Error("invalid checkpoint manifest");
          paths.add(file.path); total += Number(file.size);
          if (total > 4_000_000) throw new Error("checkpoint too large");
          files.push({ path: file.path, sha256: file.sha256, size: Number(file.size) });
        }
        for (const file of files) {
          const bytes = await readBounded(bucket, prefix + file.path, file.size);
          if (bytes.byteLength !== file.size || createHash("sha256").update(bytes).digest("hex") !== file.sha256)
            throw new Error("checkpoint incomplete");
          file.data_base64 = Buffer.from(bytes).toString("base64");
        }
        if (!allowed()) return response(404, "not_found");
        const bundle: Bundle = { turn_id: turn, revision, files };
        this.storage.transactionSync(() => {
          if (!allowed() || revision <= (latest()?.revision ?? 0)) return;
          // SQLite rows are limited to 2 MB; keep bounded chunks in the same transaction.
          const encoded = JSON.stringify(bundle);
          this.storage.sql.exec("DELETE FROM managed_output_checkpoint_chunks WHERE turn_id=?", turn);
          for (let offset = 0, ordinal = 0; offset < encoded.length; offset += 1_000_000, ordinal++) {
            this.storage.sql.exec("INSERT INTO managed_output_checkpoint_chunks VALUES (?,?,?)", turn, ordinal, encoded.slice(offset, offset + 1_000_000));
          }
          this.storage.sql.exec("INSERT INTO managed_output_checkpoints VALUES (?,?,'') ON CONFLICT(turn_id) DO UPDATE SET revision=excluded.revision,body=''", turn, revision);
        });
      }
    } catch { /* Missing or incomplete writes keep the last verified revision. */ }
    if (!allowed()) return response(404, "not_found");
    const saved = latest();
    if (!saved) return response(204);
    if (after !== null && saved.revision <= Number(after)) return response(304);
    const encoded = saved.body || this.storage.sql.exec<{ body: string }>(
      "SELECT body FROM managed_output_checkpoint_chunks WHERE turn_id=? ORDER BY ordinal", turn).toArray().map(row => row.body).join("");
    return new Response(encoded, { headers: { ...headers, "content-type": "application/json" } });
  }
}

async function readBounded(bucket: R2Bucket, key: string, maximum: number): Promise<Uint8Array> {
  const object = await bucket.get(key);
  if (!object) throw new Error("missing checkpoint");
  if (object.size > maximum) { await object.body.cancel(); throw new Error("checkpoint exceeds bound"); }
  const reader = object.body.getReader(), chunks: Uint8Array[] = []; let size = 0;
  try {
    while (true) {
      const next = await reader.read(); if (next.done) break;
      size += next.value.byteLength;
      if (size > maximum) { await reader.cancel(); throw new Error("checkpoint exceeds bound"); }
      chunks.push(next.value);
    }
  } finally { reader.releaseLock(); }
  const bytes = new Uint8Array(size); let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return bytes;
}
