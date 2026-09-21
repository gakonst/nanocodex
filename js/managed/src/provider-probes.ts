import type { ProviderObservation, ProviderTelemetryStore } from "./provider-telemetry";

const ENDPOINTS = {
  openrouter: "https://openrouter.ai/api/v1/chat/completions",
  vercel: "https://ai-gateway.vercel.sh/v1/chat/completions",
} as const;
const EFFORTS = new Set(["none", "minimal", "low", "medium", "high", "xhigh", "max"]);
const MAX_RESPONSE_BYTES = 65_536;
export const PROVIDER_PROBE_PROMPT_VERSION = "ttft-v1";
/** Request dispatch to the first complete SSE event containing nonempty generated
 * text or plaintext reasoning. Excludes headers, role, keepalive and encrypted
 * reasoning metadata. Only a validated, completed stream contributes a sample.
 * This measures observable generation at the Worker, not client delivery. */
export const PROVIDER_PROBE_TTFT_DEFINITION = "first_nonempty_text_or_reasoning_delta";

export interface ProviderProbeTarget {
  backend: keyof typeof ENDPOINTS | "workers_ai" | "cloudflare";
  /** Provider-facing model ID; the scheduler owns canonical catalog mapping. */
  model: string;
  effort: string | null;
  key?: string;
  /** Deployment-owned Cloudflare REST account; never retained in telemetry. */
  accountId?: string;
}
export interface ProviderProbeAiBinding {
  run(model: string, input: {
    messages: { role: "user"; content: string }[];
    stream: true;
    max_completion_tokens: number;
    reasoning_effort?: string;
  } | {
    input: string;
    stream: true;
    max_output_tokens: number;
    reasoning?: { effort: string };
  }, options?: { signal?: AbortSignal }): Promise<unknown>;
}
export interface ProviderProbeOptions {
  enabled: boolean;
  dailyRequestLimit: number;
  targets: readonly ProviderProbeTarget[];
  store: ProviderTelemetryStore;
  /** Actual executing Worker location, supplied from trusted runtime observation;
   * client request.cf.colo does not establish Smart Placement execution location. */
  workerColo: string | null;
  fetch?: typeof fetch;
  ai?: ProviderProbeAiBinding;
  /** Wall clock for budget day and observation timestamp. */
  now?: () => number;
  monotonicNow?: () => number;
  timeoutMs?: number;
  /** 1..45 sequential attempts, default 8. */
  maxTargetsPerRun?: number;
  /** Offset into runnable targets. The scheduler must persist and advance this by
   * the returned attempt count across ticks to cover the entire configured catalog. */
  startIndex?: number;
  /** 16..2048, default 128, including reasoning. A valid length terminal still
   * measures TTFT if text arrived; never retry or lower the requested effort. */
  maxCompletionTokens?: number;
}

function integerInRange(value: number, min: number, max: number): boolean {
  return Number.isSafeInteger(value) && value >= min && value <= max;
}
function runnable(target: ProviderProbeTarget, ai?: ProviderProbeAiBinding): boolean {
  if (!target || typeof target.model !== "string" || !/^[a-zA-Z0-9_./:@-]{1,160}$/.test(target.model)) return false;
  if (target.effort !== null && !EFFORTS.has(target.effort)) return false;
  if (target.backend === "cloudflare") {
    // Any REST configuration selects REST exclusively, including invalid/partial
    // configuration. Never fall back to the binding with a different identity.
    if (target.key !== undefined || target.accountId !== undefined) {
      return typeof target.accountId === "string" && /^[a-fA-F0-9]{32}$/.test(target.accountId)
        && typeof target.key === "string" && /^[\x21-\x7e]+$/.test(target.key);
    }
    return typeof ai?.run === "function";
  }
  if (target.backend === "workers_ai") return typeof ai?.run === "function";
  return Object.hasOwn(ENDPOINTS, target.backend) && typeof target.key === "string"
    && !!target.key.trim() && !/[\r\n]/.test(target.key);
}

class ProbeProtocolError extends Error {}
class ProbeCancelledError extends Error {}
function protocolError(): never { throw new ProbeProtocolError(); }
function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function nonemptyText(value: unknown): boolean {
  if (value === undefined || value === null) return false;
  if (typeof value !== "string") return protocolError();
  return value.length > 0;
}

/** SSE framing is independent of transport chunk boundaries. Decode UTF-8
 * incrementally and reject invalid sequences; support LF, CRLF, CR and multiline
 * data. No content, usage metadata, or provider error is retained. */
function streamParser(onGenerated: () => void, workersAi = false, responses = false) {
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  let line = "", data: string[] = [], event = "", afterCr = false;
  let finished: string | null = null;
  let done = false;
  function responseEvent(raw: string) {
    // Responses completes with a typed terminal event; [DONE] is optional.
    if (raw === "[DONE]") { if (!done) protocolError(); return; }
    if (done) protocolError();
    let value: unknown;
    try { value = JSON.parse(raw); } catch { return protocolError(); }
    if (!record(value) || typeof value.type !== "string" || value.error != null
      || (event && event !== "message" && event !== value.type)) protocolError();
    const type = value.type;
    if (type === "response.cancelled" || (record(value.response) && value.response.status === "cancelled")) throw new ProbeCancelledError();
    if (type === "error" || type === "response.failed" || (record(value.response)
      && (value.response.error != null || value.response.status === "failed"))) protocolError();
    if (type === "response.output_text.delta" || type === "response.reasoning_text.delta"
      || type === "response.reasoning_summary_text.delta") {
      if (nonemptyText(value.delta)) onGenerated();
    } else if (type === "response.completed" || type === "response.incomplete") {
      if (!record(value.response)) protocolError();
      if (type === "response.completed" ? value.response.status !== "completed"
        : value.response.status !== "incomplete" || !record(value.response.incomplete_details)
          || value.response.incomplete_details.reason !== "max_output_tokens") protocolError();
      done = true;
    } else if (!type.startsWith("response.")) protocolError();
    // Created/in-progress, item/tool events, done snapshots and encrypted metadata
    // are protocol context, never evidence of a generated text/reasoning delta.
  }
  function dispatch() {
    if (event === "error") protocolError();
    if (event === "ping" || event === "keepalive") {
      // Only empty/comment heartbeats are permitted to bypass JSON validation.
      if (data.some(value => value.trim())) protocolError();
    } else if (data.length && responses) {
      responseEvent(data.join("\n"));
    } else if (data.length) {
      if (done || (event && event !== "message")) protocolError();
      const raw = data.join("\n");
      if (raw === "[DONE]") {
        if (!finished) protocolError();
        done = true;
      } else {
        let value: unknown;
        try { value = JSON.parse(raw); } catch { return protocolError(); }
        // Workers AI ends its Chat Completions stream with a legacy usage-only
        // trailer. It is not another generated token and only follows a terminal.
        if (workersAi && finished && record(value) && value.response === "" && record(value.usage)
          && Object.keys(value).every(key => key === "response" || key === "usage")) {
          data = []; event = ""; return;
        }
        if (!record(value) || value.error != null || value.success === false
          || (value.errors != null && (!Array.isArray(value.errors) || value.errors.length))
          || !Array.isArray(value.choices)) protocolError();
        if (value.choices.length === 0) {
          if (!record(value.usage)) protocolError();
        } else {
          if (value.choices.length !== 1) protocolError();
          const choice: unknown = value.choices[0];
          if (!record(choice) || choice.index !== 0 || choice.error != null || !record(choice.delta)) protocolError();
          const delta = choice.delta;
          if ((delta.role != null && delta.role !== "assistant") || delta.error != null || delta.tool_calls != null || delta.function_call != null) protocolError();
          let generated = false;
          for (const field of ["content", "reasoning", "reasoning_content"]) {
            generated = nonemptyText(delta[field]) || generated;
          }
          if (delta.reasoning_details != null) {
            if (!Array.isArray(delta.reasoning_details)) protocolError();
            for (const detail of delta.reasoning_details) {
              if (!record(detail)) protocolError();
              if (detail.type === "reasoning.text") generated = nonemptyText(detail.text) || generated;
              else if (detail.type === "reasoning.summary") generated = nonemptyText(detail.summary) || generated;
              else if (detail.type !== "reasoning.encrypted") protocolError();
            }
          }
          if (finished && generated) protocolError();
          // OpenRouter can repeat the stop reason in its content-free usage frame.
          if (choice.finish_reason != null) {
            if (choice.finish_reason !== "stop" && choice.finish_reason !== "length") protocolError();
            if (finished !== null && finished !== choice.finish_reason) protocolError();
            finished = choice.finish_reason;
          }
          if (generated) onGenerated();
        }
      }
    }
    data = [];
    event = "";
  }
  function acceptLine() {
    if (line === "") dispatch();
    else if (!line.startsWith(":")) {
      const colon = line.indexOf(":");
      const name = colon < 0 ? line : line.slice(0, colon);
      let value = colon < 0 ? "" : line.slice(colon + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (name === "data") data.push(value);
      else if (name === "event") event = value;
      // SSE id/retry and unknown fields do not contain generated text.
    }
    line = "";
  }
  return {
    push(bytes: Uint8Array) {
      let text: string;
      try { text = decoder.decode(bytes, { stream: true }); } catch { return protocolError(); }
      for (const character of text) {
        if (afterCr && character === "\n") { afterCr = false; continue; }
        afterCr = false;
        if (character === "\r" || character === "\n") {
          acceptLine();
          afterCr = character === "\r";
        } else line += character;
      }
      if (done) {
        // Validate any bytes already received after the terminal event, including
        // an unfinished UTF-8 code point, before cancelling the transport.
        try { if (decoder.decode()) protocolError(); } catch { return protocolError(); }
        if (line.trim() || data.length || event) protocolError();
      }
      return done;
    },
  };
}

/** Call from scheduled(event,env,ctx) via ctx.waitUntil(runProviderProbes(...)).
 * This helper neither registers a schedule nor discovers credentials. Configure
 * ONE durable budget owner; multiplying shards multiplies the 4096/day ceiling.
 * Failed/unterminated streams are censored. A valid stop or length terminal plus
 * [DONE], or a valid Responses completed/max-output terminal, plus generated
 * text establishes TTFT, not answer correctness.
 * All cancellation is best effort and never awaited without the probe deadline.
 */
export async function runProviderProbes(options: ProviderProbeOptions): Promise<number> {
  if (options.enabled !== true || !integerInRange(options.dailyRequestLimit, 1, 4096)) return 0;
  const maxTargets = options.maxTargetsPerRun ?? 8;
  const startIndex = options.startIndex ?? 0;
  const maxTokens = options.maxCompletionTokens ?? 128;
  if (!integerInRange(maxTargets, 1, 45) || !integerInRange(startIndex, 0, Number.MAX_SAFE_INTEGER)
    || !integerInRange(maxTokens, 16, 2048)) return 0;
  const targets = options.targets.filter(target => runnable(target, options.ai));
  const wallNow = options.now ?? Date.now;
  const monotonicNow = options.monotonicNow ?? (() => performance.now());
  const requestedTimeout = options.timeoutMs ?? 10_000;
  const timeoutMs = Number.isFinite(requestedTimeout) ? Math.max(1, Math.min(requestedTimeout, 30_000)) : 10_000;
  let attempted = 0;
  for (let offset = 0; offset < Math.min(targets.length, maxTargets); offset++) {
    const target = targets[(startIndex % targets.length + offset) % targets.length];
    const timestamp = wallNow();
    // A storage failure must not issue an unbudgeted paid request.
    try {
      if (!await options.store.reserveProbe(new Date(timestamp).toISOString().slice(0, 10), options.dailyRequestLimit)) break;
    } catch { break; }
    attempted++;
    const controller = new AbortController();
    let rejectDeadline!: (reason: Error) => void;
    const deadline = new Promise<never>((_, reject) => { rejectDeadline = reject; });
    // Observe immediately, including when request setup throws synchronously.
    void deadline.catch(() => {});
    const timer = setTimeout(() => { controller.abort(); rejectDeadline(new Error()); }, timeoutMs);
    const bounded = <T>(promise: Promise<T>) => Promise.race([promise, deadline]);
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    const cancel = (stream?: { cancel(): Promise<unknown> }) => {
      try { void stream?.cancel().catch(() => {}); } catch { /* best effort */ }
    };
    const started = monotonicNow();
    const elapsed = () => Math.max(0, monotonicNow() - started);
    const observation: ProviderObservation = { timestamp, source: "probe", workerColo: options.workerColo,
      clientIngressColo: null, backend: target.backend, model: target.model, effort: target.effort,
      outcome: "network_error", status: null, headersMs: null, fullResponseMs: null,
      generationTtftMs: null, clientDeliveryMs: null, elapsedMs: 0 };
    try {
      // Put fresh entropy before any reusable prompt prefix. No customer content.
      const messages: { role: "user"; content: string }[] = [{ role: "user",
        content: `${crypto.randomUUID()} ${PROVIDER_PROBE_PROMPT_VERSION}. Reply with only OK.` }];
      let body: ReadableStream<Uint8Array>;
      const responsesPayload = { input: messages[0].content, stream: true as const, max_output_tokens: maxTokens,
        ...(target.effort === null ? {} : { reasoning: { effort: target.effort } }) };
      if (target.backend === "workers_ai" || (target.backend === "cloudflare" && target.accountId === undefined)) {
        const payload = target.backend === "cloudflare" ? responsesPayload
          : { messages, stream: true as const, max_completion_tokens: maxTokens,
            ...(target.effort === null ? {} : { reasoning_effort: target.effort }) };
        const request = options.ai!.run(target.model, payload, { signal: controller.signal });
        // A binding may ignore abort while obtaining a stream. Cancel late arrivals.
        void request.then(value => {
          if (controller.signal.aborted && value instanceof ReadableStream) cancel(value);
        }, () => {});
        const value = await bounded(request);
        if (!(value instanceof ReadableStream)) protocolError();
        body = value;
        // The binding exposes no HTTP status or header timing; do not invent either.
      } else {
        const payload = target.backend === "cloudflare" ? { model: target.model, ...responsesPayload }
          : { model: target.model, messages, stream: true,
          ...(target.backend === "openrouter"
            ? { max_tokens: maxTokens, provider: { require_parameters: true },
              ...(target.effort === null ? {} : { reasoning: { effort: target.effort, exclude: false } }) }
            : { max_completion_tokens: maxTokens,
              ...(target.effort === null ? {} : { reasoning_effort: target.effort }) }) };
        const endpoint = target.backend === "cloudflare"
          ? `https://api.cloudflare.com/client/v4/accounts/${target.accountId}/ai/v1/responses`
          : ENDPOINTS[target.backend];
        const request = (options.fetch ?? fetch)(endpoint, {
          method: "POST", redirect: "manual", signal: controller.signal,
          headers: { authorization: `Bearer ${target.key}`, "content-type": "application/json", accept: "text/event-stream" },
          body: JSON.stringify(payload),
        });
        void request.then(response => { if (controller.signal.aborted) cancel(response.body ?? undefined); }, () => {});
        const response = await bounded(request);
        observation.headersMs = elapsed();
        observation.status = response.status;
        if (!response.ok) {
          observation.outcome = "http_error";
          cancel(response.body ?? undefined);
          continue;
        }
        if (response.redirected || !response.body || response.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "text/event-stream") {
          cancel(response.body ?? undefined);
          protocolError();
        }
        body = response.body;
      }
      reader = body.getReader();
      let firstGeneratedMs: number | null = null;
      const parser = streamParser(() => { firstGeneratedMs ??= elapsed(); }, target.backend === "workers_ai", target.backend === "cloudflare");
      let bytes = 0;
      while (true) {
        const chunk = await bounded(reader.read());
        if (chunk.done) protocolError(); // EOF is not a successful terminal event.
        bytes += chunk.value.byteLength;
        if (bytes > MAX_RESPONSE_BYTES) protocolError();
        if (parser.push(chunk.value)) break;
      }
      if (firstGeneratedMs === null) protocolError();
      observation.generationTtftMs = firstGeneratedMs;
      observation.fullResponseMs = elapsed(); // Validated SSE completion, not TCP EOF.
      observation.outcome = "success";
    } catch (error) {
      observation.outcome = controller.signal.aborted ? "timeout" : error instanceof ProbeCancelledError ? "cancelled" : error instanceof ProbeProtocolError ? "protocol_error" : "network_error";
    } finally {
      clearTimeout(timer);
      cancel(reader);
      controller.abort();
      observation.elapsedMs = elapsed();
      // Measurements cannot make a scheduled tick fail or leak upstream error text.
      try { await options.store.append(observation); } catch { /* best effort */ }
    }
  }
  return attempted;
}
