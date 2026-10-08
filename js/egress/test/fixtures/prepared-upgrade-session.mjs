// Test-only Session fixture: the production managed PreparedModelUpgrade
// against the real SessionModelEgress service binding (RPC ACK + fetch 101).
import { consumeRpcData } from "nanocodex/cloudflare/rpc";
import { DurableObject } from "cloudflare:workers";
import { PreparedModelUpgrade } from "../../../managed/src/prepared-model-upgrade.ts";

const OWNER = "33333333-3333-4333-8333-333333333333";
export class Session extends DurableObject {
  valid = true;
  headers(extra = {}) {
    const hex = [...this.ctx.id.toString()].filter(c => /[0-9a-f]/.test(c)).join("").padEnd(64, "0").slice(0, 64);
    return new Headers({ authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", upgrade: "websocket",
      "openai-beta": "responses_websockets=2026-02-06", "session-id": this.rid, "thread-id": this.rid,
      "x-client-request-id": this.rid, "user-agent": "nanocodex-js/cloudflare",
      "x-nanocodex-subject": `managed-session-v1_${hex}`, "x-nanocodex-session-model-owner": OWNER,
      "x-nanocodex-model-region": "weur", ...extra });
  }
  async fetch(request) {
    const url = new URL(request.url), op = url.pathname.split("/").pop();
    // Direct controls exercise remote consumption liveness independently of
    // the managed helper's own cancellation race. All calls use production RPC/fetch.
    if (op === "remote-prepare") {
      this.rid = crypto.randomUUID();
      this.remote = consumeRpcData(await this.env.MODEL.prepareModelUpgrade({ headers: [...this.headers()] }));
      return Response.json({ ...this.remote, headers: [...this.headers()] });
    }
    if (op === "remote-take") {
      const headers = this.headers({ "x-nanocodex-prepared-model-upgrade": this.remote.id });
      const response = await this.env.MODEL.fetch(new Request("https://nanocodex.internal/v1/responses", { headers }));
      return Response.json({ status: response.status });
    }
    if (op === "remote-cancel") {
      const cancelled = consumeRpcData(await this.env.MODEL.cancelModelUpgrade({ headers: [...this.headers()], id: this.remote.id }));
      return Response.json({ cancelled });
    }
    if (op === "prepare") {
      this.rid = crypto.randomUUID();
      const started = Date.now();
      this.prepared = new PreparedModelUpgrade(new Request("https://nanocodex.internal/v1/responses",
        { headers: this.headers() }), this.env.MODEL);
      await this.prepared.acknowledged();
      const ack_ms = Date.now() - started;
      // First writes only after the ACK, as in DurableAgentSession admission.
      this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS admission (id TEXT)");
      this.ctx.storage.sql.exec("INSERT INTO admission VALUES (?)", this.rid);
      return Response.json({ ack_ms, headers: [...this.headers().entries()] });
    }
    if (op === "hold") { this.hold = new Promise(resolve => { this.release = resolve; }); return Response.json({ ok: true }); }
    if (op === "release") { this.release?.(); return Response.json({ ok: true }); }
    if (op === "dispose") { this.valid = false; this.prepared?.dispose("retired"); return Response.json({ ok: true }); }
    if (op !== "take") return new Response(null, { status: 404 });
    const extra = url.searchParams.get("mismatch") ? { "x-codex-turn-state": "changed" } : {};
    const actual = new Request("https://nanocodex.internal/v1/responses", { headers: this.headers(extra) });
    let response;
    try {
      response = await this.prepared.take(actual, async () => { await this.ctx.storage.sync(); await this.hold; },
        () => this.valid);
    } catch { return Response.json({ outcome: "retired" }, { status: 409 }); }
    const prepared = !!response;
    if (prepared && await this.prepared.take(actual, async () => {}, () => true)) throw Error("duplicate consumption");
    response ??= await this.env.MODEL.fetch(actual);
    if (response.status !== 101 || !response.webSocket) return Response.json({ prepared, status: response.status });
    const socket = response.webSocket; socket.accept();
    const frames = [];
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(Error("no completion")), 3000);
      socket.addEventListener("message", event => {
        const frame = JSON.parse(event.data); frames.push(frame);
        if (frame.type === "response.completed") { clearTimeout(timer); resolve(); }
      });
      socket.send(JSON.stringify({ type: "response.create", input: this.rid }));
    });
    socket.close(1000, "done");
    return Response.json({ prepared, status: 101, frames });
  }
}
export default { fetch(request, env) { return env.SESSIONS.getByName(new URL(request.url).searchParams.get("id")).fetch(request); } };
