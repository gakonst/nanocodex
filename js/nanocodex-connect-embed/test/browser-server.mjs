import { createServer } from "node:http";

export const agentIds = {
  alpha: "0198d3f0-8844-7000-8000-000000000001",
  beta: "0198d3f0-8844-7000-8000-000000000002",
};

/** The only synthetic boundary is the remote agent. Requests use the real SDK's
 * HTTP and SSE implementation; neither the controller nor its source is mocked. */
export async function startBrowserFixture(assets) {
  const requests = [];
  const agents = new Map(Object.entries(agentIds).map(([name, id]) => [id, {
    name, events: [], streams: new Set(), turns: new Map(), nextCursor: 0,
  }]));
  const emit = (agent, turnId, data) => {
    const event = { cursor: String(++agent.nextCursor), created_at: Date.now(), turn_id: turnId, ...data };
    agent.events.push(event);
    for (const stream of agent.streams) stream.write(frame(event));
    return event;
  };
  const raw = (agent, turnId, type, payload) => emit(agent, turnId, {
    type: "event", event: { protocol_version: 1, request_id: turnId, seq: agent.nextCursor + 1, type, payload },
  });
  for (const agent of agents.values()) {
    for (const age of ["older", "recent"]) {
      const id = `${agent.name}-${age}`;
      emit(agent, id, { type: "turn_accepted", id, input: `${agent.name} ${age} question`, replayed: false });
      emit(agent, id, { type: "turn_completed", id, final_message: `${agent.name} ${age} answer`, usage: null });
    }
  }
  function finish(agent, turn, data) {
    turn.terminal = emit(agent, turn.id, { id: turn.id, ...data });
  }
  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url, "http://localhost");
      if (assets.has(url.pathname)) {
        const asset = assets.get(url.pathname);
        response.writeHead(200, { "content-type": asset.type });
        response.end(asset.body);
        return;
      }
      let body = "";
      for await (const chunk of request) body += chunk;
      const input = body ? JSON.parse(body) : undefined;
      const recorded = { method: request.method, path: url.pathname, query: url.search, body: input };
      requests.push(recorded);
      response.on("finish", () => { recorded.status = response.statusCode; });
      const [, agentId, suffix = ""] = url.pathname.match(/^\/v1\/agents\/([^/]+)(.*)$/) ?? [];
      const agent = agents.get(agentId);
      if (!agent) return json(response, 404, { error: "unknown synthetic agent" });
      if (suffix === "/events/history") {
        const before = url.searchParams.get("before");
        const eligible = agent.events.filter(event => !before || Number(event.cursor) < Number(before));
        // Deliberately small pages exercise the public Load older control.
        const data = eligible.slice(-2);
        return json(response, 200, { data, has_more: eligible.length > data.length, latest_cursor: String(agent.nextCursor) });
      }
      if (suffix === "/events") {
        response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
        response.write(": connected\n\n");
        const cursor = url.searchParams.get("cursor");
        if (cursor !== "latest") {
          for (const event of agent.events) if (Number(event.cursor) > Number(cursor ?? 0)) response.write(frame(event));
        }
        agent.streams.add(response);
        response.on("close", () => agent.streams.delete(response));
        return;
      }
      if (suffix === "/turns" && request.method === "POST") {
        const id = input.id;
        if (input.input === "reject submission") {
          return json(response, 403, { error: { code: "forbidden", message: "Synthetic permission denied" } });
        }
        const turn = { id, input: input.input };
        agent.turns.set(id, turn);
        agent.active = turn;
        const accepted = emit(agent, id, { type: "turn_accepted", id, input: input.input, replayed: false });
        json(response, 202, { turn_id: id, accepted_cursor: accepted.cursor, state: "accepted" });
        raw(agent, id, "run.started", {});
        raw(agent, id, "assistant.delta", { text: "Checking the local fixture", item_id: id + "-comment", phase: "commentary" });
        return;
      }
      const match = suffix.match(/^\/turns\/([^/]+)(?:\/(cancel|steer))?$/);
      if (match) {
        const turn = agent.turns.get(match[1]);
        if (!turn) return json(response, 404, { error: "unknown turn" });
        if (match[2] === "cancel") {
          finish(agent, turn, { type: "turn_cancelled" });
          return json(response, 200, { turn_id: turn.id, state: "cancelled" });
        }
        if (match[2] === "steer") {
          if (input.input === "unavailable steering") return json(response, 503, { error: "service_unavailable", message: "Synthetic steering service unavailable" });
          raw(agent, turn.id, "run.steered", { input: input.input });
          return json(response, 200, { turn_id: turn.id, state: "steering" });
        }
        return json(response, 200, {
          turn_id: turn.id, state: turn.terminal ? "completed" : "running",
          ...(turn.terminal ? { terminal: turn.terminal, terminal_cursor: turn.terminal.cursor } : {}),
        });
      }
      return json(response, 404, { error: "unsupported fixture route" });
    } catch (error) {
      json(response, 500, { error: String(error) });
    }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  return {
    origin: `http://127.0.0.1:${server.address().port}`,
    requests,
    advance(name, phase) {
      const agent = agents.get(agentIds[name]);
      const turn = agent.active;
      if (!turn) throw new Error(`No active turn for ${name}`);
      if (phase === "tool") {
        raw(agent, turn.id, "tool.call", { call_id: `${turn.id}-tool`, tool: "fixture_lookup", arguments: { query: turn.input } });
      } else if (phase === "final") {
        raw(agent, turn.id, "tool.result", { call_id: `${turn.id}-tool`, tool: "fixture_lookup", status: "completed", result: "Two synthetic records found" });
        raw(agent, turn.id, "assistant.message", { text: `Completed: ${turn.input}`, item_id: turn.id + "-final", phase: "final_answer" });
        finish(agent, turn, { type: "turn_completed", final_message: `Completed: ${turn.input}`, usage: null });
      } else if (phase === "error") {
        finish(agent, turn, { type: "turn_failed", error: "Synthetic agent failure" });
      } else throw new Error(`Unknown fixture phase ${phase}`);
    },
    async close() {
      for (const agent of agents.values()) for (const stream of agent.streams) stream.end();
      server.closeAllConnections();
      await new Promise(resolve => server.close(resolve));
    },
  };
}
function frame(event) { return `id: ${event.cursor}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`; }
function json(response, status, body) {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(body));
}
