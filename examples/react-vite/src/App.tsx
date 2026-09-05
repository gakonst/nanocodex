import {
  FormEvent,
  Suspense,
  lazy,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import type { MppConnection } from "./MppControls";
import type {
  AgentWorkerMessage as WorkerMessage,
} from "./protocol";
import type {
  AgentEvent,
  Thinking,
} from "nanocodex";
import {
  appendRetainedEvents,
  summarizeEventBatch,
} from "./eventBatch";

const MppControls = lazy(async () => ({
  default: (await import("./MppControls")).MppControls,
}));

type Status = "waiting" | "starting" | "ready" | "running" | "completed" | "failed";
type JsonObject = Record<string, unknown>;
type TranscriptItem = {
  id: number;
  role: "user" | "assistant" | "error";
  text: string;
};
type ToolTrace = {
  call_id?: string;
  tool?: string;
  arguments?: unknown;
  result?: unknown;
  status?: string;
  duration_ns?: number;
};
const DEFAULT_PROMPT = `Inspect the browser runtime with tools.browserInfo(), then explain what is running in Rust/WASM versus JavaScript. End with one concrete idea for a useful browser-native tool.`;
const MERCATOR_PROMPT = `Use tool_search to discover the Mercator tools. Then, only from Code Mode, search for a low-cost service that returns the current Tempo chain block number and describe the best match. Build and quote a one-node flow with totalMax and maxSpend both set to 0.01. If the aggregate quote is at most 0.01 pathUSD, submit it with a unique idempotency key and poll the job until it completes. Report the chosen service, quote, result, model MPP payment, and Mercator/downstream MPP payment. Never call mcp__mercator__* as a top-level model tool.`;

export function App() {
  const workerRef = useRef<Worker | null>(null);
  const eventQueue = useRef<AgentEvent[]>([]);
  const eventFrame = useRef<number | undefined>(undefined);
  const nextId = useRef(1);
  const startedAt = useRef(0);
  const [thinking, setThinking] = useState<Thinking>("high");
  const [transport, setTransport] = useState<"openai" | "mpp">("openai");
  const [status, setStatus] = useState<Status>("waiting");
  const [ready, setReady] = useState(false);
  const [prompt, setPrompt] = useState(DEFAULT_PROMPT);
  const [pending, setPending] = useState<Set<number>>(() => new Set());
  const [transcript, setTranscript] = useState<TranscriptItem[]>([]);
  const [events, setEvents] = useState<AgentEvent[]>([]);
  const [liveAnswer, setLiveAnswer] = useState("");
  const [reasoning, setReasoning] = useState("");
  const [elapsedMs, setElapsedMs] = useState(0);
  const [payment, setPayment] = useState<{ rootAddress?: string; accessKeyAddress?: string; channelId?: string; cumulative?: string; mcpCumulative?: string }>({});
  const [mppConnection, setMppConnection] = useState<MppConnection>();
  const updateMppConnection = useCallback(
    (connection: MppConnection | undefined) => setMppConnection(connection),
    [],
  );
  const flushEvents = useCallback(() => {
    if (eventFrame.current !== undefined) {
      cancelAnimationFrame(eventFrame.current);
      eventFrame.current = undefined;
    }
    const queued = eventQueue.current;
    eventQueue.current = [];
    if (!queued.length) return;
    const summary = summarizeEventBatch(
      queued,
      startedAt.current,
      Date.now(),
    );
    setEvents((current) => appendRetainedEvents(current, queued));
    const assistant = summary.assistant;
    if (assistant) {
      setLiveAnswer((current) => assistant.mode === "replace"
        ? assistant.text
        : current + assistant.text);
    }
    if (summary.reasoning) {
      setReasoning((current) => current + summary.reasoning);
    }
    if (summary.status) setStatus(summary.status);
    if (summary.elapsedMs !== undefined) setElapsedMs(summary.elapsedMs);
    if (summary.errors.length) {
      setTranscript((current) => [
        ...current,
        ...summary.errors.map((text) => ({
          id: nextId.current++,
          role: "error" as const,
          text,
        })),
      ]);
    }
  }, []);

  useEffect(() => {
    const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
    workerRef.current = worker;
    worker.onmessage = ({ data }: MessageEvent<WorkerMessage>) => {
      if (data.type === "ready") {
        setPayment(data.transport === "mpp" ? {
          rootAddress: data.rootAddress,
          accessKeyAddress: data.accessKeyAddress,
          channelId: data.channelId,
        } : {});
        setReady(true);
        setStatus("ready");
        return;
      }
      if (data.type === "event") {
        eventQueue.current.push(data.event);
        eventFrame.current ??= requestAnimationFrame(flushEvents);
        return;
      }
      flushEvents();
      if (data.type === "result") {
        if (data.payment) setPayment((current) => ({ ...current, ...data.payment }));
        setPending((current) => without(current, data.id));
        setStatus("completed");
        setElapsedMs(Date.now() - startedAt.current);
        setLiveAnswer(data.message);
        setTranscript((current) => [
          ...current,
          { id: data.id, role: "assistant", text: data.message },
        ]);
        return;
      }
      if (data.id !== undefined) setPending((current) => without(current, data.id!));
      setStatus("failed");
      setTranscript((current) => {
        const tail = current.at(-1);
        return tail?.role === "error" && tail.text === data.message
          ? current
          : [
              ...current,
              {
                id: data.id ?? nextId.current++,
                role: "error",
                text: data.message,
              },
            ];
      });
    };
    return () => {
      if (eventFrame.current !== undefined) {
        cancelAnimationFrame(eventFrame.current);
      }
      eventQueue.current = [];
      worker.terminate();
    };
  }, [flushEvents]);

  useEffect(() => {
    if (status !== "running") return;
    const timer = window.setInterval(() => setElapsedMs(Date.now() - startedAt.current), 100);
    return () => window.clearInterval(timer);
  }, [status]);

  const terminal = useMemo(
    () => [...events].reverse().find((event) => event.type === "run.completed" || event.type === "run.failed"),
    [events],
  );
  const tools = useMemo(() => toolTimeline(events), [events]);
  const stats = asObject(terminal?.payload.stats);
  const usage = asObject(stats?.usage);

  async function start(event: FormEvent) {
    event.preventDefault();
    if (eventFrame.current !== undefined) {
      cancelAnimationFrame(eventFrame.current);
      eventFrame.current = undefined;
    }
    eventQueue.current = [];
    setReady(false);
    setStatus("starting");
    setPending(new Set());
    setTranscript([]);
    setEvents([]);
    setLiveAnswer("");
    setReasoning("");
    setElapsedMs(0);
    try {
      if (transport === "mpp") {
        if (!mppConnection) {
          throw new Error("Tempo Wallet integration is still loading");
        }
        setPayment({ rootAddress: await mppConnection.start() });
      } else {
        setPayment({});
      }
      workerRef.current?.postMessage({ type: "start", thinking, transport });
    } catch (error) {
      setStatus("failed");
      setTranscript([{ id: nextId.current++, role: "error", text: error instanceof Error ? error.message : String(error) }]);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    const instruction = prompt.trim();
    if (!instruction || !ready) return;
    const id = nextId.current++;
    startedAt.current = Date.now();
    setElapsedMs(0);
    setStatus("running");
    setLiveAnswer("");
    setReasoning("");
    setPrompt("");
    setPending((current) => new Set(current).add(id));
    setTranscript((current) => [...current, { id, role: "user", text: instruction }]);
    workerRef.current?.postMessage({ type: "prompt", id, prompt: instruction });
  }

  return (
    <main className="shell">
      <header className="hero">
        <div>
          <div className="eyebrow"><span className="live-dot" /> NANOCODEX / WASM LAB</div>
          <h1>The agent loop,<br /><em>inside your browser.</em></h1>
          <p className="lede">
            React drives a dedicated Worker. Rust/WASM owns the persistent Responses session,
            typed history, and tool loop. Choose the normal OpenAI path or opt into Tempo MPP.
          </p>
        </div>
        <div className="architecture" aria-label="Runtime architecture">
          <RuntimeStep number="01" title="React" note="controls + live trace" />
          <RuntimeStep number="02" title="Web Worker" note="isolated host boundary" />
          <RuntimeStep number="03" title="Rust / WASM" note="session + model loop" active />
          {transport === "mpp" ? (
            <>
              <RuntimeStep number="04" title="Tempo MPP" note="reused paid channel" />
              <RuntimeStep number="05" title="OpenAI Responses" note="keyless paid WebSocket" />
            </>
          ) : (
            <>
              <RuntimeStep number="04" title="CF Worker API" note="secret-bound upgrade" />
              <RuntimeStep number="05" title="OpenAI Responses" note="API-key WebSocket" />
            </>
          )}
        </div>
      </header>

      <section className="control-panel">
        <form className="connection-controls" onSubmit={start}>
          <label className="endpoint-field">
            <span>{transport === "mpp" ? "MPP Responses WebSocket" : "OpenAI Responses WebSocket"}</span>
            <input
              value={transport === "mpp"
                ? "wss://openai.mpp.tempo.xyz/v1/responses"
                : "wss://api.openai.com/v1/responses"}
              readOnly
              aria-label="Default OpenAI Responses WebSocket endpoint"
            />
          </label>
          <label>
            <span>Payment</span>
            <select value={transport} onChange={(event) => {
              const next = event.target.value as "openai" | "mpp";
              setTransport(next);
              setPrompt((current) =>
                !current.trim() || current === DEFAULT_PROMPT || current === MERCATOR_PROMPT
                  ? next === "mpp" ? MERCATOR_PROMPT : DEFAULT_PROMPT
                  : current,
              );
            }}>
              <option value="openai">OpenAI API key</option>
              <option value="mpp">Tempo MPP</option>
            </select>
          </label>
          <label>
            <span>Reasoning</span>
            <select value={thinking} onChange={(event) => setThinking(event.target.value as Thinking)}>
              <option value="low">low</option>
              <option value="medium">medium</option>
              <option value="high">high</option>
              <option value="xhigh">xhigh</option>
            </select>
          </label>
          <button
            className="connect"
            type="submit"
            disabled={transport === "mpp" && !mppConnection}
          >
            {transport === "mpp" && !mppConnection
              ? "Loading Tempo…"
              : ready ? "Reset session" : "Start agent"}
          </button>
        </form>
        {transport === "mpp" ? (
          <Suspense fallback={null}>
            <MppControls onChange={updateMppConnection} />
          </Suspense>
        ) : null}

        <div className="panel-divider" />

        <form onSubmit={submit}>
          <div className="prompt-heading">
            <label htmlFor="prompt">Next prompt</label>
            <span>{prompt.length.toLocaleString()} chars · follow-on context retained</span>
          </div>
          <textarea
            id="prompt"
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            disabled={!ready}
            spellCheck="false"
            placeholder="Start the agent, then send a prompt."
          />
          <div className="run-row">
            {transport === "mpp" ? (
              <div>
                <p>
                  Tempo root <code>{shortAddress(payment.rootAddress)}</code> delegates to access key{" "}
                  <code>{shortAddress(payment.accessKeyAddress)}</code>.
                </p>
                {payment.channelId && <p>Reusable MPP channel <code>{shortAddress(payment.channelId)}</code>.</p>}
              </div>
            ) : (
              <p><code>OPENAI_API_KEY</code> stays in the Worker secret binding and never enters the page.</p>
            )}
            <div className="run-controls">
              <span className={`status status-${status}`}>{status}</span>
              <button className="run" type="submit" disabled={!ready || !prompt.trim()}>
                Queue turn <span>↗</span>
              </button>
            </div>
          </div>
        </form>
      </section>

      <section className="metrics" aria-label="Session metrics">
        <Metric label="Wall time" value={formatDuration(elapsedMs)} />
        <Metric label="Model calls" value={formatNumber(stats?.model_calls ?? count(events, "model.call.started"))} />
        <Metric label="Tool calls" value={formatNumber(stats?.tool_calls ?? tools.length)} />
        <Metric label="Total tokens" value={formatNumber(usage?.total_tokens)} />
        <Metric label="WS connects" value={formatNumber(stats?.connection_attempts ?? count(events, "model.connection.completed"))} />
        {transport === "mpp" && (
          <Metric label="MPP paid" value={payment.cumulative ? `${Number(payment.cumulative) / 1_000_000}` : "—"} />
        )}
        {transport === "mpp" && (
          <Metric label="Mercator paid" value={payment.mcpCumulative ? `${Number(payment.mcpCumulative) / 1_000_000}` : "—"} />
        )}
        {transport === "mpp" && (
          <Metric label="PathUSD available" value={mppConnection?.balance ?? "—"} />
        )}
      </section>

      <section className="workspace-grid">
        <article className="card answer-card">
          <CardHeader index="A" title="Live session" meta={`${transcript.length} messages`} />
          <div className="transcript">
            {!transcript.length && !liveAnswer && <Empty text="Your persistent conversation will appear here." />}
            {transcript.map((item) => <Message key={`${item.role}-${item.id}`} item={item} />)}
            {status === "running" && liveAnswer && (
              <div className="message assistant streaming"><span>assistant / live</span><p>{liveAnswer}<i className="cursor" /></p></div>
            )}
            {pending.size > 0 && !liveAnswer && <div className="thinking-line"><span /> Rust/WASM is working…</div>}
          </div>
        </article>

        <article className="card trace-card">
          <CardHeader index="B" title="Execution trace" meta={`${events.length} events`} />
          <div className="trace">
            {!events.length && <Empty text="No runtime events yet." />}
            {tools.map((tool, index) => <ToolEntry key={tool.call_id ?? index} tool={tool} index={index} />)}
            {events.filter(isLifecycleEvent).map((event) => <LifecycleEntry key={`${event.seq}-${event.type}`} event={event} />)}
          </div>
        </article>

        <article className="card reasoning-card">
          <CardHeader index="C" title="Reasoning summary" meta={reasoning ? "streamed" : "optional"} />
          <div className={`reasoning ${reasoning ? "" : "empty"}`}>
            {reasoning || "API-visible reasoning summaries for the current turn will appear here."}
          </div>
        </article>

        <article className="card json-card">
          <CardHeader index="D" title="Raw events" meta={`${events.length} records`} />
          <pre>{events.length ? events.map((event) => JSON.stringify(event)).join("\n") : "// Exact typed event JSON will appear here."}</pre>
        </article>
      </section>
    </main>
  );
}

function RuntimeStep({ number, title, note, active = false }: { number: string; title: string; note: string; active?: boolean }) {
  return <div className={`runtime-step ${active ? "active" : ""}`}><span>{number}</span><strong>{title}</strong><small>{note}</small></div>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div className="metric"><span>{label}</span><strong>{value}</strong></div>;
}

function CardHeader({ index, title, meta }: { index: string; title: string; meta: string }) {
  return <header className="card-header"><span>{index}</span><h2>{title}</h2><small>{meta}</small></header>;
}

function Empty({ text }: { text: string }) {
  return <div className="empty trace-empty">{text}</div>;
}

function Message({ item }: { item: TranscriptItem }) {
  return <div className={`message ${item.role}`}><span>{item.role}</span><p>{item.text}</p></div>;
}

function ToolEntry({ tool, index }: { tool: ToolTrace; index: number }) {
  return (
    <details className="tool-entry" open>
      <summary>
        <span className="trace-index">{String(index + 1).padStart(2, "0")}</span>
        <strong>{tool.tool ?? "exec"}</strong>
        <span className={`tool-state ${tool.status ?? "running"}`}>{tool.status ?? "running"}</span>
        <small>{tool.duration_ns ? formatNs(tool.duration_ns) : ""}</small>
      </summary>
      <div className="tool-body">
        <div><span>arguments</span><pre>{formatJson(tool.arguments)}</pre></div>
        {tool.result !== undefined && <div><span>result</span><pre>{formatJson(tool.result)}</pre></div>}
      </div>
    </details>
  );
}

function LifecycleEntry({ event }: { event: AgentEvent }) {
  return (
    <div className="lifecycle-entry">
      <span className="trace-index">{String(event.seq).padStart(2, "0")}</span>
      <strong>{event.type}</strong>
      <small>{payloadNumber(event.payload, "duration_ns") ? formatNs(payloadNumber(event.payload, "duration_ns")!) : ""}</small>
    </div>
  );
}

function toolTimeline(events: AgentEvent[]): ToolTrace[] {
  const byId = new Map<string, ToolTrace>();
  for (const event of events) {
    const callId = payloadString(event.payload, "call_id");
    if (!callId) continue;
    if (event.type === "tool.call") {
      byId.set(callId, { ...event.payload, call_id: callId, status: "running" });
    } else if (event.type === "tool.result") {
      byId.set(callId, { ...byId.get(callId), ...event.payload, call_id: callId });
    }
  }
  return [...byId.values()];
}

function isLifecycleEvent(event: AgentEvent): boolean {
  return [
    "run.started",
    "model.connection.completed",
    "model.warmup.completed",
    "model.call.completed",
    "model.attempt.retrying",
    "run.completed",
    "run.failed",
  ].includes(event.type);
}

function payloadString(payload: JsonObject, key: string): string | undefined {
  return typeof payload[key] === "string" ? payload[key] : undefined;
}

function payloadNumber(payload: JsonObject, key: string): number | undefined {
  return typeof payload[key] === "number" ? payload[key] : undefined;
}

function asObject(value: unknown): JsonObject | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as JsonObject : undefined;
}

function count(events: AgentEvent[], type: string): number {
  return events.filter((event) => event.type === type).length;
}

function formatDuration(milliseconds: number): string {
  if (milliseconds < 1_000) return `${Math.round(milliseconds)} ms`;
  return `${(milliseconds / 1_000).toFixed(milliseconds < 10_000 ? 2 : 1)} s`;
}

function formatNs(nanoseconds: number): string {
  return formatDuration(nanoseconds / 1_000_000);
}

function formatNumber(value: unknown): string {
  return typeof value === "number" ? value.toLocaleString() : "—";
}

function shortAddress(value: string | undefined): string {
  return value ? `${value.slice(0, 6)}…${value.slice(-4)}` : "not connected";
}

function formatJson(value: unknown): string {
  if (typeof value === "string") return value;
  return value === undefined ? "" : JSON.stringify(value, null, 2);
}

function without(values: Set<number>, value: number): Set<number> {
  const next = new Set(values);
  next.delete(value);
  return next;
}
