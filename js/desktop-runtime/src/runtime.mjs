import { EventEmitter } from "node:events";
import { createHash, randomUUID } from "node:crypto";
import { spawn } from "node:child_process";
import { chmod, copyFile, mkdir, open, realpath, rename, rm, stat, writeFile } from "node:fs/promises";
import { constants } from "node:fs";
import { basename, dirname, isAbsolute, join } from "node:path";
import { homedir, hostname } from "node:os";
import { setTimeout as delay } from "node:timers/promises";
import { Agent } from "nanocodex/managed";
import { createTools } from "nanocodex/tools";
import * as Workspace from "nanocodex/node/workspace";
import { createNodeProcessTools } from "nanocodex-tools/node";
import WebSocket from "ws";

export const DEFAULT_ORIGIN = "https://nanocodex.gakonst.workers.dev";
export const DEFAULT_SETTINGS = Object.freeze({ model: "gpt-5.6-sol", thinking: "high", reasoning_mode: "standard", fast_mode: false });

export function validateSettings(settings) {
  if (!settings || typeof settings !== "object" || Array.isArray(settings)) throw new Error("Choose model settings first.");
  if (settings.model === "gpt-6-astra") {
    if (!["low", "medium", "high", "xhigh", "max"].includes(settings.thinking)) throw new Error("Astra supports Low through Max reasoning. Choose High to get started.");
    if (settings.reasoning_mode !== "standard") throw new Error("Astra uses Standard mode.");
  }
  return settings;
}

function desktopFetch(url, init = {}) {
  // Event watches already own an inactivity watchdog in the managed SDK. Every
  // other desktop request must finish or show an error instead of spinning.
  const streaming = new Headers(init.headers).get("accept") === "text/event-stream";
  const timeout = streaming ? undefined : AbortSignal.timeout(20_000);
  const signal = timeout ? (init.signal ? AbortSignal.any([init.signal, timeout]) : timeout) : init.signal;
  return fetch(url, { ...init, redirect: "error", ...(signal ? { signal } : {}) });
}

export function managedOrigin(value) {
  const url = new URL(value);
  if (url.username || url.password || url.search || url.hash || url.pathname !== "/"
    || (url.protocol !== "https:" && !(url.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)))) {
    throw new Error("Use an HTTPS service origin, or HTTP localhost for development.");
  }
  return url.origin;
}

export function validateHand(value) {
  if (!value || !["local", "vm"].includes(value.kind)) throw new Error("Choose a local workspace or VM Hand.");
  const id = value.id || `desktop-${randomUUID().slice(0, 8)}`;
  if (!/^[a-z0-9][a-z0-9._-]{0,62}$/.test(id) || ["brain", "sandbox", "tmp", "dev", "proc"].includes(id)) throw new Error("Use a short, lowercase machine ID.");
  if (typeof value.name !== "string" || !value.name.trim() || Buffer.byteLength(value.name) > 128) throw new Error("A machine name is required (up to 128 bytes).");
  if (typeof value.workspace !== "string" || !isAbsolute(value.workspace) || value.workspace.includes("\0")) throw new Error("Choose an absolute workspace path.");
  if (value.agentId && !/^[A-Za-z0-9._:-]{1,128}$/.test(value.agentId)) throw new Error("Invalid agent ID.");
  const config = { id, name: value.name.trim(), kind: value.kind, workspace: value.workspace, ...(value.agentId ? { agentId: value.agentId } : {}) };
  if (value.kind === "vm") {
    for (const name of ["rootfs", "guestRuntime", "binary"]) {
      if (typeof value[name] !== "string" || !isAbsolute(value[name]) || value[name].includes("\0")) throw new Error(`Choose an absolute ${name} path.`);
      config[name] = value[name];
    }
    if (!Number.isInteger(value.cpus) || value.cpus < 1 || value.cpus > 255) throw new Error("VM CPUs must be between 1 and 255.");
    if (!Number.isInteger(value.memoryMiB) || value.memoryMiB < 128 || value.memoryMiB > 1_048_576) throw new Error("Choose valid VM memory in MiB.");
    Object.assign(config, { cpus: value.cpus, memoryMiB: value.memoryMiB, network: value.network !== false });
    delete config.agentId; // The existing VM CLI attaches at account scope.
  }
  return config;
}

export function restoredLayout(value) {
  if (!value || !Array.isArray(value.tabs)) return undefined;
  const ids = new Set();
  const tabs = value.tabs.slice(0, 100).flatMap(tab => {
    if (!tab || typeof tab.id !== "string" || !tab.id || tab.id.length > 128 || ids.has(tab.id)) return [];
    ids.add(tab.id);
    const clean = { id: tab.id, draft: "", target: "", folder: "" };
    for (const key of ["threadId", "title", "draft", "target", "folder", "seenCursor"]) {
      if (typeof tab[key] === "string" && tab[key].length <= (key === "draft" ? 200_000 : 4096)) clean[key] = tab[key];
    }
    if (clean.threadId && !/^[A-Za-z0-9._:-]{1,128}$/.test(clean.threadId)) delete clean.threadId;
    if (clean.folder && !isAbsolute(clean.folder)) clean.folder = "";
    if (clean.seenCursor && !/^[0-9]{1,40}$/.test(clean.seenCursor)) delete clean.seenCursor;
    return [clean];
  });
  if (!tabs.length) return undefined;
  return {
    tabs,
    activeTabId: ids.has(value.activeTabId) ? value.activeTabId : tabs[0].id,
    tabPosition: value.tabPosition === "top" ? "top" : "left",
    theme: ["system", "light", "dark"].includes(value.theme) ? value.theme : "system",
  };
}

/** Desktop lifecycle owner. The renderer gets data and specific actions, never
 * a credential, fetch proxy, subprocess handle, or arbitrary IPC channel. */
export class DesktopRuntime extends EventEmitter {
  #options;
  #generation = 0;
  #threads = new Map();
  #resources = new Map();
  #persist;
  #saveConnection;
  #state;
  #closed = false;
  #connectionAttempt = 0;
  #accountTransition = Promise.resolve();
  #dataDirectory;
  #folderPreparations = new Map();
  #helperPreparations = new Map();

  constructor({ baseUrl = DEFAULT_ORIGIN, apiKey, saved = {}, defaults = {}, dataDirectory = join(homedir(), "Library", "Application Support", "Nanocodex", "Runtime"), persist = async () => {}, saveConnection = async () => {} } = {}) {
    super();
    if (!saved || typeof saved !== "object" || Array.isArray(saved)) saved = {};
    this.#persist = persist;
    this.#saveConnection = saveConnection;
    this.#dataDirectory = dataDirectory;
    this.#options = { baseUrl: managedOrigin(baseUrl), fetch: desktopFetch, ...(apiKey ? { apiKey } : {}) };
    this.#state = {
      connected: false, hasCredentials: Boolean(apiKey), baseUrl: this.#options.baseUrl, accountScope: randomUUID(), threads: [],
      hands: (Array.isArray(saved.hands) ? saved.hands : []).flatMap(config => {
        try { return [{ ...validateHand(config), status: "stopped", calls: 0, activeCalls: 0, logs: [] }]; }
        catch { return []; } // A stale preference must not prevent the app opening.
      }),
      layout: restoredLayout(saved.layout),
      defaults: { name: hostname().replace(/\.local$/i, "").slice(0, 100) || (process.platform === "darwin" ? "This Mac" : "This computer"), kind: "local", workspace: join(homedir(), "Nanocodex"), cpus: 2, memoryMiB: 2048, network: true, ...defaults },
      platform: process.platform, version: "0.1.0",
    };
  }

  state() { return structuredClone(this.#state); }
  #emit() { this.emit("event", { type: "state", state: this.state() }); }
  #emitThread(thread) {
    // One IPC snapshot per frame batch, even when the service emits a token burst.
    if (thread.emitTimer || thread.abort.signal.aborted) return;
    thread.emitTimer = setTimeout(() => {
      thread.emitTimer = undefined;
      if (!thread.abort.signal.aborted) this.emit("event", { type: "thread", thread: this.#snapshot(thread) });
    }, 32);
  }
  #snapshot(thread) {
    const { id, events, hasMore, connected, activeTurns, acceptedTurns, settings, error } = thread;
    return structuredClone({ id, events, hasMore, connected, activeTurns, acceptedTurns, settings, error });
  }
  #requireConnection() { if (this.#closed || !this.#options.apiKey || !this.#state.connected) throw new Error("Connect your Nanocodex account in Settings first."); }
  #safeError(error) { return String(error?.message ?? error).replaceAll(this.#options.apiKey || "\u0000", "[redacted]").replace(/ncx_live_[A-Za-z0-9_-]+/g, "[redacted]").slice(0, 500); }
  #sameAccount(generation) { if (this.#closed || generation !== this.#generation) throw new Error("The account changed while this operation was running."); }
  #transition(action) {
    const operation = this.#accountTransition.catch(() => {}).then(action);
    this.#accountTransition = operation;
    return operation;
  }
  async #save() { await this.#persist({ layout: this.#state.layout, hands: this.#state.hands.map(({ status, calls, activeCalls, error, logs, ...config }) => config) }); }

  async saveLayout(value) {
    // A UI may deliver a debounced message after the account has changed. The
    // scope belongs to the UI snapshot that created the message, not its arrival.
    if (value?.accountScope !== undefined && value.accountScope !== this.#state.accountScope) return;
    if (!value || !Array.isArray(value.tabs) || value.tabs.length > 100 || !["left", "top"].includes(value.tabPosition) || !["system", "light", "dark"].includes(value.theme)) throw new Error("Invalid tab layout.");
    const ids = new Set();
    const tabs = value.tabs.map(tab => {
      if (typeof tab.id !== "string" || tab.id.length > 128 || ids.has(tab.id)) throw new Error("Invalid tab.");
      ids.add(tab.id);
      const clean = { id: tab.id };
      for (const key of ["threadId", "title", "draft", "target", "folder", "seenCursor"]) {
        if (tab[key] !== undefined && (typeof tab[key] !== "string" || tab[key].length > (key === "draft" ? 200_000 : 4096))) throw new Error("Invalid tab content.");
        if (tab[key] !== undefined) clean[key] = tab[key];
      }
      return clean;
    });
    this.#state.layout = restoredLayout({ tabs, activeTabId: value.activeTabId, tabPosition: value.tabPosition, theme: value.theme });
    await this.#save();
  }

  async refresh() {
    if (this.#closed || !this.#options.apiKey) return this.state();
    const generation = this.#generation;
    try {
      const agents = await Agent.list(this.#options);
      if (generation !== this.#generation) return this.state();
      this.#state.threads = agents.map(agent => ({ id: agent.id, title: agent.summary?.title || "New thread", updatedAt: agent.summary?.updatedAt ?? 0, turnCount: agent.summary?.turnCount ?? 0 })).sort((a, b) => b.updatedAt - a.updatedAt);
      this.#state.connected = true;
      delete this.#state.error;
    } catch (error) {
      if (generation !== this.#generation) return this.state();
      this.#state.error = this.#safeError(error);
      if (error.status === 401 || error.status === 403) this.#state.connected = false;
    }
    this.#emit();
    return this.state();
  }

  async connect({ baseUrl, apiKey, remember = false }) {
    if (this.#closed) throw new Error("The desktop runtime is closed.");
    const attempt = ++this.#connectionAttempt;
    const origin = managedOrigin(baseUrl || DEFAULT_ORIGIN);
    if (!/^ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/.test(apiKey)) throw new Error("Enter an account-issued Nanocodex API key.");
    // Verify before replacing the current account. A failed login preserves it.
    const agents = await Agent.list({ baseUrl: origin, apiKey, fetch: desktopFetch });
    return this.#transition(async () => {
      if (this.#closed || attempt !== this.#connectionAttempt) throw new Error("A newer account connection replaced this request.");
      // Credential-store failure preserves the currently connected account.
      await this.#saveConnection({ baseUrl: origin, apiKey, remember });
      await this.#resetAccount();
      if (this.#closed) throw new Error("The desktop runtime is closed.");
      this.#options = { baseUrl: origin, apiKey, fetch: desktopFetch };
      this.#state.baseUrl = origin;
      this.#state.connected = true;
      this.#state.hasCredentials = true;
      this.#state.threads = agents.map(agent => ({ id: agent.id, title: agent.summary?.title || "New thread", updatedAt: agent.summary?.updatedAt ?? 0, turnCount: agent.summary?.turnCount ?? 0 })).sort((a, b) => b.updatedAt - a.updatedAt);
      // Authentication is committed once its credential is saved and adopted.
      // A later preference failure must not look like failed sign-in and cause
      // the client to revoke that already-active credential.
      try { await this.#save(); }
      catch { this.#state.error = "Signed in, but tab preferences could not be saved. Check available disk space."; }
      this.#emit();
      return this.state();
    });
  }

  async disconnect() {
    ++this.#connectionAttempt;
    return this.#transition(async () => {
      await this.#resetAccount();
      await this.#saveConnection(null);
      await this.#save();
      this.#emit();
      return this.state();
    });
  }

  async #resetAccount() {
    ++this.#generation;
    this.#state.accountScope = randomUUID();
    for (const id of this.#threads.keys()) this.closeThread(id);
    this.#state.connected = false;
    this.#state.hasCredentials = false;
    this.#options = { baseUrl: this.#options.baseUrl, fetch: desktopFetch };
    await Promise.all(this.#state.hands.map(hand => this.stopHand(hand.id)));
    this.#state.threads = [];
    // Configurations are explicit grants to the old account. Never transfer them.
    this.#state.hands = [];
    this.#state.layout = undefined;
    delete this.#state.error;
  }

  async request(path, init = {}) {
    this.#requireConnection();
    const generation = this.#generation;
    const response = await desktopFetch(new URL(path, this.#options.baseUrl), {
      ...init,
      headers: { "content-type": "application/json", authorization: `Bearer ${this.#options.apiKey}`, ...init.headers },
    });
    const body = await response.json();
    this.#sameAccount(generation);
    if (!response.ok) throw new Error(body.message || body.error || `Managed request failed (${response.status}).`);
    return body;
  }

  async createThread(settings = DEFAULT_SETTINGS) {
    this.#requireConnection();
    validateSettings(settings);
    const generation = this.#generation;
    const { agent_id } = await this.request("/v1/agents", { method: "POST", headers: { "idempotency-key": randomUUID() }, body: JSON.stringify({ settings }) });
    this.#sameAccount(generation);
    const agent = Agent.open(agent_id, this.#options);
    const thread = { id: agent.id, title: "New thread", updatedAt: Date.now(), turnCount: 0 };
    this.#state.threads.unshift(thread);
    this.#emit();
    return thread;
  }

  async openThread(id) {
    this.#requireConnection();
    const existing = this.#threads.get(id);
    if (existing) { await existing.ready; return this.#snapshot(existing); }
    const agent = Agent.open(id, this.#options);
    const thread = { id, agent, abort: new AbortController(), events: [], cursors: new Set(), hasMore: false, connected: false, activeTurns: [], acceptedTurns: 0, settings: { ...DEFAULT_SETTINGS }, cursor: "0" };
    this.#threads.set(id, thread);
    thread.ready = (async () => {
      const [page, state] = await Promise.all([agent.events.page({ limit: 256, signal: thread.abort.signal }), this.request(`/v1/agents/${encodeURIComponent(id)}`, { signal: thread.abort.signal })]);
      if (thread.abort.signal.aborted) throw new Error("Thread closed.");
      // SDK pages are immutable contract values; retain our own mutable array
      // before applying subsequent SSE events.
      thread.events = [...page.data];
      thread.cursors = new Set(page.data.map(event => event.cursor));
      thread.hasMore = page.hasMore;
      thread.cursor = page.latestCursor;
      thread.activeTurns = state.active_turns ?? [];
      thread.acceptedTurns = Math.max(state.accepted_turns ?? 0, thread.activeTurns.length, thread.events.filter(event => event.data.type === "turn_accepted").length);
      thread.settings = state.settings ?? { ...DEFAULT_SETTINGS };
      thread.connected = true;
      this.#emitThread(thread);
      void this.#watch(thread);
    })();
    try {
      await thread.ready;
      return this.#snapshot(thread);
    } catch (error) {
      thread.abort.abort();
      if (this.#threads.get(id) === thread) this.#threads.delete(id);
      throw error;
    }
  }

  async #watch(thread) {
    let backoff = 500;
    while (!thread.abort.signal.aborted) {
      try {
        for await (const event of thread.agent.events.watch({ cursor: thread.cursor, signal: thread.abort.signal })) {
          if (thread.abort.signal.aborted) return;
          thread.connected = true;
          delete thread.error;
          if (!thread.cursors.has(event.cursor)) {
            thread.cursors.add(event.cursor);
            thread.events.push(event);
            if (compareCursor(event.cursor, thread.cursor) >= 0) thread.cursor = event.cursor;
            else thread.events.sort((a, b) => compareCursor(a.cursor, b.cursor));
            if (event.data.type === "turn_accepted") {
              if (!thread.activeTurns.includes(event.data.id)) thread.activeTurns.push(event.data.id);
              thread.acceptedTurns = Math.max(thread.acceptedTurns, thread.events.filter(entry => entry.data.type === "turn_accepted").length);
            }
            if (["turn_completed", "turn_failed", "turn_cancelled"].includes(event.data.type)) {
              thread.activeTurns = thread.activeTurns.filter(id => id !== event.data.id);
              void this.refresh();
            }
            // Drop verbose completed-turn detail first, preserving all prompts and answers.
            if (thread.events.length > 4096) {
              const removable = thread.events.findIndex(e => e.data.type === "event" && !thread.activeTurns.includes(e.turnId));
              if (removable >= 0) thread.cursors.delete(thread.events.splice(removable, 1)[0].cursor);
            }
          }
          this.#emitThread(thread);
          backoff = 500;
        }
      } catch (error) {
        if (thread.abort.signal.aborted) return;
        thread.error = this.#safeError(error);
      }
      if (thread.abort.signal.aborted) return;
      thread.connected = false;
      this.#emitThread(thread);
      await delay(backoff, undefined, { signal: thread.abort.signal }).catch(() => {});
      backoff = Math.min(10_000, backoff * 2);
    }
  }

  closeThread(id) {
    const thread = this.#threads.get(id);
    if (!thread) return;
    clearTimeout(thread.emitTimer);
    thread.abort.abort();
    this.#threads.delete(id);
  }
  async older(id) {
    const thread = this.#threads.get(id);
    if (!thread || !thread.hasMore || thread.loadingOlder) return thread ? this.#snapshot(thread) : undefined;
    thread.loadingOlder = true;
    try {
      const page = await thread.agent.events.page({ before: thread.events[0]?.cursor, limit: 256, signal: thread.abort.signal });
      const seen = new Set(thread.events.map(event => event.cursor));
      thread.events = [...page.data.filter(event => !seen.has(event.cursor)), ...thread.events].sort((a, b) => compareCursor(a.cursor, b.cursor));
      for (const event of page.data) thread.cursors.add(event.cursor);
      thread.hasMore = page.hasMore;
      this.#emitThread(thread);
      return this.#snapshot(thread);
    } finally { thread.loadingOlder = false; }
  }

  async prompt({ agentId, input, requestId }) {
    this.#requireConnection();
    const generation = this.#generation;
    if (typeof input !== "string" || !input.trim() || input.length > 200_000) throw new Error("Enter a message of up to 200,000 characters.");
    const agent = Agent.open(agentId, this.#options);
    const turn = agent.turn.prompt({ input, id: requestId, idempotencyKey: requestId });
    const id = await turn.accepted();
    this.#sameAccount(generation);
    const thread = this.#threads.get(agentId);
    if (thread) {
      thread.acceptedTurns = Math.max(1, thread.acceptedTurns);
      this.#emitThread(thread);
    }
    void this.refresh();
    return id;
  }
  async steer({ agentId, turnId, input }) {
    if (typeof input !== "string" || !input.trim()) throw new Error("Enter a steering message.");
    await this.request(`/v1/agents/${encodeURIComponent(agentId)}/turns/${encodeURIComponent(turnId)}/steer`, { method: "POST", body: JSON.stringify({ input }) });
  }
  async cancel({ agentId, turnId }) {
    await this.request(`/v1/agents/${encodeURIComponent(agentId)}/turns/${encodeURIComponent(turnId)}/cancel`, { method: "POST" });
  }
  async settings({ agentId, settings }) {
    const thread = this.#threads.get(agentId);
    const state = thread ? { settings: thread.settings, accepted_turns: thread.acceptedTurns } : await this.request(`/v1/agents/${encodeURIComponent(agentId)}`);
    const current = state.settings;
    validateSettings({ ...current, ...settings });
    // Repeating immutable fields also fails after acceptance. Send only edits,
    // so changing effort or Fast keeps working in an existing conversation.
    const patch = Object.fromEntries(["model", "thinking", "reasoning_mode", "fast_mode"].filter(key => Object.hasOwn(settings, key) && settings[key] !== current[key]).map(key => [key, settings[key]]));
    if (state.accepted_turns > 0 && (Object.hasOwn(patch, "model") || Object.hasOwn(patch, "reasoning_mode"))) throw new Error("Start a new tab to change the model or reasoning mode.");
    if (!Object.keys(patch).length) return structuredClone(current);
    const { settings: updated } = await this.request(`/v1/agents/${encodeURIComponent(agentId)}/settings`, { method: "PATCH", body: JSON.stringify(patch) });
    if (thread) { thread.settings = updated; this.#emitThread(thread); }
    return updated;
  }

  async saveHand(input) {
    this.#requireConnection();
    const generation = this.#generation;
    const config = validateHand(input);
    if (config.kind === "local" && config.workspace === this.#state.defaults.workspace) await mkdir(config.workspace, { recursive: true, mode: 0o700 });
    if (config.kind === "local") {
      if (!(await stat(config.workspace)).isDirectory()) throw new Error("Choose an existing folder.");
      config.workspace = await realpath(config.workspace);
    }
    this.#sameAccount(generation);
    if (this.#resources.has(config.id)) throw new Error("Stop this Hand before changing it.");
    if (config.kind === "vm") {
      // A VM always receives its own writable image. The source may be a shared
      // immutable build cache; never pass it to the mutating VM CLI directly.
      const scope = createHash("sha256").update(`${this.#options.baseUrl}\0${this.#options.apiKey}`).digest("hex");
      const privateRoot = join(this.#dataDirectory, "hands", scope, config.id, "root.ext4");
      if (config.rootfs !== privateRoot) {
        await mkdir(dirname(privateRoot), { recursive: true, mode: 0o700 });
        try { await copyFile(config.rootfs, privateRoot, constants.COPYFILE_FICLONE | constants.COPYFILE_EXCL); }
        catch (error) {
          if (error.code === "EEXIST") throw new Error("This VM already has a workspace. Create a new VM to use another image.");
          throw error;
        }
        await chmod(privateRoot, 0o600);
        config.rootfs = privateRoot;
      }
      this.#sameAccount(generation);
    }
    const hand = { ...config, status: "stopped", calls: 0, activeCalls: 0, logs: [] };
    this.#state.hands = [...this.#state.hands.filter(h => h.id !== hand.id), hand];
    await this.#save(); this.#emit(); return this.state();
  }

  async prepareFolderHand({ agentId, workspace }) {
    this.#requireConnection();
    const generation = this.#generation;
    Agent.open(agentId, this.#options); // Validate the thread identifier first.
    if (typeof workspace !== "string" || !isAbsolute(workspace)) throw new Error("Choose a folder for this tab.");
    const folder = await realpath(workspace);
    if (!(await stat(folder)).isDirectory()) throw new Error("Choose a folder for this tab.");
    this.#sameAccount(generation);
    const key = `${generation}\0${agentId}\0${folder}`;
    if (this.#folderPreparations.has(key)) return this.#folderPreparations.get(key);
    const preparation = (async () => {
      let hand;
      const candidates = this.#state.hands.filter(candidate => candidate.kind === "local" && (!candidate.agentId || candidate.agentId === agentId))
        .sort((a, b) => Number(b.status === "connected") - Number(a.status === "connected"));
      for (const candidate of candidates) {
        if (await realpath(candidate.workspace).catch(() => null) === folder) { hand = candidate; break; }
      }
      this.#sameAccount(generation);
      if (!hand) {
        const config = validateHand({ kind: "local", name: basename(folder) || this.#state.defaults.name, workspace: folder, agentId });
        await this.saveHand(config);
        this.#sameAccount(generation);
        hand = this.#state.hands.find(candidate => candidate.id === config.id);
      }
      await this.startHand(hand.id);
      this.#sameAccount(generation);
      const connected = this.#state.hands.find(candidate => candidate.id === hand.id);
      if (connected?.status !== "connected") throw new Error(connected?.error || "The folder could not connect. Try sending again.");
      return structuredClone(connected);
    })();
    this.#folderPreparations.set(key, preparation);
    try { return await preparation; }
    finally { if (this.#folderPreparations.get(key) === preparation) this.#folderPreparations.delete(key); }
  }
  #log(hand, message) {
    hand.logs = [...hand.logs.slice(-99), `${new Date().toLocaleTimeString()}  ${message}`];
    this.#emit();
  }
  async startHand(id) {
    this.#requireConnection();
    const hand = this.#state.hands.find(hand => hand.id === id);
    if (!hand) throw new Error("Hand not found.");
    const existing = this.#resources.get(id);
    if (existing) { await existing.ready?.catch(() => {}); return this.state(); }
    hand.status = "connecting"; delete hand.error;
    const resource = { abort: new AbortController(), cleanups: [], ready: undefined };
    resource.add = close => {
      let closing;
      const once = () => closing ??= Promise.resolve().then(close);
      resource.cleanups.push(once);
      if (resource.abort.signal.aborted) void once().catch(() => {});
    };
    resource.close = () => Promise.all(resource.cleanups.map(close => close()));
    this.#resources.set(id, resource);
    this.#log(hand, "Connecting Hand…");
    try {
      resource.ready = hand.kind === "vm" ? this.#startVm(hand, resource) : this.#startLocal(hand, resource);
      await resource.ready;
    } catch (error) {
      const stopped = resource.abort.signal.aborted;
      resource.abort.abort();
      await resource.close().catch(() => {});
      if (!stopped) { hand.status = "error"; hand.error = this.#safeError(error); this.#log(hand, hand.error); }
      if (this.#resources.get(id) === resource) this.#resources.delete(id);
    }
    this.#emit(); return this.state();
  }
  async #startLocal(hand, resource) {
    const processes = await createNodeProcessTools({ workspace: hand.workspace, onActivity: event => {
      if (event.type === "started") { hand.calls++; hand.activeCalls++; }
      else hand.activeCalls = Math.max(0, hand.activeCalls - 1);
      this.#log(hand, event.type === "started" ? `Executing command · process ${event.processId}` : `Process ${event.processId} exited (${event.exitCode})`);
    } });
    resource.add(processes.close);
    resource.abort.signal.throwIfAborted();
    const workspace = await Workspace.open({ path: hand.workspace, root: hand.workspace });
    resource.abort.signal.throwIfAborted();
    const tools = await createTools({ tools: processes.tools, workspace, attachmentId: hand.id, machines: [{ id: hand.id, name: hand.name, workspace: hand.workspace, capabilities: ["native", "shell", "filesystem", "process", "pipes"] }] });
    resource.add(() => tools.close());
    resource.abort.signal.throwIfAborted();
    const endpoint = new URL(hand.agentId ? `/v1/agents/${encodeURIComponent(hand.agentId)}/tool-host` : "/v1/account/tool-host", this.#options.baseUrl);
    endpoint.protocol = endpoint.protocol === "https:" ? "wss:" : "ws:";
    const apiKey = this.#options.apiKey;
    const attachment = tools.attach({ endpoint, transport: { connect: target => new WebSocket(target, { headers: { authorization: `Bearer ${apiKey}` } }) } });
    const connection = await attachment.connect();
    resource.abort.signal.throwIfAborted();
    hand.status = "connected";
    this.#log(hand, hand.agentId ? "Connected to the selected thread." : "Connected to your account. Available to all your agents.");
    const monitor = setInterval(() => {
      if (resource.abort.signal.aborted) return;
      const status = connection.connected ? "connected" : "connecting";
      if (hand.status !== status) {
        hand.status = status;
        this.#log(hand, status === "connected" ? "Connection restored." : "Reconnecting Hand…");
      }
    }, 500);
    monitor.unref();
    resource.add(() => clearInterval(monitor));
    void (async () => {
      while (!resource.abort.signal.aborted) {
        await attachment.closed();
        if (resource.abort.signal.aborted) return;
        // A retired lease must not fight its replacement. Reconnection is explicit.
        hand.status = "stopped";
        this.#log(hand, "Connection closed. Start the Hand to reconnect.");
        resource.abort.abort();
        await resource.close();
        if (this.#resources.get(hand.id) === resource) this.#resources.delete(hand.id);
        return;
      }
    })().catch(error => {
      if (this.#resources.get(hand.id) !== resource) return;
      resource.abort.abort();
      hand.status = "error"; hand.error = this.#safeError(error); this.#emit();
      void resource.close().finally(() => { if (this.#resources.get(hand.id) === resource) this.#resources.delete(hand.id); });
    });
  }
  async #startVm(hand, resource) {
    for (const path of [hand.binary, hand.rootfs, hand.guestRuntime]) await stat(path);
    const binary = await this.#prepareVmHelper(hand.binary);
    const cache = join(this.#dataDirectory, "vm-cache");
    await mkdir(cache, { recursive: true, mode: 0o700 });
    resource.abort.signal.throwIfAborted();
    const args = ["hand", "--vm", hand.rootfs, "--vm-guest-runtime", hand.guestRuntime, "--vm-cache", cache, "--vm-workspace", hand.workspace, "--vm-cpus", String(hand.cpus), "--vm-memory-mib", String(hand.memoryMiB), "--machine-id", hand.id, "--machine-name", hand.name, "--log-format", "json"];
    if (!hand.network) args.push("--vm-no-network");
    const env = Object.fromEntries(["PATH", "HOME", "TMPDIR", "LANG", "NANOCODEX_KRUNFW_DIR"].filter(key => process.env[key]).map(key => [key, process.env[key]]));
    Object.assign(env, { NANOCODEX_API_KEY: this.#options.apiKey, NANOCODEX_MANAGED_URL: this.#options.baseUrl });
    const child = spawn(binary, args, { env, detached: process.platform !== "win32", stdio: ["ignore", "pipe", "pipe"] });
    let settle;
    const closed = new Promise(resolve => { settle = resolve; });
    let readyResolve;
    let readyReject;
    const ready = new Promise((resolve, reject) => { readyResolve = resolve; readyReject = reject; });
    const consume = () => {
      let buffer = "";
      return chunk => {
        buffer = (buffer + chunk.toString()).slice(-32_768);
        const lines = buffer.split("\n"); buffer = lines.pop();
        for (const line of lines) {
          try {
            const entry = JSON.parse(line); const stage = entry.fields?.stage;
            if (resource.abort.signal.aborted) continue;
            if (stage === "vm.hand.ready") { hand.status = "connected"; readyResolve(); }
            if (typeof stage === "string") this.#log(hand, stage);
            if (entry.level === "ERROR" && entry.fields?.error) hand.error = this.#safeError(entry.fields.error);
          } catch {
            // The CLI reports a failed launch as a plain final Error line.
            // Surface that reason without arbitrary guest output or secrets.
            if (!resource.abort.signal.aborted && line.startsWith("Error: ")) {
              hand.error = this.#safeError(line.slice(7));
              this.#log(hand, hand.error);
            }
          }
        }
      };
    };
    child.stdout.on("data", consume()); child.stderr.on("data", consume());
    child.on("error", error => { readyReject(error); });
    child.on("close", code => {
      settle();
      readyReject(new Error(hand.error || `VM Hand exited (${code}) before it was ready. Check the runtime, firmware, and root image.`));
      if (!resource.abort.signal.aborted) { hand.status = code === 0 ? "stopped" : "error"; if (code !== 0) hand.error ||= `VM Hand exited (${code}). Check the runtime, firmware, and root image.`; }
      if (this.#resources.get(hand.id) === resource) this.#resources.delete(hand.id);
      this.#emit();
    });
    const signalChild = signal => {
      if (!child.pid) return;
      try { process.kill(process.platform === "win32" ? child.pid : -child.pid, signal); }
      catch (error) { if (error.code !== "ESRCH") throw error; }
    };
    resource.add(async () => {
      if (child.exitCode !== null || child.signalCode !== null) return;
      signalChild("SIGINT");
      await Promise.race([closed, delay(20_000, undefined, { ref: false })]);
      if (child.exitCode === null && child.signalCode === null) { signalChild("SIGTERM"); await Promise.race([closed, delay(2_000, undefined, { ref: false })]); }
      if (child.exitCode === null && child.signalCode === null) signalChild("SIGKILL");
      await closed;
    });
    const timeout = setTimeout(() => readyReject(new Error("The VM did not become ready within 60 seconds. Check its image and runtime.")), 60_000);
    const abort = () => readyReject(resource.abort.signal.reason);
    resource.abort.signal.addEventListener("abort", abort, { once: true });
    try { resource.abort.signal.throwIfAborted(); await ready; }
    finally { clearTimeout(timeout); resource.abort.signal.removeEventListener("abort", abort); }
  }

  async #prepareVmHelper(source) {
    if (process.platform !== "darwin") return source;
    // An explicitly selected script may delegate to an installed signed helper.
    // Entitlements apply to native Mach-O executables, not shell wrappers.
    const file = await open(source, "r");
    let magic;
    try { const header = Buffer.alloc(4); await file.read(header, 0, 4, 0); magic = header.toString("hex"); }
    finally { await file.close(); }
    if (!["feedface", "feedfacf", "cefaedfe", "cffaedfe", "cafebabe", "bebafeca", "cafebabf", "bfbafeca"].includes(magic)) return source;
    const info = await stat(source);
    const identity = createHash("sha256").update(`${source}\0${info.size}\0${info.mtimeMs}`).digest("hex");
    if (this.#helperPreparations.has(identity)) return this.#helperPreparations.get(identity);
    const preparation = (async () => {
      if (await signedForVm(source)) return source;
      const folder = join(this.#dataDirectory, "helpers", identity);
      const binary = join(folder, "nanocodex2");
      if (await signedForVm(binary)) return binary;
      await mkdir(folder, { recursive: true, mode: 0o700 });
      const temporary = join(folder, `nanocodex2-${randomUUID()}`);
      const entitlement = join(folder, "vm.entitlements");
      try {
        await copyFile(source, temporary, constants.COPYFILE_FICLONE | constants.COPYFILE_EXCL);
        await chmod(temporary, 0o700);
        await writeFile(entitlement, '<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>', { mode: 0o600 });
        const result = await nativeCommand("/usr/bin/codesign", ["--force", "--sign", "-", "--entitlements", entitlement, temporary]);
        if (result.code !== 0 || !await signedForVm(temporary)) throw new Error("This VM helper could not be prepared. Choose the installed Nanocodex release helper.");
        await rename(temporary, binary);
        return binary;
      } finally { await rm(temporary, { force: true }); }
    })();
    this.#helperPreparations.set(identity, preparation);
    try { return await preparation; }
    catch (error) { this.#helperPreparations.delete(identity); throw error; }
  }
  async stopHand(id) {
    const resource = this.#resources.get(id);
    if (resource) {
      resource.abort.abort();
      // Close acquired resources immediately to unblock a pending handshake,
      // then wait until setup can no longer acquire additional resources.
      await Promise.all([resource.close(), resource.ready?.catch(() => {})]);
      await resource.close();
      if (this.#resources.get(id) === resource) this.#resources.delete(id);
    }
    const hand = this.#state.hands.find(hand => hand.id === id);
    if (hand) { hand.status = "stopped"; hand.activeCalls = 0; this.#log(hand, "Stopped. Compute is no longer available to agents."); }
    return this.state();
  }
  async removeHand(id) {
    await this.stopHand(id);
    this.#state.hands = this.#state.hands.filter(hand => hand.id !== id);
    await this.#save(); this.#emit(); return this.state();
  }
  async close() {
    if (this.#closed) return this.#accountTransition;
    this.#closed = true;
    ++this.#connectionAttempt;
    ++this.#generation;
    for (const id of this.#threads.keys()) this.closeThread(id);
    await Promise.all(this.#state.hands.map(hand => this.stopHand(hand.id)));
    await this.#accountTransition.catch(() => {});
  }
}

export function compareCursor(a, b) { return a.length - b.length || (a < b ? -1 : a > b ? 1 : 0); }

async function signedForVm(binary) {
  const entitlement = await nativeCommand("/usr/bin/codesign", ["--display", "--entitlements", "-", "--xml", binary]);
  if (entitlement.code !== 0 || !/<key>com\.apple\.security\.hypervisor<\/key>\s*<true\s*\/>/.test(entitlement.output)) return false;
  return (await nativeCommand("/usr/bin/codesign", ["--verify", "--strict", binary])).code === 0;
}

async function nativeCommand(command, args) {
  const environment = Object.fromEntries(["PATH", "HOME", "TMPDIR", "LANG", "SYSTEMROOT"].filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
  const child = spawn(command, args, { env: environment, stdio: ["ignore", "pipe", "pipe"] });
  let output = "";
  for (const stream of [child.stdout, child.stderr]) stream.on("data", chunk => { output = (output + chunk).slice(-32_768); });
  const timeout = setTimeout(() => child.kill("SIGKILL"), 10_000);
  try {
    return await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", code => resolve({ code, output }));
    });
  } finally { clearTimeout(timeout); }
}
