const STORAGE_KEY = "nanocodex.vercel.workflow.web.v1";
const CLIENT_KEY = "nanocodex.vercel.workflow.client";
const TERMINAL_SESSION_EVENT = "nanocodex:workflow-session";
const AGENT_TERMINAL_EVENT = "nanocodex:agent-terminal-snapshot";
const AGENT_TERMINAL_READY_EVENT = "nanocodex:agent-terminal-ready";
const MAX_STREAMED_TEXT = 1024 * 1024;
const byId = (id) => document.getElementById(id);
const ui = {
  activity: byId("activity"),
  adminToken: byId("admin-token"),
  copySession: byId("copy-session"),
  detach: byId("detach"),
  form: byId("prompt-form"),
  input: byId("prompt"),
  joinSession: byId("join-session"),
  newSession: byId("new-session"),
  send: byId("send"),
  session: byId("session"),
  sessionId: byId("session-id"),
  status: byId("status"),
};

let state = loadState() ?? freshState();
let clientId = sessionStorage.getItem(CLIENT_KEY);
if (!clientId) {
  clientId = crypto.randomUUID();
  sessionStorage.setItem(CLIENT_KEY, clientId);
}
let socket;
let generation = 0;
let reconnectTimer;
let reconnectDelay = 500;
let detached = false;
let connected = false;
let streamedText = "";
let eventCount = 0;
let nextIndex = loadCursor(state.sessionId);

renderState();
if (state.sessionId) connect();

window.addEventListener("storage", (event) => {
  if (event.key === STORAGE_KEY) syncStoredState(event.newValue);
});
window.addEventListener(AGENT_TERMINAL_READY_EVENT, renderMessages);

ui.newSession.addEventListener("click", () => void createSession());
ui.joinSession.addEventListener("click", () => joinSession(ui.sessionId.value.trim()));
ui.copySession.addEventListener("click", () => void copySession());
ui.detach.addEventListener("click", () => detach(true));
ui.form.addEventListener("submit", (event) => {
  event.preventDefault();
  const input = ui.input.value.trim();
  if (!state.sessionId) return setActivity("create or join a workflow first", true);
  if (!state.actorReady || !connected) return setActivity("workflow stream is not ready", true);
  if (!input) return setActivity("prompt is empty", true);
  if (state.pending) return setActivity("one durable turn is already pending", true);
  const pending = { id: crypto.randomUUID(), input, owner: clientId };
  state.pending = pending;
  observeTurn(pending.id, pending.input);
  ui.input.value = "";
  saveState();
  void sendPending();
});

async function createSession() {
  setBusy(true);
  setStatus("creating", "warn");
  try {
    const token = ui.adminToken.value.trim();
    const response = await fetch("/api/sessions", {
      method: "POST",
      headers: token ? { authorization: "Bearer " + token } : {},
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body?.error?.message ?? `session creation failed with HTTP ${response.status}`);
    ui.adminToken.value = "";
    joinSession(body.session_id);
  } catch (error) {
    setStatus("failed", "bad");
    setActivity(errorMessage(error), true);
  } finally {
    setBusy(false);
  }
}

function joinSession(sessionId) {
  if (!/^[A-Za-z0-9_-]{1,200}$/.test(sessionId)) {
    return setActivity("enter a valid Workflow session ID", true);
  }
  detach(false);
  state = freshState(sessionId);
  nextIndex = 0;
  saveCursor(sessionId, nextIndex);
  saveState();
  streamedText = "";
  eventCount = 0;
  renderState();
  connect();
}

function connect() {
  if (!state.sessionId) return;
  clearTimeout(reconnectTimer);
  detached = false;
  connected = false;
  const activeGeneration = ++generation;
  setStatus("connecting", "warn");
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  const url = new URL(`${protocol}//${location.host}/api/ws`);
  url.searchParams.set("sessionId", state.sessionId);
  url.searchParams.set("startIndex", String(nextIndex));
  socket = new WebSocket(url);
  socket.addEventListener("open", () => setActivity("connected; attaching to durable workflow stream"));
  socket.addEventListener("message", (message) => {
    if (activeGeneration === generation) onSocketMessage(message.data);
  });
  socket.addEventListener("close", () => {
    if (activeGeneration !== generation) return;
    connected = false;
    renderControls();
    if (detached) return;
    setStatus(state.pending ? "reconnecting · turn durable" : "reconnecting", "warn");
    reconnectTimer = setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 10_000);
  });
  socket.addEventListener("error", () => socket?.close());
}

function onSocketMessage(encoded) {
  let message;
  try { message = JSON.parse(encoded); } catch { return setActivity("invalid server message", true); }
  if (message.type === "stream_ready") {
    connected = true;
    reconnectDelay = 500;
    setStatus(state.actorReady ? (state.pending ? "running" : "ready") : "initializing", state.actorReady ? "ok" : "warn");
    setActivity(`durable stream attached at event ${nextIndex}`);
    renderControls();
    if (state.pending?.owner === clientId) void sendPending();
    return;
  }
  if (message.type === "stream_error") {
    setStatus("failed", "bad");
    setActivity(message.message || "workflow stream failed", true);
    return;
  }
  if (message.type !== "stream_event" || !Number.isSafeInteger(message.index)) return;
  if (message.index < nextIndex) return;
  if (message.index > nextIndex) {
    setActivity(`stream gap at event ${nextIndex}; reconnecting`, true);
    socket?.close(1012, "resume from missing event");
    return;
  }
  nextIndex = message.index + 1;
  saveCursor(state.sessionId, nextIndex);
  onSessionEvent(message.event);
}

function onSessionEvent(message) {
  if (!message || typeof message !== "object") return;
  if (message.type === "ready") {
    state.actorReady = true;
    setStatus(state.pending ? "running" : "ready", "ok");
    setActivity("workflow actor ready");
    saveState();
    renderControls();
    if (state.pending?.owner === clientId) void sendPending();
  } else if (message.type === "turn_accepted") {
    observeTurn(message.id, message.input);
    setStatus(message.replayed ? "resuming" : "running", "ok");
    setActivity((message.replayed ? "rejoined " : "started ") + shortId(message.id));
  } else if (message.type === "event") {
    eventCount += 1;
    const kind = message.event?.type ?? "agent event";
    const delta = kind === "assistant.delta" ? message.event?.payload?.text : undefined;
    if (typeof delta === "string") {
      streamedText = (streamedText + delta).slice(-MAX_STREAMED_TEXT);
      renderMessages();
    }
    setActivity(kind + " · " + eventCount + " events");
  } else if (message.type === "turn_completed") {
    finishTurn(message.id);
    if (!hasMessage("agent", message.id)) {
      state.messages.push({ role: "agent", text: message.final_message, turnId: message.id });
    }
    streamedText = "";
    saveState();
    renderMessages();
    setStatus("ready", "ok");
    setActivity("committed durably · " + eventCount + " events");
  } else if (message.type === "turn_failed") {
    finishTurn(message.id);
    if (!hasMessage("error", message.id)) {
      state.messages.push({ role: "error", text: message.error, turnId: message.id });
    }
    streamedText = "";
    saveState();
    renderMessages();
    setStatus("failed", "bad");
    setActivity(message.error, true);
  }
}

async function sendPending() {
  const pending = state.pending;
  if (!pending || pending.owner !== clientId || !state.sessionId || !state.actorReady) return;
  try {
    const response = await fetch(`/api/sessions/${encodeURIComponent(state.sessionId)}/prompt`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id: pending.id, input: pending.input }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body?.error?.message ?? `prompt failed with HTTP ${response.status}`);
    setStatus("running", "ok");
    setActivity("turn " + shortId(pending.id) + " accepted by workflow hook; detach any time");
  } catch (error) {
    setActivity(errorMessage(error), true);
  }
}

function detach(showMessage) {
  detached = true;
  connected = false;
  generation += 1;
  clearTimeout(reconnectTimer);
  if (socket && socket.readyState < WebSocket.CLOSING) socket.close(1000, "client detached");
  socket = undefined;
  renderControls();
  if (showMessage) {
    setStatus(state.pending ? "detached · turn durable" : "detached", "warn");
    setActivity("safe to close; Workflow owns inference and state");
  }
}

function observeTurn(id, input) {
  if (typeof id !== "string" || typeof input !== "string") return;
  const active = state.activeTurns.find((turn) => turn.id === id);
  if (active) active.input = input;
  else state.activeTurns.push({ id, input });
  if (!hasMessage("you", id)) state.messages.push({ role: "you", text: input, turnId: id });
  saveState();
  renderMessages();
}

function finishTurn(id) {
  if (state.pending?.id === id) delete state.pending;
  state.activeTurns = state.activeTurns.filter((turn) => turn.id !== id);
  renderControls();
}

function hasMessage(role, id) {
  return state.messages.some((message) => message.role === role && message.turnId === id);
}

function renderState() {
  ui.session.textContent = state.sessionId || "none";
  ui.sessionId.value = state.sessionId;
  renderMessages();
  renderControls();
  if (!state.sessionId) setStatus("no session");
  else if (state.pending) setStatus("pending · reconnecting", "warn");
  announceSession();
}

function renderMessages() {
  window.dispatchEvent(new CustomEvent(AGENT_TERMINAL_EVENT, {
    detail: {
      messages: state.messages.map(({ role, text }) => ({ role, text })),
      streamedText,
    },
  }));
}

function renderControls() {
  ui.send.disabled = !state.sessionId || !state.actorReady || !connected || Boolean(state.pending);
  ui.copySession.disabled = !state.sessionId;
}

function freshState(sessionId = "") {
  return { sessionId, actorReady: false, activeTurns: [], messages: [] };
}

function loadState() {
  return parseState(localStorage.getItem(STORAGE_KEY));
}

function parseState(encoded) {
  try {
    const value = JSON.parse(encoded ?? "null");
    if (!value || typeof value !== "object" || typeof value.sessionId !== "string") return undefined;
    return {
      sessionId: value.sessionId,
      actorReady: value.actorReady === true,
      activeTurns: Array.isArray(value.activeTurns) ? value.activeTurns.filter(isTurn).slice(-16) : [],
      messages: Array.isArray(value.messages) ? value.messages.filter(isMessage).slice(-50) : [],
      ...(isPending(value.pending) ? { pending: value.pending } : {}),
    };
  } catch { return undefined; }
}

function syncStoredState(encoded) {
  const incoming = parseState(encoded);
  if (!incoming) return;
  const changedSession = incoming.sessionId !== state.sessionId;
  state = incoming;
  ui.session.textContent = state.sessionId || "none";
  ui.sessionId.value = state.sessionId;
  renderMessages();
  renderControls();
  if (changedSession) {
    detach(false);
    nextIndex = loadCursor(state.sessionId);
    if (state.sessionId) connect();
    announceSession();
  }
}

function announceSession() {
  window.dispatchEvent(new CustomEvent(TERMINAL_SESSION_EVENT, {
    detail: { sessionId: state.sessionId },
  }));
}

function saveState() {
  state.messages = state.messages.slice(-50);
  state.activeTurns = state.activeTurns.slice(-16);
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    state.messages = state.messages.slice(-10).map((message) => ({ ...message, text: message.text.slice(-20_000) }));
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  }
  renderControls();
}

function cursorKey(sessionId) { return `nanocodex.vercel.workflow.cursor:${sessionId}`; }
function loadCursor(sessionId) {
  if (!sessionId) return 0;
  const value = Number(sessionStorage.getItem(cursorKey(sessionId)) ?? 0);
  return Number.isSafeInteger(value) && value >= 0 ? value : 0;
}
function saveCursor(sessionId, value) {
  if (sessionId) sessionStorage.setItem(cursorKey(sessionId), String(value));
}

function isTurn(value) {
  return Boolean(value && typeof value === "object" && typeof value.id === "string" && typeof value.input === "string");
}
function isPending(value) {
  return isTurn(value) && typeof value.owner === "string";
}
function isMessage(value) {
  return Boolean(value && typeof value === "object"
    && ["you", "agent", "error"].includes(value.role)
    && typeof value.text === "string"
    && (value.turnId === undefined || typeof value.turnId === "string"));
}

async function copySession() {
  if (!state.sessionId) return;
  try {
    await navigator.clipboard.writeText(state.sessionId);
    setActivity("workflow session ID copied");
  } catch {
    ui.sessionId.select();
    setActivity("copy unavailable; session ID selected", true);
  }
}

function setBusy(busy) {
  ui.newSession.disabled = busy;
  ui.joinSession.disabled = busy;
}
function setStatus(text, tone) {
  ui.status.textContent = text;
  ui.status.dataset.tone = tone ?? "";
}
function setActivity(text, bad = false) {
  ui.activity.textContent = text;
  ui.activity.dataset.bad = bad ? "true" : "false";
}
function shortId(id) { return String(id).slice(0, 8); }
function errorMessage(error) { return error instanceof Error ? error.message : String(error); }
