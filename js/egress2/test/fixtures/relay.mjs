import { DurableObject } from "cloudflare:workers";
class FixtureRelay extends DurableObject {
  async fetch(request) {
    if (request.headers.has("x-managed2-relay-region") || request.headers.has("x-managed2-owner")
      || request.headers.has("x-managed2-trace-id")) {
      return Response.json({ error: "private routing header leaked" }, { status: 400 });
    }
    if (request.headers.get("upgrade")?.toLowerCase() === "websocket") {
      const pair = new WebSocketPair();
      const [client, server] = Object.values(pair);
      server.accept();
      server.send(JSON.stringify({ relay: this.region }));
      return new Response(null, { status: 101, webSocket: client });
    }
    const path = new URL(request.url).pathname;
    if (path.endsWith("/alpha/search")) {
      const body = await request.json();
      const expected = body.commands.expected_output_budget;
      if (expected === undefined ? Object.hasOwn(body, "max_output_tokens") : body.max_output_tokens !== expected) {
        return Response.json({ error: "unexpected output budget" }, { status: 400 });
      }
      return Response.json({ output: this.region });
    }
    return Response.json({ relay: this.region });
  }
}
export class Relay extends FixtureRelay { region = "relay"; }
