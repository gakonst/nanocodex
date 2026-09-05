import { Agent as ManagedAgent } from "../../managed/index.mjs";
import { registerManagedAgentAlias } from "../../managed/internal.mjs";
import { createTools } from "../../tools/Tools.mjs";
import { AttachmentRejectedError } from "../../tools/attachment.mjs";
import {
  hostedAppToolCatalog,
  hostedToolCatalogDigest,
} from "../../tools/hostedCatalog.mjs";

const PROVIDER_NAME = "ChatGPT · Nanocodex Connect";
const MCP_CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const ATTACHMENT_BACKOFF_MS = [250, 500, 1_000, 2_000, 5_000];

/** Opens the durable Nanocodex agent provisioned by a signed Connect approval. */
export async function create(client, options) {
  const connection = options?.connection;
  if (!connection || typeof connection !== "object") {
    throw new TypeError("agent.create requires an active connection");
  }
  if (connection.grant?.status !== "active") {
    throw new Error("The Connect authorization is not active.");
  }
  if (!connection.grant.connectors?.includes("chatgpt")) {
    throw new Error("Connect ChatGPT before opening the durable Nanocodex agent.");
  }
  const unsupported = Object.keys(options ?? {}).find((key) => (
    key !== "connection" && key !== "signal" && key !== "tools"
  ));
  if (unsupported) {
    throw new TypeError(`Connect durable agents do not accept app-local ${unsupported}`);
  }
  options.signal?.throwIfAborted();

  const appCatalog = hostedAppToolCatalog(options.tools ?? []);
  const appCatalogDigest = appCatalog.length === 0
    ? undefined
    : await hostedToolCatalogDigest(appCatalog);
  if (appCatalogDigest !== connection.grant.appToolCatalogDigest) {
    throw new Error("The app-local tools do not match the exact signed Connect grant.");
  }

  const grantSession = client._captureSession?.();
  if (!grantSession) throw new Error("The Connect authorization session is unavailable.");
  const transport = {
    baseUrl: client.transport.baseUrl,
    grantSession,
  };
  let tools;
  if (options.tools !== undefined || connection.grant.mcpConnections.length > 0) {
    tools = await createAttachedTools(
      transport,
      connection.grant.id,
      options.tools,
      connection.grant.mcpConnections,
    );
  }
  const managedOptions = {
    baseUrl: client.transport.baseUrl,
    fetch: managedGrantFetch(
      grantSession,
      client.transport.baseUrl,
      connection.grant.id,
      connection.agentId,
    ),
    ...(tools === undefined ? {} : {
      toolsTransport: connectToolsTransport(
        transport,
        connection.grant.id,
        connection.agentId,
      ),
    }),
  };
  try {
    const managed = ManagedAgent.open(connection.agentId, managedOptions);
    return await connectAgent(managed, connection, transport, tools, options.signal);
  } catch (error) {
    await tools?.close();
    throw error;
  }
}

async function connectAgent(managed, connection, transport, tools, signal) {
  const visibility = connection.grant.visibility;
  const toolState = tools === undefined ? undefined : startToolAttachment(managed, tools, signal);
  try {
    await toolState?.supervisor;
    signal?.throwIfAborted();
  } finally {
    detachInitialToolAbort(toolState);
  }
  const agent = {
    id: managed.id,
    sessionId: managed.id,
    type: "connect",
    provider: PROVIDER_NAME,
    state: () => managed.state(),
    events: Object.freeze({
      async page(options) {
        const page = await managed.events.page(options);
        if (!visibility.conversationHistory && !visibility.rawTraces) {
          return Object.freeze({
            data: Object.freeze([]),
            hasMore: false,
            latestCursor: page.latestCursor,
          });
        }
        return Object.freeze({
          ...page,
          data: Object.freeze(page.data
            .map((event) => projectManagedEvent(event, visibility))
            .filter(Boolean)),
        });
      },
      watch(options) {
        return projectManagedEvents(managed.events.watch(options), visibility);
      },
    }),
    mercator: Object.freeze({
      enabled: true,
      channelId: undefined,
      cumulative: 0n,
      opened: false,
    }),
    turn: Object.freeze({
      prompt(parameters) {
        const turn = managed.turn.prompt(parameters);
        return Object.freeze({
          idempotencyKey: turn.idempotencyKey,
          accepted: () => turn.accepted(),
          state: () => turn.state(),
          steer: (options) => turn.steer(options),
          cancel: () => turn.cancel(),
          async result(options) {
            const result = await turn.result(options);
            return Object.freeze({
              ...result,
              finalMessage: visibility.finalMessages ? result.finalMessage : "",
              provider: PROVIDER_NAME,
              capabilitiesUsed: Object.freeze([]),
            });
          },
        });
      },
    }),
    session: Object.freeze({
      shutdown: () => shutdownToolAttachment(toolState),
    }),
  };
  registerManagedAgentAlias(agent, managed, {
    voiceTransport: connectVoiceTransport(transport, connection.grant.id, connection.agentId),
  });
  return Object.freeze(agent);
}

async function createAttachedTools({ baseUrl, grantSession }, grantId, tools, connections) {
  const mcp = Object.fromEntries(connections.map(({ id, name }) => {
    if (!MCP_CONNECTION_ID.test(id)) {
      throw new TypeError("Connect grant contains an invalid MCP connection ID");
    }
    return [id, {
      description: name,
      fetch: grantMcpFetch(grantSession, baseUrl, grantId, id),
      url: new URL(`/v1/grants/${grantId}/mcp/${id}`, baseUrl),
    }];
  }));
  return createTools({
    ...(tools === undefined ? {} : { tools }),
    ...(connections.length === 0 ? {} : {
      mcp,
      mcpOptions: {
        catalogProvider: (connectionId) => `mcp:${connectionId}`,
      },
    }),
  });
}

function grantMcpFetch(session, baseUrl, grantId, connectionId) {
  const endpoint = new URL(`/v1/grants/${grantId}/mcp/${connectionId}`, baseUrl);
  return (input, init) => {
    const url = new URL(input instanceof Request ? input.url : input, endpoint);
    if (url.origin !== endpoint.origin || url.pathname !== endpoint.pathname) {
      throw new TypeError("Connect MCP fetch is restricted to its authorized grant connection");
    }
    if (input instanceof Request) {
      const request = init === undefined ? input : new Request(input, init);
      return session.fetch(new Request(url, request));
    }
    return session.fetch(url, init);
  };
}

function connectToolsTransport({ baseUrl, grantSession }, grantId, agentId) {
  const path = `/v1/grants/${grantId}/agents/${encodeURIComponent(agentId)}/tool-host`;
  return async () => {
    const response = await grantSession.fetch(new Request(new URL(`${path}/ticket`, baseUrl), {
      method: "POST",
    }));
    const receipt = await response.json().catch(() => undefined);
    if (!response.ok || typeof receipt?.ticket !== "string" || !receipt.ticket) {
      const message = receipt?.error?.message ?? "managed tool-host authorization failed";
      if (terminalToolHostStatus(response.status)) throw new AttachmentRejectedError(message);
      throw new Error(message);
    }
    const url = new URL(path, baseUrl);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    url.searchParams.set("ticket", receipt.ticket);
    const WebSocketImpl = globalThis.WebSocket;
    if (typeof WebSocketImpl !== "function") {
      throw new Error("WebSocket is unavailable in this runtime.");
    }
    return new WebSocketImpl(url);
  };
}

function terminalToolHostStatus(status) {
  return status >= 400 && status < 500 && status !== 408 && status !== 425 && status !== 429;
}

function startToolAttachment(managed, tools, signal) {
  signal?.throwIfAborted();
  const state = {
    abort: new AbortController(),
    closing: undefined,
    connector: undefined,
    initialSignal: signal,
    onInitialAbort: undefined,
    tools,
  };
  if (signal) {
    state.onInitialAbort = () => {
      state.abort.abort(signal.reason);
      state.connector?.close();
    };
    signal.addEventListener("abort", state.onInitialAbort, { once: true });
    if (signal.aborted) state.onInitialAbort();
  }
  state.supervisor = superviseToolAttachment(state, managed.toolsTarget());
  return state;
}

function detachInitialToolAbort(state) {
  if (!state?.onInitialAbort) return;
  state.initialSignal.removeEventListener("abort", state.onInitialAbort);
  state.onInitialAbort = undefined;
  state.initialSignal = undefined;
}

async function superviseToolAttachment(state, target) {
  for (let attempt = 0; ; attempt += 1) {
    if (state.abort.signal.aborted) {
      throwInitialToolAbort(state);
      return;
    }
    const connector = state.tools.attach(target);
    state.connector = connector;
    try {
      await connector.connect();
      return;
    } catch (error) {
      connector.close();
      if (state.connector === connector) state.connector = undefined;
      if (error instanceof AttachmentRejectedError) throw error;
      if (state.abort.signal.aborted) {
        throwInitialToolAbort(state);
        return;
      }
      await attachmentBackoff(
        ATTACHMENT_BACKOFF_MS[Math.min(attempt, ATTACHMENT_BACKOFF_MS.length - 1)],
        state.abort.signal,
      );
    }
  }
}

function throwInitialToolAbort(state) {
  if (state.initialSignal?.aborted) throw state.initialSignal.reason;
}

function attachmentBackoff(milliseconds, signal) {
  if (signal.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(done, milliseconds);
    signal.addEventListener("abort", done, { once: true });
    function done() {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    }
  });
}

function shutdownToolAttachment(state) {
  if (!state) return Promise.resolve();
  if (state.closing) return state.closing;
  state.abort.abort();
  state.connector?.close();
  state.closing = (async () => {
    await state.supervisor;
    await state.tools.close();
  })();
  return state.closing;
}

function connectVoiceTransport({ baseUrl, grantSession }, grantId, agentId) {
  const grantPath = `/v1/grants/${grantId}/agents/${encodeURIComponent(agentId)}/realtime`;
  let voiceSessionId;
  return Object.freeze({
    call(body, signal) {
      const call = managedRealtimeCallBody(body, agentId);
      voiceSessionId = call.voiceSessionId;
      return grantSession.fetch(new Request(new URL(`${grantPath}/calls`, baseUrl), {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-nanocodex-voice-session-id": voiceSessionId,
        },
        body: call.body,
        signal,
      }));
    },
    async sidebandUrl(callId) {
      if (!voiceSessionId) throw new Error("Connect voice call must open before its sideband");
      const response = await grantSession.fetch(new Request(new URL(`${grantPath}/ticket`, baseUrl), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ call_id: callId, voice_session_id: voiceSessionId }),
      }));
      const receipt = await response.json().catch(() => undefined);
      if (!response.ok || typeof receipt?.ticket !== "string") {
        throw new Error(receipt?.error?.message ?? "voice sideband authorization failed");
      }
      const url = new URL(`${grantPath}/sideband`, baseUrl);
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      url.searchParams.set("call_id", callId);
      url.searchParams.set("ticket", receipt.ticket);
      url.searchParams.set("voice_session_id", voiceSessionId);
      return url;
    },
  });
}

function managedRealtimeCallBody(encoded, agentId) {
  let envelope;
  try { envelope = JSON.parse(encoded); }
  catch { throw new TypeError("Connect voice call body is invalid"); }
  if (!envelope || typeof envelope !== "object" || Array.isArray(envelope)
    || envelope.managed_agent_id !== agentId
    || typeof envelope.call_body !== "string"
    || typeof envelope.realtime_session_id !== "string") {
    throw new TypeError("Connect voice call body is invalid");
  }
  return { body: envelope.call_body, voiceSessionId: envelope.realtime_session_id };
}

function managedGrantFetch(session, baseUrl, grantId, agentId) {
  const origin = new URL(baseUrl).origin;
  const prefix = `/v1/agents/${encodeURIComponent(agentId)}`;
  return async (input, init) => {
    const url = new URL(input instanceof Request ? input.url : input);
    if (url.origin !== origin || (url.pathname !== prefix && !url.pathname.startsWith(`${prefix}/`))) {
      throw new TypeError("Connect managed fetch is restricted to its authorized durable agent");
    }
    const source = input instanceof Request
      ? (init === undefined ? input : new Request(input, init))
      : undefined;
    const method = source?.method ?? init?.method ?? "GET";
    const read = method === "GET" && isManagedReadPath(url.pathname.slice(prefix.length));
    url.pathname = `/v1/grants/${grantId}/agents/${encodeURIComponent(agentId)}${url.pathname.slice(prefix.length)}`;
    if (read) {
      const request = new Request(url, source ?? init);
      return session.fetch(new Request(request, { method: "POST" }));
    }
    if (source) return session.fetch(new Request(url, source));
    return session.fetch(url, init);
  };
}

/** @internal Exact managed reads tunneled as POST so browsers send Origin. */
export function isManagedReadPath(pathname) {
  return pathname === ""
    || pathname === "/events"
    || pathname === "/events/history"
    || /^\/turns\/[^/]+$/.test(pathname);
}

async function* projectManagedEvents(events, visibility) {
  try {
    for await (const event of events) {
      const projected = projectManagedEvent(event, visibility);
      if (projected) yield projected;
    }
  } finally {
    await events.return?.();
  }
}

function projectManagedEvent(event, visibility) {
  if (visibility.rawTraces) return event;
  const data = event?.data;
  if (!data || typeof data !== "object") return undefined;
  if (data.type === "event") {
    const eventType = data.event?.type;
    if ((eventType === "assistant.delta" || eventType === "assistant.message")
      && visibility.finalMessages) {
      const payload = data.event?.payload;
      return payload?.phase === "commentary" ? undefined : event;
    }
    return visibility.actionSummaries
      && (eventType === "tool.call" || eventType === "tool.result")
      ? event
      : undefined;
  }
  if (data.type === "turn_completed" && !visibility.finalMessages) {
    return Object.freeze({
      ...event,
      data: Object.freeze({ ...data, final_message: "" }),
    });
  }
  return event;
}

/** @internal Projects app-visible result fields from the signed SIWE resources. */
export function projectAgentObservations(visibility, finalMessage, capabilitiesUsed) {
  return Object.freeze({
    finalMessage: visibility.finalMessages ? finalMessage : "",
    capabilitiesUsed: Object.freeze(visibility.actionSummaries ? [...capabilitiesUsed] : []),
  });
}
