import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { AccountHostedTools } from "../src/account-hosted-tools";
import { screenAction } from "../src/hand-remote-agent";

const owner = "11111111-1111-4111-8111-111111111193";
const other = "22222222-2222-4222-8222-222222222293";
const surface = { id: "desktop", name: "Desktop", kind: "vm", width: 1600, height: 900, controllable: true, agent_tools: true };
const namespace = () => (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
function next(socket: WebSocket): Promise<any> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { socket.removeEventListener("message", receive); reject(new Error("No host message")); }, 2000);
    function receive(event: MessageEvent) { clearTimeout(timer); socket.removeEventListener("message", receive); resolve(JSON.parse(String(event.data))); }
    socket.addEventListener("message", receive);
  });
}
async function host(machine: string) {
  const stub = namespace().getByName(owner);
  const response = await stub.fetch("https://account-tools.internal/hands/host", { headers: { "x-nanocodex-owner-id": owner, upgrade: "websocket" } });
  const socket = response.webSocket!, ready = next(socket); socket.accept(); const state = await ready;
  const published = next(socket); socket.send(JSON.stringify({ type: "catalog", machine_id: machine, machine_name: machine, surfaces: [surface] })); await published;
  const snapshot = await stub.fetch("https://account-tools.internal/snapshot", { method: "POST", body: JSON.stringify({ owner_id: owner }) });
  const catalog: any = await snapshot.json();
  const tool = catalog.tools.find((tool: any) => tool.route_token.includes(machine));
  return { stub, socket, state, tool };
}
describe("agent screen protocol", () => {
  it("rejects mixed, unbounded, and malformed input before sending anything", () => {
    for (const value of [ { action: "click", x: 0.2, y: 0.4, text: "mixed" }, { action: "drag", x: 0, y: 0, endX: 1, endY: 1, durationMs: 5000 },
      { action: "type", text: "🦄".repeat(1025) }, { action: "key", key: 40, modifiers: [224, 224] }, { action: "scroll", x: NaN, y: 0, deltaX: 0, deltaY: 2 } ]) {
      expect(() => screenAction(value)).toThrow();
    }
    expect(screenAction({ action: "click", x: 0.2, y: 0.4 })).toEqual({ action: "click", x: 0.2, y: 0.4 });
  });
  it("advertises an immutable account tool, returns images, and fences the result to its host", async () => {
    const first = await host("agent-primary"), second = await host("agent-other");
    const invoke = (entry: any, ownerID = owner) => first.stub.fetch("https://account-tools.internal/invoke", {
      method: "POST", body: JSON.stringify({ owner_id: ownerID, name: entry.definition.name, route_token: entry.route_token,
        session_id: "11111111-1111-4111-8111-111111111199", call_id: "screen-observe", input: { action: "observe" } }),
    });
    expect((await invoke(first.tool, other)).status).toBe(404);
    const requested = next(first.socket), pending = invoke(first.tool);
    const request = await requested;
    expect(request).toMatchObject({ type: "agent_call", surface_id: "desktop", generation: first.state.generation, input: { action: "observe" } });
    // Another published device cannot settle this device's admitted call.
    second.socket.send(JSON.stringify({ type: "agent_result", request_id: request.request_id, status: "busy" }));
    first.socket.send(JSON.stringify({ type: "agent_result", request_id: request.request_id, status: "ok", jpeg: "/9j/2Q==", width: 1, height: 1 }));
    const result: any = await (await pending).json();
    expect(result.success).toBe(true);
    expect(result.output[1]).toMatchObject({ type: "input_image", image_url: "data:image/jpeg;base64,/9j/2Q==" });
    expect(result.value).toMatchObject({ status: "ok", image_url: "data:image/jpeg;base64,/9j/2Q==", detail: "original" });
    expect(result.structured_result).toEqual(result.value);
    const replacement = await host("agent-primary");
    expect((await invoke(first.tool)).status).toBe(409);
    replacement.socket.close(); second.socket.close();
  });
  it("reports unknown outcomes when the host disconnects without replaying input", async () => {
    const connected = await host("agent-disconnect");
    const requested = next(connected.socket);
    const response = connected.stub.fetch("https://account-tools.internal/invoke", { method: "POST", body: JSON.stringify({
      owner_id: owner, name: connected.tool.definition.name, route_token: connected.tool.route_token,
      session_id: "11111111-1111-4111-8111-111111111199", call_id: "screen-click", input: { action: "click", x: 0.5, y: 0.5 },
    }) });
    await requested; connected.socket.close();
    expect(await (await response).json()).toMatchObject({ success: false, structured_result: { status: "unavailable" } });
  });
});
