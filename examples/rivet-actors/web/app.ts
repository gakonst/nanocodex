import { createClient } from "rivetkit/client";

import type { registry } from "../src/registry.js";

type PendingTurn = { id: string; input: string };
type ActiveTurn = { id: string; input: string };
type Message = { role: "you" | "agent" | "error"; text: string; turnId?: string };
type WebState = {
  endpoint: string;
  actorKey: string;
  pending?: PendingTurn;
  activeTurns: ActiveTurn[];
  messages: Message[];
};

const STORAGE_KEY = "nanocodex.rivet.web.v1";
const MAX_STREAMED_TEXT_BYTES = 1024 * 1024;
const byId = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const ui = {
  activity: byId<HTMLSpanElement>("activity"),
  actor: byId<HTMLElement>("actor"),
  actorKey: byId<HTMLInputElement>("actor-key"),
  connect: byId<HTMLButtonElement>("connect"),
  detach: byId<HTMLButtonElement>("detach"),
  endpoint: byId<HTMLInputElement>("endpoint"),
  form: byId<HTMLFormElement>("prompt-form"),
  input: byId<HTMLTextAreaElement>("prompt"),
  newActor: byId<HTMLButtonElement>("new-actor"),
  send: byId<HTMLButtonElement>("send"),
  status: byId<HTMLSpanElement>("status"),
  transcript: byId<HTMLElement>("transcript"),
};
const makeClient = (endpoint: string) => createClient<typeof registry>({ endpoint });
type NanocodexClient = ReturnType<typeof makeClient>;
type Session = ReturnType<NanocodexClient["nanocodex"]["getOrCreate"]>;
type Connection = ReturnType<Session["connect"]>;

const requestedEndpoint = new URLSearchParams(location.search).get("endpoint")?.trim();
const defaultEndpoint = requestedEndpoint || (location.hostname === "127.0.0.1" || location.hostname === "localhost"
  ? "http://127.0.0.1:6420"
  : `${location.origin}/api/rivet`);
let state = loadState() ?? freshState(defaultEndpoint);
if (requestedEndpoint) state.endpoint = requestedEndpoint;
let client: NanocodexClient | undefined;
let session: Session | undefined;
let connection: Connection | undefined;
let generation = 0;
let eventCount = 0;
let streamedText = "";

ui.endpoint.value = state.endpoint;
ui.actorKey.value = state.actorKey;
renderMessages();
saveState();
void connect();

window.addEventListener("storage", (event) => {
  if (event.key === STORAGE_KEY) syncStoredState(event.newValue);
});
ui.connect.addEventListener("click", () => void connect());
ui.newActor.addEventListener("click", () => {
  void detach();
  state = freshState(ui.endpoint.value.trim() || defaultEndpoint);
  streamedText = "";
  ui.actorKey.value = state.actorKey;
  ui.actor.textContent = "not resolved";
  saveState();
  renderMessages();
  void connect();
});
ui.detach.addEventListener("click", () => void detach());
ui.form.addEventListener("submit", (event) => {
  event.preventDefault();
  const input = ui.input.value.trim();
  if (!input) return setActivity("prompt is empty", true);
  if (!session) return setActivity("connect to the actor first", true);
  if (state.pending) return setActivity("one durable turn is already pending", true);
  const pending = { id: crypto.randomUUID(), input };
  state.pending = pending;
  observeTurn(pending.id, pending.input);
  ui.input.value = "";
  saveState();
  void runPending(session, generation);
});

async function connect(): Promise<void> {
  const endpoint = ui.endpoint.value.trim();
  const actorKey = ui.actorKey.value.trim();
  if (!endpoint || !actorKey) return setActivity("endpoint and actor key are required", true);
  await detach(false);
  state.endpoint = endpoint.replace(/\/$/, "");
  state.actorKey = actorKey;
  saveState();
  eventCount = 0;
  streamedText = "";
  setStatus("connecting", "warn");
  const activeGeneration = ++generation;
  try {
    client = makeClient(state.endpoint);
    const handle = client.nanocodex.getOrCreate([state.actorKey]);
    session = handle;
    const nextConnection = handle.connect();
    connection = nextConnection;
    nextConnection.on("turnAccepted", (accepted) => {
      if (activeGeneration !== generation) return;
      observeTurn(accepted.id, accepted.input);
      setStatus(accepted.replayed ? "resuming" : "running", "ok");
      setActivity((accepted.replayed ? "rejoined " : "started ") + shortId(accepted.id));
    });
    nextConnection.on("agentEvent", (event) => {
      if (activeGeneration !== generation) return;
      eventCount += 1;
      const kind = event && typeof event === "object" && "type" in event ? String(event.type) : "agent event";
      const delta = kind === "assistant.delta"
        && event.payload && typeof event.payload === "object" && "text" in event.payload
        ? event.payload.text
        : undefined;
      if (typeof delta === "string") {
        streamedText = (streamedText + delta).slice(-MAX_STREAMED_TEXT_BYTES);
        renderMessages();
      }
      setActivity(kind + " · " + eventCount + " events");
    });
    nextConnection.on("turnCompleted", (completed) => {
      if (activeGeneration === generation) completeTurn(completed.id, completed.final_message);
    });
    nextConnection.on("turnFailed", (failed) => {
      if (activeGeneration === generation) failTurn(failed.id, failed.error);
    });
    nextConnection.onStatusChange((status) => {
      if (activeGeneration === generation) setStatus(status, status === "connected" ? "ok" : "warn");
    });
    await nextConnection.ready;
    const [actorId, sessionStatus] = await Promise.all([handle.resolve(), handle.status()]);
    if (activeGeneration !== generation) return;
    ui.actor.textContent = actorId;
    for (const turn of sessionStatus.active_turn_details) observeTurn(turn.id, turn.input);
    setStatus(sessionStatus.active_turns.length > 0 ? "running" : "ready", "ok");
    setActivity(sessionStatus.completed_turns + " committed turns");
    if (state.pending) void runPending(handle, activeGeneration);
  } catch (error) {
    if (activeGeneration === generation) {
      setStatus("failed", "bad");
      setActivity(errorMessage(error), true);
    }
  }
}

async function runPending(active: Session, activeGeneration: number): Promise<void> {
  const pending = state.pending;
  if (!pending) return;
  try {
    const accepted = await active.start(pending);
    if (activeGeneration !== generation) return;
    observeTurn(accepted.id, accepted.input);
    setStatus(accepted.replayed ? "resuming" : "running", "ok");
    setActivity((accepted.replayed ? "rejoined " : "started ") + shortId(pending.id) + "; detach any time");
    const completed = await awaitDurableTurn(active, pending, activeGeneration);
    if (activeGeneration === generation) completeTurn(completed.id, completed.final_message);
  } catch (error) {
    if (activeGeneration === generation) failTurn(pending.id, errorMessage(error));
  }
}

async function awaitDurableTurn(
  active: Session,
  pending: PendingTurn,
  activeGeneration: number,
) {
  let lastError: unknown;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      return await active.turn(pending);
    } catch (error) {
      lastError = error;
      if (activeGeneration !== generation || state.pending?.id !== pending.id) throw error;
      const status = await active.status();
      if (!status.active_turns.includes(pending.id)) throw error;
      setStatus("reconnecting", "warn");
      setActivity("connection interrupted; rejoining " + shortId(pending.id));
    }
  }
  throw lastError;
}

async function detach(showMessage = true): Promise<void> {
  generation += 1;
  const oldConnection = connection;
  const oldClient = client;
  session = undefined;
  connection = undefined;
  client = undefined;
  await Promise.allSettled([oldConnection?.dispose(), oldClient?.dispose()]);
  if (showMessage) {
    setStatus(state.pending || state.activeTurns.length > 0 ? "detached · resumable" : "detached", "warn");
    setActivity("safe to close; inference and state remain with the actor");
  }
}

function observeTurn(id: string, input: string): void {
  const current = state.activeTurns.find((turn) => turn.id === id);
  if (current) current.input = input;
  else state.activeTurns.push({ id, input });
  if (!hasMessage("you", id)) state.messages.push({ role: "you", text: input, turnId: id });
  saveState();
  renderMessages();
}

function completeTurn(id: string, finalMessage: string): void {
  finishTurn(id);
  if (!hasMessage("agent", id)) {
    state.messages.push({ role: "agent", text: finalMessage, turnId: id });
  }
  streamedText = "";
  saveState();
  renderMessages();
  setStatus(state.activeTurns.length > 0 ? "running" : "ready", "ok");
  setActivity("committed durably · " + eventCount + " events");
}

function failTurn(id: string, error: string): void {
  finishTurn(id);
  if (!hasMessage("error", id)) state.messages.push({ role: "error", text: error, turnId: id });
  streamedText = "";
  saveState();
  renderMessages();
  setStatus("failed", "bad");
  setActivity(error, true);
}

function finishTurn(id: string): void {
  if (state.pending?.id === id) delete state.pending;
  state.activeTurns = state.activeTurns.filter((turn) => turn.id !== id);
}

function hasMessage(role: Message["role"], id: string): boolean {
  return state.messages.some((message) => message.role === role && message.turnId === id);
}

function renderMessages(): void {
  ui.transcript.replaceChildren();
  if (!state.messages.length && !streamedText) {
    const empty = document.createElement("article");
    empty.className = "system";
    empty.textContent = "Open this actor in another tab to watch prompts, streaming output, and durable results stay synchronized.";
    ui.transcript.append(empty);
  }
  for (const message of state.messages) {
    const article = document.createElement("article");
    article.className = message.role;
    const label = document.createElement("strong");
    label.textContent = message.role;
    const text = document.createElement("div");
    text.textContent = message.text;
    article.append(label, text);
    ui.transcript.append(article);
  }
  if (streamedText) {
    const article = document.createElement("article");
    article.className = "agent live";
    const label = document.createElement("strong");
    label.textContent = "agent · live";
    const text = document.createElement("div");
    text.textContent = streamedText;
    article.append(label, text);
    ui.transcript.append(article);
  }
  ui.transcript.scrollTop = ui.transcript.scrollHeight;
}

function freshState(endpoint: string): WebState {
  return {
    endpoint,
    actorKey: crypto.randomUUID(),
    activeTurns: [],
    messages: [],
  };
}

function loadState(): WebState | undefined {
  return parseStoredState(localStorage.getItem(STORAGE_KEY));
}

function parseStoredState(encoded: string | null): WebState | undefined {
  try {
    const value = JSON.parse(encoded ?? "null") as Partial<WebState> | null;
    if (!value || typeof value.endpoint !== "string" || typeof value.actorKey !== "string") return undefined;
    const pending = value.pending;
    return {
      endpoint: value.endpoint,
      actorKey: value.actorKey,
      activeTurns: Array.isArray(value.activeTurns)
        ? value.activeTurns.filter(isTurn).slice(-16)
        : [],
      messages: Array.isArray(value.messages)
        ? value.messages.filter(isMessage).slice(-50)
        : [],
      ...(pending && isTurn(pending) ? { pending } : {}),
    };
  } catch {
    return undefined;
  }
}

function syncStoredState(encoded: string | null): void {
  const incoming = parseStoredState(encoded);
  if (!incoming) return;
  const previousPendingId = state.pending?.id;
  const changedActor = incoming.endpoint !== state.endpoint || incoming.actorKey !== state.actorKey;
  state = incoming;
  ui.endpoint.value = state.endpoint;
  ui.actorKey.value = state.actorKey;
  renderMessages();
  if (changedActor) {
    ui.actor.textContent = "not resolved";
    void connect();
  } else if (previousPendingId !== state.pending?.id && session) {
    void runPending(session, generation);
  }
}

function saveState(): void {
  state.messages = state.messages.slice(-50);
  state.activeTurns = state.activeTurns.slice(-16);
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    state.messages = state.messages.slice(-10).map((message) => ({
      ...message,
      text: message.text.slice(-20_000),
    }));
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  }
}

function isTurn(value: unknown): value is ActiveTurn {
  return Boolean(value && typeof value === "object"
    && "id" in value && typeof value.id === "string"
    && "input" in value && typeof value.input === "string");
}

function isMessage(value: unknown): value is Message {
  if (!value || typeof value !== "object") return false;
  if (!("role" in value) || !["you", "agent", "error"].includes(String(value.role))) return false;
  if (!("text" in value) || typeof value.text !== "string") return false;
  return !("turnId" in value) || value.turnId === undefined || typeof value.turnId === "string";
}

function setStatus(text: string, tone?: "ok" | "warn" | "bad"): void {
  ui.status.textContent = text;
  ui.status.dataset.tone = tone ?? "";
}
function setActivity(text: string, bad = false): void {
  ui.activity.textContent = text;
  ui.activity.dataset.bad = bad ? "true" : "false";
}
function shortId(id: string): string { return id.slice(0, 8); }
function errorMessage(error: unknown): string { return error instanceof Error ? error.message : String(error); }
