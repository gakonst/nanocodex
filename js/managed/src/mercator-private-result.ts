import type { ToolContext } from "nanocodex";
import { providerVaultRequest } from "./provider-vault";

const LASO = "x402-laso-finance-9ad65ae7";
const TRANSPORT = "defaultMCP:https://mercator.sh/mcp";
const JOB = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const FREE = new Set(["search_services", "describe_service", "quote_plan", "get_connection_status", "get_suggested_queries"]);
const STATUS = new Set(["pending", "running", "succeeded", "partially_succeeded", "failed"]);
type Binding = { owner: string; transport: string; digest: string; operation: string; node?: string; job?: string; card?: string; retryAllowed?: boolean; nonLaso?: boolean };
type Reader = { get<T>(key: string): Promise<T | undefined>; put(key: string, value: unknown): Promise<unknown> };
type Store = Reader & { transaction<T>(closure: (txn: Reader) => Promise<T>): Promise<T> };
type Call = { name: string; arguments: unknown };

/** Account-owned encrypted broker storage survives conversation/session changes.
 * Callbacks are pure storage operations; only explicit CAS conflicts retry. */
export function mercatorPrivateStore(binding: Pick<Fetcher, "fetch">, owner: string): Store {
  const request = async (input: unknown) => {
    const response = await binding.fetch(`https://broker.internal/users/${encodeURIComponent(owner)}/credentials/provider-bindings`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(input), redirect: "manual",
    });
    if ((!response.ok && response.status !== 409) || !response.body) throw Error("mercator_private_store_unavailable");
    const reader = response.body.getReader(); let length = 0; const chunks: Uint8Array[] = [];
    try { for (;;) { const { done, value } = await reader.read(); if (done) break;
      length += value.byteLength; if (length > 65536) throw Error("mercator_private_store_invalid"); chunks.push(value);
    } } finally { await reader.cancel().catch(() => {}); }
    const bytes = new Uint8Array(length); let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    const data: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (!record(data)) throw Error("mercator_private_store_invalid");
    if (response.status === 409) {
      if (data.error === "provider_binding_conflict") return { conflict: true };
      throw Error("mercator_private_store_unavailable");
    }
    return data;
  };
  const read = async (key: string) => {
    const data = await request({ operation: "read", key });
    if (!Number.isSafeInteger(data.version) || data.version < 0 || !("value" in data)) throw Error("mercator_private_store_invalid");
    return { value: data.value === null ? undefined : data.value, version: data.version as number };
  };
  const store: Store = {
    async get<T>(key: string) { return (await read(key)).value as T | undefined; },
    async put(key, value) { await store.transaction(async txn => { await txn.put(key, value); }); },
    async transaction<T>(closure: (txn: Reader) => Promise<T>): Promise<T> {
      for (let attempt = 0; attempt < 8; attempt++) {
        const reads = new Map<string, { value: unknown; version: number }>();
        const writes = new Map<string, unknown>();
        const ensure = async (key: string) => { if (!reads.has(key)) reads.set(key, await read(key)); return reads.get(key)!; };
        const result = await closure({
          async get<V>(key: string) { return (writes.has(key) ? writes.get(key) : (await ensure(key)).value) as V | undefined; },
          async put(key, value) { await ensure(key); writes.set(key, value); },
        });
        if (!writes.size) return result;
        const receipt = await request({ operation: "commit", reads: [...reads].map(([key, row]) => ({ key, version: row.version })), writes: [...writes].map(([key, value]) => ({ key, value })) });
        if (receipt.committed === true) return result;
        if (receipt.conflict !== true) throw Error("mercator_private_store_unavailable");
      }
      throw Error("mercator_private_store_conflict");
    },
  };
  return store;
}

/** Installed only by the host on its default Mercator transport. Mercator's
 * authenticated response establishes service provenance, not Laso wallet identity. */
export function mercatorPrivateResult(options: {
  owner: string; binding: Pick<Fetcher, "fetch">; store: Store;
  authorize(context: ToolContext): void;
}) {
  const key = (kind: string, id: string) => `mercator-private:v2:${options.owner}:${kind}:${id}`;
  const owned = (b: Binding | undefined): b is Binding => !!b && b.owner === options.owner && b.transport === TRANSPORT;
  return {
    async beforeCall(call: Call, context: ToolContext): Promise<{ privateContext?: Binding; result?: unknown } | undefined> {
      if (FREE.has(call.name)) return undefined;
      options.authorize(context);
      const input = call.arguments;
      if (!record(input)) throw Error("mercator_private_request_invalid");
      if (call.name === "list_jobs") return undefined;
      if (call.name === "create_job") {
        if (!record(input.plan) || !Array.isArray(input.plan.nodes) || input.plan.nodes.length < 1 || input.plan.nodes.length > 10
          || typeof input.idempotency_key !== "string" || input.idempotency_key.length < 8 || input.idempotency_key.length > 200) throw Error("mercator_private_request_invalid");
        const nodes = input.plan.nodes;
        if (nodes.some((node: unknown) => !record(node) || typeof node.serviceId !== "string")) throw Error("mercator_private_request_invalid");
        const laso = nodes.filter((node: any) => /laso/i.test(node.serviceId));
        if (laso.length && (nodes.length !== 1 || laso[0].serviceId !== LASO || laso[0].method !== "GET" || laso[0].path !== "/get-card"
          || typeof laso[0].id !== "string" || !/^[A-Za-z0-9_-]{1,80}$/.test(laso[0].id)
          || !record(laso[0].input) || laso[0].input.format !== "json"
          || Object.keys(laso[0].input).some(k => !["amount", "format"].includes(k))
          || typeof laso[0].input.amount !== "number" || !Number.isFinite(laso[0].input.amount) || laso[0].input.amount < 5 || laso[0].input.amount > 1000
          || (laso[0].dependsOn?.length ?? 0) !== 0)) throw Error("mercator_private_plan_unsupported");
        const digest = await hash(canonical({ plan: input.plan, approved_total: input.approved_total }));
        const operation = await hash(input.idempotency_key);
        const bound: Binding = { owner: options.owner, transport: TRANSPORT, digest, operation, ...(laso.length ? { node: laso[0].id } : { nonLaso: true }) };
        // Atomic and durable across overlapping calls/restarts. Even a missing
        // response must never lead to another charge attempt. Recover via get_job.
        const replay = await options.store.transaction(async txn => {
          const prior = await txn.get<Binding>(key("operation", operation));
          if (prior && (!owned(prior) || prior.digest !== digest)) throw Error("mercator_private_operation_conflict");
          if (prior && (prior.node || bound.node) && !prior.retryAllowed) return prior;
          if (!prior || prior.retryAllowed) await txn.put(key("operation", operation), bound);
        });
        if (replay) return { result: mcp(replay.job
          ? { job: { jobId: replay.job }, status: "resume_existing_job", next_action: "Use get_job with this jobId to recover the existing card. Do not create another job." }
          : { status: "outcome_unknown", next_action: "The previous dispatch may have executed. Reconcile existing Mercator history; do not create another job." }) };
        return { privateContext: bound };
      }
      if (call.name === "get_job" || call.name === "get_job_details") {
        if (typeof input.job_id !== "string" || !JOB.test(input.job_id)) throw Error("mercator_private_job_unavailable");
        const bound = await options.store.get<Binding>(key("job", input.job_id));
        if (bound && (!owned(bound) || bound.job !== input.job_id)) throw Error("mercator_private_job_unavailable");
        // A pointer may select a bare credential, losing all classification context.
        if (call.name === "get_job_details" && !bound?.nonLaso) throw Error("mercator_private_use_get_job");
        if (bound?.node && (input.include_plan === true || input.result_mode === "summary")) throw Error("mercator_private_use_get_job");
        return { privateContext: bound };
      }
      throw Error("mercator_private_tool_unavailable");
    },
    async transformResult(call: Call & { result: unknown; privateContext?: unknown }, context: ToolContext): Promise<unknown> {
      if (FREE.has(call.name)) return call.result;
      options.authorize(context);
      const bound = call.privateContext as Binding | undefined;
      if (bound && !owned(bound)) throw Error("mercator_private_job_unavailable");
      const data = envelope(call.result);
      if (!record(data)) throw Error("mercator_private_result_invalid");
      if (call.name === "list_jobs") {
        if (!Array.isArray(data.jobs)) throw Error("mercator_private_result_invalid");
        // History is useful even for unbound Laso orders. It never authorizes
        // capture; discard titles, text mirrors, links, resource bodies and extras.
        const jobs = [];
        for (const job of data.jobs) {
          if (!record(job) || typeof job.jobId !== "string" || !JOB.test(job.jobId) || !STATUS.has(job.status)) throw Error("mercator_private_result_invalid");
          const safe: Record<string, unknown> = { jobId: job.jobId, status: job.status };
          for (const field of ["createdAt", "completedAt", "expiresAt"])
            if (job[field] === null || (typeof job[field] === "string" && /^\d{4}-\d{2}-\d{2}T[\d:.]+Z$/.test(job[field]) && Number.isFinite(Date.parse(job[field])))) safe[field] = job[field];
          for (const field of ["charged", "refunded"])
            if (typeof job[field] === "string" && /^\d+(\.\d+)?$/.test(job[field])) safe[field] = job[field];
          if (typeof job.resultAvailable === "boolean") safe.resultAvailable = job.resultAvailable;
          if (Array.isArray(job.services) && job.services.length && job.services.every((id: unknown) => typeof id === "string" && /^[a-z0-9-]{1,100}$/.test(id))) {
            safe.services = job.services;
            if (!job.services.some((id: string) => /laso/i.test(id))) await options.store.transaction(async txn => {
              const prior = await txn.get<Binding>(key("job", job.jobId));
              if (!prior) await txn.put(key("job", job.jobId), { owner: options.owner, transport: TRANSPORT, digest: "history", operation: "history", job: job.jobId, nonLaso: true });
            });
          }
          if (typeof job.currency === "string" && /^(USD|USDC|USDT|EUR|GBP)$/.test(job.currency)) safe.currency = job.currency;
          if (Array.isArray(safe.services) && !safe.services.some((id: string) => /laso/i.test(id))) {
            if (typeof job.title === "string" && job.title.length <= 100) safe.title = job.title;
            if (typeof job.refundStatus === "string" || job.refundStatus === null) safe.refundStatus = job.refundStatus;
          }
          jobs.push(safe);
        }
        return mcp({ jobs, ...(jobs.every(job => Array.isArray(job.services) && !(job.services as string[]).some(id => /laso/i.test(id))) && typeof data.next_action === "string" ? { next_action: data.next_action } : {}), ...(typeof data.nextCursor === "string" && JOB.test(data.nextCursor) ? { nextCursor: data.nextCursor } : {}) });
      }
      if (call.name === "create_job" && ((record(data.payment) && data.payment.status === "required") || data.management === "close" || data.management === "topUp")) {
        if (data.job !== undefined || !bound) throw Error("mercator_private_result_invalid");
        // An explicit prepayment outcome proves no job was created. A subsequent
        // user call may retry the identical plan; no automatic dispatch occurs.
        await options.store.transaction(async txn => {
          const prior = await txn.get<Binding>(key("operation", bound.operation));
          if (!owned(prior) || prior.digest !== bound.digest || prior.job) throw Error("mercator_private_operation_conflict");
          await txn.put(key("operation", bound.operation), { ...prior, retryAllowed: true });
        });
        const safe = data.management === "close" || data.management === "topUp"
          ? { management: data.management } : { status: "payment_required", payment: { status: "required" } };
        return mcp({ ...safe, next_action: bound.node ? "Complete Mercator payment authorization, then retry the same request and idempotency key." : typeof data.next_action === "string" ? data.next_action : "Complete Mercator payment authorization." });
      }
      if (call.name === "get_job_details") {
        if (!bound?.nonLaso || data.jobId !== bound.job || containsPrivate(data)) throw Error("mercator_private_job_mismatch");
        return mcp(data);
      }
      const job = data.job;
      if (!record(job) || typeof job.jobId !== "string" || !JOB.test(job.jobId) || !STATUS.has(job.status)) throw Error("mercator_private_result_invalid");
      if ((bound?.job && bound.job !== job.jobId) || (call.name === "get_job" && record(call.arguments) && call.arguments.job_id !== job.jobId)) throw Error("mercator_private_job_mismatch");
      const safeJob = { jobId: job.jobId, status: job.status };
      const classification = serviceClass(job);
      if (!bound?.node) {
        // No tokens from an unknown Laso job enter capture, including a copied
        // job ID from history. Unknown output cannot certify its own provenance.
        if (classification === "laso" || containsPrivate(data)) return mcp({ job: safeJob, error: "mercator_private_job_unavailable" });
        if (!bound?.nonLaso && classification !== "ordinary") {
          return mcp({ job: safeJob, ...(job.status === "pending" || job.status === "running" ? {} : { error: "mercator_private_job_unavailable" }) });
        }
        const linked: Binding = { ...(bound ?? { owner: options.owner, transport: TRANSPORT, digest: "observed", operation: "observed" }), job: job.jobId, nonLaso: true };
        await options.store.transaction(async txn => {
          const prior = await txn.get<Binding>(key("job", job.jobId));
          if (prior && (!owned(prior) || prior.node)) throw Error("mercator_private_job_mismatch");
          await txn.put(key("job", job.jobId), linked);
        });
        // Rebuild the MCP envelope to drop duplicate text/resources/_meta.
        return mcp(data);
      }
      const linked = { ...bound, job: job.jobId };
      await options.store.transaction(async txn => {
        const prior = await txn.get<Binding>(key("job", job.jobId));
        if (prior && (!owned(prior) || prior.operation !== bound.operation || prior.digest !== bound.digest)) throw Error("mercator_private_job_mismatch");
        await txn.put(key("job", job.jobId), { ...linked, ...(prior?.card ? { card: prior.card } : {}) });
        await txn.put(key("operation", bound.operation), { ...linked, ...(prior?.card ? { card: prior.card } : {}) });
      });
      if (job.status === "pending" || job.status === "running") return mcp({ job: safeJob, status: "awaiting_card" });
      if (!record(job.result) || Object.keys(job.result).length !== 1 || !record(job.result[bound.node])) return mcp({ job: safeJob, error: "provider_response_invalid" });
      const payload = job.result[bound.node];
      if (!record(payload.card) || typeof payload.card.card_id !== "string" || !payload.card.card_id.length || payload.card.card_id.length > 256) return mcp({ job: safeJob, error: "provider_response_invalid" });
      await options.store.transaction(async txn => {
        const current = await txn.get<Binding>(key("job", job.jobId));
        if (!owned(current) || (current.card && current.card !== payload.card.card_id)) throw Error("mercator_private_card_mismatch");
        const exact = { ...current, card: payload.card.card_id };
        await txn.put(key("job", job.jobId), exact);
        await txn.put(key("operation", bound.operation), exact);
      });
      const operationId = await uuid(`${bound.operation}:${bound.node}`);
      const captured = await providerVaultRequest(options.binding, options.owner, "capture", {
        operation_id: operationId, source: {
          provider: "laso", schema: "laso-us-card-v1", request_id: `${job.jobId}/${bound.node}`,
          transport: TRANSPORT, owner_id: options.owner, operation_id: operationId,
          job_id: job.jobId, node_id: bound.node, card_id: payload.card.card_id,
        }, payload,
      }, context.signal);
      if (typeof captured.capture_id !== "string") return mcp(captured);
      const saved = await providerVaultRequest(options.binding, options.owner, "store", {
        capture_id: captured.capture_id, operation_id: await uuid(`${operationId}:save`),
      }, context.signal);
      return mcp({ job: safeJob, ...saved });
    },
  };
}
// Payment legs and replayPlan are host Mercator metadata, outside provider outputs.
// Require explicit coverage of every output node before allowing unbound results.
function serviceClass(job: Record<string, any>): "laso" | "ordinary" | "unknown" {
  const legs = job.payments?.legs;
  const nodes = job.replayPlan?.nodes;
  const metadata = [...(Array.isArray(legs) ? legs : []), ...(Array.isArray(nodes) ? nodes : [])];
  if (metadata.some(item => record(item) && typeof item.serviceId === "string" && /laso/i.test(item.serviceId))) return "laso";
  const complete = (items: unknown, nodeKey: string) => Array.isArray(items) && items.length > 0
    && items.every(item => record(item) && typeof item.serviceId === "string" && item.serviceId.length > 0 && typeof item[nodeKey] === "string")
    && record(job.result) && Object.keys(job.result).every(id => items.some(item => item[nodeKey] === id));
  return complete(legs, "nodeId") || complete(nodes, "id") ? "ordinary" : "unknown";
}
function containsPrivate(value: unknown): boolean {
  if (Array.isArray(value)) return value.some(containsPrivate);
  if (!record(value)) return false;
  return Object.entries(value).some(([key, v]) => /^(auth|id_token|refresh_token|card_details|card_number|cvv|cvc)$/i.test(key)
    || (key === "serviceId" && typeof v === "string" && /laso/i.test(v)) || containsPrivate(v));
}
function envelope(result: unknown): unknown {
  if (!record(result) || result.isError === true) throw Error("mercator_private_result_invalid");
  if (record(result.structuredContent)) return result.structuredContent;
  if (!Array.isArray(result.content) || result.content.length !== 1 || result.content[0]?.type !== "text" || typeof result.content[0].text !== "string" || result.content[0].text.length > 128 * 1024) throw Error("mercator_private_result_invalid");
  return JSON.parse(result.content[0].text);
}
function mcp(value: unknown) { return { content: [{ type: "text", text: JSON.stringify(value) }], structuredContent: value }; }
function record(value: unknown): value is Record<string, any> { return !!value && typeof value === "object" && !Array.isArray(value); }
async function hash(value: string) { return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))), x => x.toString(16).padStart(2, "0")).join(""); }
async function uuid(value: string) { const h = await hash(value); return `${h.slice(0, 8)}-${h.slice(8, 12)}-4${h.slice(13, 16)}-a${h.slice(17, 20)}-${h.slice(20, 32)}`; }

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (record(value)) return `{${Object.keys(value).sort().filter(key => value[key] !== undefined).map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value) ?? "null";
}
