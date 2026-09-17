import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { Agent } from "nanocodex/cloudflare";

for (const failHttps of [false, true]) {
  it(failHttps
    ? "settles exhausted compaction and replays its failure after SQLite reconstruction"
    : "falls back from compaction WebSocket closures to streaming HTTPS in the Worker", async () => {
    const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
    await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
      let wsCompactions = 0;
      let wsGenerations = 0;
      let httpsCompactions = 0;
      let httpsGenerations = 0;
      const compactionStarted = (body: { input?: { type: string }[] }) =>
        body.input?.some((item) => item.type === "compaction_trigger") ?? false;
      const completed = (id: string, highUsage = false) => ({
        type: "response.completed", response: {
          id, status: "completed", end_turn: true,
          output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "remembered" }] }],
          usage: { input_tokens: highUsage ? 250_000 : 100, output_tokens: 1, total_tokens: highUsage ? 250_001 : 101 },
        },
      });
      class ModelSocket extends EventTarget {
        readyState = 1;
        accept() {}
        close() { this.readyState = 3; }
        send(data: string) {
          const body = JSON.parse(data);
          queueMicrotask(() => {
            if (compactionStarted(body)) {
              wsCompactions++;
              this.readyState = 3;
              this.dispatchEvent(new CloseEvent("close", { code: 1006 }));
            } else {
              wsGenerations++;
              this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(completed("before-compaction", true)) }));
            }
          });
        }
      }
      const owner = { ctx, env: { NANOCODEX: { async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init);
        expect(request.url).toBe("https://nanocodex.internal/v1/responses");
        expect(request.headers.get("x-nanocodex-subject")).toBe(ctx.id.toString());
        expect(request.headers.get("authorization")).toBe("Bearer NANOCODEX_PROVIDER_CREDENTIAL");
        if (request.method === "GET") {
          return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
        }
        expect(request.method).toBe("POST");
        const body = await request.json<{ input: { type: string }[]; stream: boolean; previous_response_id?: string; type?: string; prompt_cache_key?: string; text?: { format?: { schema?: unknown } } }>();
        expect(body.stream).toBe(true);
        expect(body.prompt_cache_key).toBe("compaction-test-cache");
        expect(body.text?.format?.schema).toEqual({ type: "object", properties: {} });
        expect(body.type).toBeUndefined();
        expect(body.previous_response_id).toBeUndefined();
        expect(JSON.stringify(body)).toContain("retain build req_7f3");
        const compact = compactionStarted(body);
        if (compact) {
          httpsCompactions++;
          if (failHttps) return new Response("provider unavailable", { status: 503 });
        } else {
          httpsGenerations++;
          expect(JSON.stringify(body)).toContain("opaque-https-summary");
        }
        const events = compact ? [
          { type: "response.output_item.done", item: { id: "cmp-https", type: "compaction", encrypted_content: "opaque-https-summary" } },
          { type: "response.completed", response: { id: "compacted", status: "completed", output: [], usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 } } },
        ] : [completed(`https-${httpsGenerations}`)];
        // Exercise streaming across arbitrary SSE frame/chunk boundaries.
        const bytes = new TextEncoder().encode(events.map(event => `data: ${JSON.stringify(event)}\n\n`).join("") + "data: [DONE]\n\n");
        let offset = 0;
        return new Response(new ReadableStream({
          pull(controller) {
            if (offset === bytes.length) { controller.close(); return; }
            controller.enqueue(bytes.slice(offset, offset + 17));
            offset = Math.min(offset + 17, bytes.length);
          },
        }), { headers: { "content-type": "text/event-stream" } });
      } } } };
      const options = { eventPersistence: "caller" as const };
      Object.defineProperty(options, Symbol.for("nanocodex.cloudflare.internalRuntime"), {
        value: { responseControls: { promptCacheKey: "compaction-test-cache", outputSchema: { type: "object", properties: {} } } },
      });
      let agent = await Agent.create(owner, options);
      try {
        await agent.turn.prompt({ id: "establish", input: "retain build req_7f3" }).result();
        const second = () => agent.turn.prompt({ id: "needs-compaction", input: "continue after compaction" }).result();
        if (failHttps) {
          await expect(second()).rejects.toThrow(/compaction.*503/i);
          expect(wsCompactions).toBe(3);
          expect(httpsCompactions).toBe(3);
          expect(JSON.stringify(await agent.session.context())).toContain("retain build req_7f3");
          await agent.session.shutdown();
          agent = await Agent.create(owner, options);
          await expect(second()).rejects.toThrow(/compaction.*503/i);
          expect(wsCompactions).toBe(3);
          expect(httpsCompactions).toBe(3);
          expect(wsGenerations).toBe(1);
          expect(httpsGenerations).toBe(0);
        } else {
          expect((await second()).finalMessage).toBe("remembered");
          expect(wsCompactions).toBe(3);
          expect(httpsCompactions).toBe(1);
          await agent.turn.prompt({ id: "stay-on-https", input: "continue again" }).result();
          expect(wsGenerations).toBe(1);
          expect(httpsGenerations).toBe(2);
          const compacted = await agent.session.context();
          expect(JSON.stringify(compacted)).toContain("opaque-https-summary");
          await agent.session.shutdown();
          agent = await Agent.create(owner, options);
          expect(await agent.session.context()).toEqual(compacted);
        }
      } finally { await agent.session.shutdown(); }
    });
  }, 30_000);
}
