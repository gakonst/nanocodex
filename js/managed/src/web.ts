const SECURITY_HEADERS = {
  "cache-control": "no-store",
  "cross-origin-opener-policy": "same-origin",
  "permissions-policy": "camera=(), microphone=(), geolocation=()",
  "referrer-policy": "no-referrer",
  "x-content-type-options": "nosniff",
  "x-frame-options": "DENY",
};

export function webAsset(pathname: string): Response | undefined {
  if (pathname === "/" || pathname === "/index.html") {
    return new Response(HTML, {
      headers: {
        ...SECURITY_HEADERS,
        "content-security-policy": "default-src 'none'; connect-src 'self' ws: wss:; script-src 'self'; style-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        "content-type": "text/html; charset=utf-8",
      },
    });
  }
  if (pathname === "/app.js") {
    return new Response(APP, {
      headers: { ...SECURITY_HEADERS, "content-type": "text/javascript; charset=utf-8" },
    });
  }
  if (pathname === "/app.css") {
    return new Response(CSS, {
      headers: { ...SECURITY_HEADERS, "content-type": "text/css; charset=utf-8" },
    });
  }
  return undefined;
}

const HTML = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Nanocodex · Managed Agent Operator</title>
  <link rel="stylesheet" href="/app.css">
</head>
<body>
  <main>
    <header>
      <div><p class="eyebrow">NANOCODEX / CLOUDFLARE</p><h1>Durable agent, disposable operator.</h1></div>
      <span id="status" class="pill">no agent</span>
    </header>
    <section class="setup">
      <label>Account API key <input id="api-key" type="password" autocomplete="off" spellcheck="false" placeholder="Paste an account-issued NANOCODEX_API_KEY"></label>
      <button id="new-agent" type="button">New agent</button>
      <button id="reconnect" type="button" class="secondary">Reconnect</button>
      <button id="detach" type="button" class="secondary">Detach</button>
    </section>
    <p class="meta">Agent <code id="agent">none</code>. The account key stays only in this tab's memory; local storage contains routing metadata, a bounded transcript, and any unfinished turn.</p>
    <section id="transcript" class="transcript" aria-live="polite">
      <article class="system">Paste an account-issued API key, create an agent, and send a prompt. Re-enter the key after reload to resume the same durable turn.</article>
    </section>
    <form id="prompt-form">
      <textarea id="prompt" rows="3" maxlength="1048576" placeholder="Ask the durable agent…" required></textarea>
      <button id="send" type="submit">Run durably</button>
    </form>
    <footer><span id="activity">idle</span><span>Rust/WASM · Durable Objects · Responses WebSocket</span></footer>
  </main>
  <script type="module" src="/app.js"></script>
</body>
</html>`;

const APP = `const STORAGE_KEY = "nanocodex.cloudflare.web.v2";
const byId = (id) => document.getElementById(id);
const ui = {
  activity: byId("activity"), apiKey: byId("api-key"), detach: byId("detach"),
  form: byId("prompt-form"), input: byId("prompt"), newAgent: byId("new-agent"),
  reconnect: byId("reconnect"), send: byId("send"), agent: byId("agent"),
  status: byId("status"), transcript: byId("transcript"),
};
let state = loadState();
let accountKey = "";
let streamAbort;
let reconnectTimer;
let streamGeneration = 0;
let submittingTurnId;
let ready = false;
let eventCount = 0;
let streamedText = "";

window.addEventListener("storage", (event) => {
  if (event.key === STORAGE_KEY) syncStoredState(event.newValue);
});
renderState();
if (state) setActivity("enter the account API key to reconnect");

ui.newAgent.addEventListener("click", async () => {
  if (!captureAccountKey()) return;
  setBusy(true);
  try {
    const response = await createAgentRequest("operator-create:" + crypto.randomUUID());
    if (response.status === 401) return rejectAccountKey();
    if (!response.ok) throw new Error("agent creation failed with HTTP " + response.status);
    const created = await response.json();
    if (!validAgentReceipt(created)) {
      throw new Error("agent creation returned an invalid receipt");
    }
    stopStream();
    state = {
      agent_id: created.agent_id,
      session_id: created.session_id,
      events_url: created.events_url,
      websocket_url: created.websocket_url,
      cursor: "0",
      messages: [],
    };
    saveState();
    renderState();
    void connect();
  } catch (error) {
    setActivity(errorMessage(error), true);
  } finally {
    setBusy(false);
  }
});

ui.reconnect.addEventListener("click", () => {
  if (!captureAccountKey()) return;
  void connect();
});
ui.detach.addEventListener("click", () => {
  stopStream();
  accountKey = "";
  ready = false;
  setStatus(state && state.pending ? "detached · turn running" : "detached", "warn");
  setActivity("detached and account key forgotten; durable state remains in the object");
});

ui.form.addEventListener("submit", (event) => {
  event.preventDefault();
  const input = ui.input.value.trim();
  if (!input || !state) return setActivity(state ? "prompt is empty" : "create an agent first", true);
  if (state.pending) return setActivity("one durable turn is already pending", true);
  state.pending = { id: crypto.randomUUID(), input };
  state.messages.push({ role: "you", text: input, turn_id: state.pending.id });
  ui.input.value = "";
  saveState();
  renderMessages();
  sendPending();
});

async function connect() {
  if (!state) return setActivity("create an agent first", true);
  if (!accountKey) return setActivity("account API key required", true);
  stopStream();
  const generation = ++streamGeneration;
  const abort = new AbortController();
  streamAbort = abort;
  ready = false;
  eventCount = 0;
  setStatus("connecting", "warn");
  try {
    const separator = state.events_url.includes("?") ? "&" : "?";
    const response = await fetch(state.events_url + separator + "cursor=" + encodeURIComponent(state.cursor || "0"), {
      headers: authHeaders({ accept: "text/event-stream" }),
      signal: abort.signal,
    });
    if (response.status === 401) return rejectAccountKey();
    if (response.status === 404) return forgetUnavailableAgent();
    if (!response.ok || !response.body) throw new Error("event stream failed with HTTP " + response.status);
    ready = true;
    setStatus("ready", "ok");
    setActivity("authenticated event stream connected");
    void sendPending();
    await consumeEvents(response.body, generation);
    if (!abort.signal.aborted && generation === streamGeneration) throw new Error("event stream ended");
  } catch (error) {
    if (abort.signal.aborted || generation !== streamGeneration) return;
    ready = false;
    setStatus(state && state.pending ? "detached · resumable" : "detached", "warn");
    setActivity(errorMessage(error), true);
    scheduleReconnect(generation);
  }
}

async function consumeEvents(body, generation) {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  while (generation === streamGeneration) {
    const chunk = await reader.read();
    if (chunk.done) break;
    buffer += decoder.decode(chunk.value, { stream: true });
    let boundary;
    while ((boundary = buffer.indexOf("\\n\\n")) >= 0) {
      const frame = buffer.slice(0, boundary);
      buffer = buffer.slice(boundary + 2);
      onEventFrame(frame);
    }
  }
}

function onEventFrame(frame) {
  const data = [];
  for (const line of frame.split("\\n")) {
    if (line.startsWith(": cursor ")) {
      updateCursor(line.slice(9));
      continue;
    }
    if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
  }
  if (!data.length) return;
  let message;
  try { message = JSON.parse(data.join("\\n")); }
  catch { return setActivity("invalid event stream message", true); }
  const cursor = typeof message.cursor === "string" ? message.cursor : undefined;
  onMessage(message);
  // Apply and persist the event before advancing its durable cursor. A crash
  // may replay an applied event, but must never skip an unapplied event.
  if (cursor) updateCursor(cursor);
}

function onMessage(message) {
  if (message.type === "agent_created") {
    setStatus("ready", "ok");
  } else if (message.type === "turn_accepted") {
    observeTurn(message.id, message.input);
    setStatus(message.replayed ? "resuming" : "running", "ok");
    setActivity((message.replayed ? "rejoined " : "started ") + shortId(message.id));
  } else if (message.type === "event") {
    eventCount += 1;
    const kind = message.event && message.event.type ? message.event.type : "agent event";
    const delta = kind === "assistant.delta" && message.event.payload && message.event.payload.text;
    if (typeof delta === "string") {
      streamedText = (streamedText + delta).slice(-1048576);
      renderMessages();
    }
    setActivity(kind + " · " + eventCount + " events");
  } else if (message.type === "turn_completed") {
    if (!state) return;
    finishTurn(message.id);
    if (!hasMessage("agent", message.id)) {
      state.messages.push({ role: "agent", text: message.final_message, turn_id: message.id });
    }
    streamedText = "";
    saveState();
    renderMessages();
    setStatus("ready", "ok");
    setActivity("committed durably · " + eventCount + " events");
  } else if (message.type === "turn_failed") {
    if (!state) return;
    finishTurn(message.id);
    if (!hasMessage("error", message.id)) {
      state.messages.push({ role: "error", text: message.error, turn_id: message.id });
    }
    streamedText = "";
    saveState();
    renderMessages();
    setStatus("failed", "bad");
    setActivity(message.error, true);
  } else if (message.type === "turn_cancelled") {
    if (!state) return;
    finishTurn(message.id);
    state.messages.push({ role: "error", text: "turn cancelled", turn_id: message.id });
    saveState();
    renderMessages();
    setStatus("ready", "ok");
  } else if (message.type === "turn_retryable") {
    setStatus("retrying", "warn");
    setActivity(message.error, true);
  } else if (message.type === "error") {
    setActivity(message.code + ": " + message.message, true);
  }
}

async function sendPending() {
  if (!ready || !state || !state.pending || !accountKey) return;
  if (submittingTurnId === state.pending.id) return;
  const pending = state.pending;
  submittingTurnId = pending.id;
  setStatus("running", "ok");
  setActivity("turn " + shortId(state.pending.id) + " is durable; detach any time");
  try {
    const turnsUrl = state.events_url.replace(/\\/events(?:\\?.*)?$/, "/turns");
    const response = await fetch(turnsUrl, {
      method: "POST",
      headers: authHeaders({
        "content-type": "application/json",
        "idempotency-key": pending.id,
      }),
      body: JSON.stringify({ id: pending.id, input: pending.input }),
    });
    if (response.status === 401) return rejectAccountKey();
    if (response.status !== 200 && response.status !== 202) {
      throw new Error("turn submission failed with HTTP " + response.status);
    }
  } catch (error) {
    setStatus("detached · resumable", "warn");
    setActivity(errorMessage(error), true);
  } finally {
    if (submittingTurnId === pending.id) submittingTurnId = undefined;
  }
}

function renderState() {
  ui.agent.textContent = state ? state.agent_id : "none";
  renderMessages();
  if (state && state.pending) {
    setStatus("pending · reconnecting", "warn");
    setActivity(accountKey
      ? "resuming unfinished turn " + shortId(state.pending.id)
      : "enter the account API key to resume unfinished turn " + shortId(state.pending.id));
  }
}

function renderMessages() {
  ui.transcript.replaceChildren();
  const messages = state && state.messages ? state.messages : [];
  if (!messages.length && !streamedText) {
    const empty = document.createElement("article");
    empty.className = "system";
    empty.textContent = "Send a prompt, detach during inference, then reload to prove the client is disposable.";
    ui.transcript.append(empty);
  }
  for (const message of messages) {
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

function loadState() {
  return parseStoredState(localStorage.getItem(STORAGE_KEY));
}

function parseStoredState(encoded) {
  try {
    const value = JSON.parse(encoded);
    if (!validAgentReceipt(value)) return undefined;
    value.messages = Array.isArray(value.messages) ? value.messages.slice(-50) : [];
    value.active_turns = Array.isArray(value.active_turns) ? value.active_turns.slice(-16) : [];
    return value;
  } catch { return undefined; }
}

function validAgentReceipt(value) {
  if (!value
    || typeof value.agent_id !== "string"
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value.agent_id)
    || value.session_id !== value.agent_id
    || typeof value.events_url !== "string"
    || typeof value.websocket_url !== "string") return false;
  try {
    const events = new URL(value.events_url);
    const socket = new URL(value.websocket_url);
    const route = "/v1/agents/" + value.agent_id;
    return events.origin === location.origin
      && events.pathname === route + "/events"
      && !events.search && !events.hash && !events.username && !events.password
      && socket.protocol === (location.protocol === "https:" ? "wss:" : "ws:")
      && socket.host === location.host
      && socket.pathname === route + "/ws"
      && !socket.search && !socket.hash && !socket.username && !socket.password;
  } catch { return false; }
}

function syncStoredState(encoded) {
  const incoming = parseStoredState(encoded);
  const previousPendingId = state && state.pending && state.pending.id;
  const changedAgent = Boolean((!state && incoming)
    || (state && !incoming)
    || (state && incoming && state.agent_id !== incoming.agent_id));
  if (changedAgent) stopStream();
  state = incoming;
  renderState();
  if (!state) return;
  if (accountKey && changedAgent) void connect();
  else if (accountKey && previousPendingId !== (state.pending && state.pending.id)) void sendPending();
}

function observeTurn(id, input) {
  if (!state || typeof id !== "string") return;
  let changed = false;
  if (!Array.isArray(state.active_turns)) {
    state.active_turns = [];
    changed = true;
  }
  const current = state.active_turns.find((turn) => turn && turn.id === id);
  if (current && JSON.stringify(current.input) !== JSON.stringify(input)) {
    current.input = input;
    changed = true;
  } else if (!current) {
    state.active_turns.push({ id, input });
    changed = true;
  }
  const text = displayInput(input);
  if (text && !hasMessage("you", id)) {
    state.messages.push({ role: "you", text, turn_id: id });
    changed = true;
  }
  if (changed) saveState();
  renderMessages();
}

function finishTurn(id) {
  if (!state) return;
  if (state.pending && state.pending.id === id) delete state.pending;
  state.active_turns = (state.active_turns || []).filter((turn) => turn && turn.id !== id);
}

function hasMessage(role, id) {
  return Boolean(state && state.messages.some((message) => message.role === role && message.turn_id === id));
}

function displayInput(input) {
  if (typeof input === "string") return input;
  if (!Array.isArray(input)) return "";
  return input.map((item) => {
    if (item && item.type === "text" && typeof item.text === "string") return item.text;
    return item && typeof item.type === "string" ? "[" + item.type + "]" : "[content]";
  }).join("\\n");
}

function saveState() {
  if (!state) return localStorage.removeItem(STORAGE_KEY);
  state.messages = state.messages.slice(-50);
  state.active_turns = (state.active_turns || []).slice(-16);
  try { localStorage.setItem(STORAGE_KEY, JSON.stringify(state)); }
  catch {
    state.messages = state.messages.slice(-10).map((message) => ({ ...message, text: message.text.slice(-20000) }));
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  }
}

function captureAccountKey() {
  const entered = ui.apiKey.value.trim();
  ui.apiKey.value = "";
  if (entered) accountKey = entered;
  if (accountKey) return true;
  setActivity("account API key required", true);
  return false;
}

function authHeaders(extra) {
  return { authorization: "Bearer " + accountKey, ...(extra || {}) };
}

async function createAgentRequest(idempotencyKey) {
  let failure;
  for (let attempt = 0; attempt < 8; attempt += 1) {
    try {
      const response = await fetch("/v1/agents", {
        method: "POST",
        headers: authHeaders({ "idempotency-key": idempotencyKey }),
      });
      if (response.status !== 408 && response.status !== 429 && response.status < 500) {
        return response;
      }
      failure = new Error("agent creation returned HTTP " + response.status);
      await response.body?.cancel();
    } catch (error) {
      failure = error;
    }
    if (attempt < 7) {
      await new Promise((resolve) => setTimeout(
        resolve,
        Math.floor(Math.random() * (Math.min(2_000, 250 * 2 ** attempt) + 1)),
      ));
    }
  }
  throw failure;
}

function rejectAccountKey() {
  accountKey = "";
  ready = false;
  stopStream();
  setStatus("authentication required", "bad");
  setActivity("account API key rejected", true);
}

function forgetUnavailableAgent() {
  stopStream();
  state = undefined;
  streamedText = "";
  saveState();
  renderState();
  setStatus("agent unavailable", "bad");
  setActivity("agent was deleted or is no longer owned by this account; create a new agent", true);
}

function updateCursor(cursor) {
  if (!state || !/^[0-9]+$/.test(cursor)) return;
  state.cursor = cursor;
  saveState();
}

function scheduleReconnect(generation) {
  if (!state || !accountKey || generation !== streamGeneration) return;
  clearTimeout(reconnectTimer);
  reconnectTimer = setTimeout(() => void connect(), 1000);
}

function stopStream() {
  clearTimeout(reconnectTimer);
  reconnectTimer = undefined;
  streamGeneration += 1;
  streamAbort?.abort();
  streamAbort = undefined;
  ready = false;
}

function setBusy(busy) { ui.newAgent.disabled = busy; }
function setStatus(text, tone) { ui.status.textContent = text; ui.status.dataset.tone = tone || ""; }
function setActivity(text, bad) { ui.activity.textContent = text; ui.activity.dataset.bad = bad ? "true" : "false"; }
function shortId(id) { return id.slice(0, 8); }
function errorMessage(error) { return error instanceof Error ? error.message : String(error); }
`;

const CSS = `:root{color-scheme:dark;--bg:#0b0d0c;--panel:#121614;--line:#28302b;--ink:#edf5ef;--muted:#89948d;--acid:#b8ff62;--red:#ff746c}*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at 80% 0,#193022 0,transparent 32rem),var(--bg);color:var(--ink);font:15px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace}main{width:min(940px,calc(100% - 32px));margin:0 auto;min-height:100vh;padding:48px 0 28px;display:grid;grid-template-rows:auto auto auto 1fr auto auto;gap:18px}header{display:flex;align-items:end;justify-content:space-between;gap:24px}h1{font:600 clamp(28px,6vw,54px)/1.03 system-ui,sans-serif;letter-spacing:-.045em;margin:5px 0}.eyebrow{color:var(--acid);font-size:12px;letter-spacing:.14em;margin:0}.pill{border:1px solid var(--line);border-radius:99px;padding:7px 11px;color:var(--muted);white-space:nowrap}.pill[data-tone=ok]{border-color:#466d35;color:var(--acid)}.pill[data-tone=bad],[data-bad=true]{color:var(--red)}.pill[data-tone=warn]{color:#ffd580}.setup{display:flex;gap:8px;align-items:end;flex-wrap:wrap}.setup label{display:grid;gap:5px;flex:1;min-width:240px;color:var(--muted);font-size:12px}input,textarea,button{font:inherit}input,textarea{width:100%;border:1px solid var(--line);border-radius:8px;background:#0d100f;color:var(--ink);padding:11px 12px;outline:none}input:focus,textarea:focus{border-color:#597f42;box-shadow:0 0 0 3px #b8ff6214}button{border:1px solid var(--acid);border-radius:8px;background:var(--acid);color:#10150d;padding:11px 14px;font-weight:700;cursor:pointer}button.secondary{background:transparent;color:var(--ink);border-color:var(--line)}button:disabled{opacity:.45;cursor:wait}.meta{color:var(--muted);font-size:12px;margin:0}.meta code{color:var(--ink);word-break:break-all}.transcript{min-height:280px;max-height:56vh;overflow:auto;border:1px solid var(--line);border-radius:12px;background:#101311d9;padding:14px}.transcript article{max-width:86%;padding:12px 14px;margin:8px 0;border-radius:9px;white-space:pre-wrap;overflow-wrap:anywhere}.transcript strong{display:block;color:var(--muted);font-size:10px;text-transform:uppercase;letter-spacing:.12em;margin-bottom:5px}.transcript .you{margin-left:auto;background:#24301f}.transcript .agent{background:#171c19;border:1px solid var(--line)}.transcript .system{color:var(--muted);font-style:italic}.transcript .error{border:1px solid #5b302d;color:#ffc0bb}form{display:grid;grid-template-columns:1fr auto;gap:9px;align-items:stretch}textarea{resize:vertical;min-height:78px}footer{display:flex;justify-content:space-between;gap:16px;color:var(--muted);font-size:11px}@media(max-width:620px){main{padding-top:24px}header{display:block}.pill{display:inline-block;margin-top:10px}form{grid-template-columns:1fr}footer{display:grid}.transcript article{max-width:96%}}`;
