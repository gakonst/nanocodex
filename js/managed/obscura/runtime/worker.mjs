import variant from "@jitl/quickjs-wasmfile-release-sync";
import {
  newQuickJSWASMModuleFromVariant,
  newVariant,
} from "quickjs-emscripten-core";
import quickjsModule from "quickjs.wasm";
import domModule from "dom.wasm";
import { initSync, WasmDom } from "./obscura_dom_wasm.js";
import { createObscuraHost } from "./obscura-host.mjs";
import { installCompatibility } from "./compat.mjs";
import { source as compatibilitySource } from "./compat-source.mjs";
import { BrowserSession, attachPageSession } from "./browser-session.mjs";
import bootstrap from "bootstrap-source";
import { ObscuraBrowser } from "./browser.mjs";
import { protocol } from "./protocol.mjs";
// Bound responses while accommodating modern application script bundles.
const MAX_RESPONSE_BYTES = 16 * 1024 * 1024;
let initialized;
const sessions = new Map();
function initialize() {
  return (initialized ??= (async () => {
    initSync({ module: domModule });
    return newQuickJSWASMModuleFromVariant(
      newVariant(variant, { wasmModule: quickjsModule }),
    );
  })());
}
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/json/protocol") return Response.json(protocol);
    if (url.pathname === "/json/version")
      return Response.json({
        Browser: "Obscura/Wasm-experimental",
        "Protocol-Version": "1.3",
        webSocketDebuggerUrl: `${url.protocol === "https:" ? "wss:" : "ws:"}//${url.host}/v1/devtools/browser`,
      });
    const route =
      /^\/v1\/devtools\/browser(?:\/([a-zA-Z0-9-]+)(?:\/json\/(protocol|list))?)?$/.exec(
        url.pathname,
      );
    if (!route) return new Response("Not found", { status: 404 });
    for (const [id, s] of sessions)
      if (!s.connected && s.expires < Date.now()) sessions.delete(id);
    let sid = route[1],
      entry = sid ? sessions.get(sid) : undefined;
    if (request.method === "POST" && !sid) {
      if (sessions.size >= 16)
        return new Response("Session limit reached", { status: 429 });
      sid = crypto.randomUUID();
      sessions.set(sid, {
        expires: Date.now() + 120000,
        connected: false,
        used: false,
      });
      return Response.json({ sessionId: sid, targets: [] });
    }
    if (sid && !entry)
      return new Response("Unknown or expired browser session", {
        status: 404,
      });
    if (route[2] === "protocol") return Response.json(protocol);
    // HTTP BrowserTargetInfo uses id; CDP TargetInfo still uses targetId.
    if (route[2] === "list")
      return Response.json(
        entry.browser
          ? [...entry.browser.targets.values()].map((t) => ({
              ...entry.browser.targetInfo(t),
              id: t.id,
            }))
          : [],
      );
    if (request.method === "DELETE" && sid) {
      if (entry.connected)
        return new Response("Close the active WebSocket first", {
          status: 409,
        });
      sessions.delete(sid);
      return new Response(null, { status: 204 });
    }
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket")
      return new Response("WebSocket upgrade required", { status: 426 });
    if (entry?.used)
      return new Response(
        "Obscura sessions cannot reconnect; start a fresh execution",
        { status: 409 },
      );
    if (!entry) {
      if (sessions.size >= 16)
        return new Response("Session limit reached", { status: 429 });
      sid = crypto.randomUUID();
      entry = { expires: Date.now() + 120000, connected: false, used: false };
      sessions.set(sid, entry);
    }
    entry.connected = true;
    entry.used = true;
    const quickJs = await initialize();
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.accept();
    let closed = false;
    let queue = Promise.resolve();
    const network = async (target, init = {}) => {
      if (!env.NETWORK) throw new Error("Browser network binding is required");
      const { kind, ...options } = init;
      const response = await env.NETWORK.fetch(new Request(target, options));
      const declared = Number(response.headers.get("content-length") || 0);
      if (declared > MAX_RESPONSE_BYTES) {
        await response.body?.cancel();
        throw new Error("Browser response exceeds 16 MiB");
      }
      const reader = response.body?.getReader();
      const chunks = [];
      let size = 0;
      if (reader) {
        try {
          while (true) {
            const r = await reader.read();
            if (r.done) break;
            size += r.value.byteLength;
            if (size > MAX_RESPONSE_BYTES)
              throw new Error("Browser response exceeds 16 MiB");
            chunks.push(r.value);
          }
        } finally {
          await reader.cancel().catch(() => {});
        }
      }
      const bytes = new Uint8Array(size);
      let offset = 0;
      for (const c of chunks) {
        bytes.set(c, offset);
        offset += c.byteLength;
      }
      const bounded = new Response(
        [204, 205, 304].includes(response.status) ? null : bytes,
        {
          status: response.status,
          headers: response.headers,
        },
      );
      Object.defineProperty(bounded, "url", { value: response.url || target });
      return bounded;
    };
    const state = new BrowserSession({
      transport: { publicOnly: true, fetch: network },
      allowCrossOriginScripts: true,
    });
    const browser = new ObscuraBrowser({
      fetch: (url, init) => state.fetch(url, init, { kind: "navigation" }),
      onEvent: (e) => {
        if (!closed) server.send(JSON.stringify(e));
      },
      onCloseTarget: (tabId) => state.closeTab(tabId),
      createHost: (tabId) =>
        createObscuraHost({
          quickJs,
          WasmDom,
          bootstrap,
          fetch: network,
          session: state,
          tabId,
          installPageGlobals: ({ vm, run, url, tabId }) => {
            const compat = installCompatibility(
              { vm, run },
              { source: compatibilitySource },
            );
            if (/^https?:/.test(url))
              attachPageSession({ vm, run, url, tabId, session: state });
            return () => compat.dispose();
          },
        }),
    });
    entry.browser = browser;
    let idleTimer;
    const close = () => {
      if (closed) return;
      closed = true;
      clearTimeout(idleTimer);
      entry.connected = false;
      entry.browser = null;
      entry.expires = Date.now() + 30000;
      queue.finally(() => browser.close());
    };
    const resetIdle = () => {
      clearTimeout(idleTimer);
      idleTimer = setTimeout(() => {
        try {
          server.close(1000, "Session expired");
        } finally {
          close();
        }
      }, 120000);
    };
    resetIdle();
    server.addEventListener("close", close);
    server.addEventListener("error", close);
    server.addEventListener("message", (event) => {
      resetIdle();
      queue = queue.then(async () => {
        if (closed) return;
        let message;
        try {
          if (typeof event.data !== "string" || event.data.length > 1024 * 1024)
            throw new Error("Invalid CDP message");
          message = JSON.parse(event.data);
          if (!Number.isSafeInteger(message.id))
            throw new Error("Invalid CDP request id");
          const result = await browser.send(
            message.method,
            message.params || {},
            message.sessionId,
          );
          if (!closed)
            server.send(
              JSON.stringify({
                id: message.id,
                result,
                ...(message.sessionId ? { sessionId: message.sessionId } : {}),
              }),
            );
        } catch (error) {
          if (!closed)
            server.send(
              JSON.stringify({
                id: message?.id ?? null,
                error: {
                  code: error.code ?? -32000,
                  message: String(error.message || error),
                },
                ...(message?.sessionId ? { sessionId: message.sessionId } : {}),
              }),
            );
        }
      });
    });
    return new Response(null, {
      status: 101,
      webSocket: client,
      headers: { "cf-browser-session-id": sid },
    });
  },
};
