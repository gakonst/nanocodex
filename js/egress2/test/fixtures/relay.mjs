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
    return Response.json(path.endsWith("/alpha/search")
      ? { output: this.region } : { relay: this.region });
  }
}
export class RelayLegacy extends FixtureRelay { region = "legacy"; }
export class RelayWnam extends FixtureRelay { region = "wnam"; }
