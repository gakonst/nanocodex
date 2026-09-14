import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { AccountHostedTools } from "../src/account-hosted-tools";
import worker from "../src/index";
import type { Principal } from "../src/account-auth";

const A = "11111111-1111-4111-8111-111111111191";
const B = "22222222-2222-4222-8222-222222222292";
const surface = { id: "screen", name: "Screen", kind: "desktop", width: 1920, height: 1080, controllable: true };
const namespace = () => (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
const headers = (owner = A) => ({ "x-nanocodex-owner-id": owner });

function next(socket: WebSocket): Promise<any> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { socket.removeEventListener("message", receive); reject(new Error("No remote signal")); }, 2000);
    function receive(event: MessageEvent) { clearTimeout(timer); socket.removeEventListener("message", receive); resolve(JSON.parse(String(event.data))); }
    socket.addEventListener("message", receive);
  });
}
async function host(machine: string, owner = A, surfaces = [surface]) {
  const stub = namespace().getByName(owner);
  const response = await stub.fetch("https://account-tools.internal/hands/host", { headers: { ...headers(owner), upgrade: "websocket" } });
  expect(response.status).toBe(101);
  const socket = response.webSocket!;
  const ready = next(socket); socket.accept();
  const state = await ready;
  const published = next(socket);
  socket.send(JSON.stringify({ type: "catalog", machine_id: machine, machine_name: machine, surfaces }));
  expect(await published).toEqual({ type: "published", generation: state.generation });
  return { stub, socket, state, owner, machine };
}

async function view(publisher: Awaited<ReturnType<typeof host>>, surfaceId = "screen") {
  const joined = next(publisher.socket);
  const query = new URLSearchParams({ machine_id: publisher.machine, surface_id: surfaceId, generation: publisher.state.generation });
  const response = await publisher.stub.fetch(`https://account-tools.internal/hands/view?${query}`, {
    headers: { ...headers(publisher.owner), upgrade: "websocket" },
  });
  expect(response.status).toBe(101);
  const socket = response.webSocket!, ready = next(socket); socket.accept();
  const state = await ready;
  expect(await joined).toEqual({ type: "viewer", viewer_id: state.connection_id, surface_id: surfaceId, generation: state.generation });
  return { socket, state };
}

function closed(socket: WebSocket): Promise<CloseEvent> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { socket.removeEventListener("close", receive); reject(new Error("Remote socket stayed open")); }, 2000);
    function receive(event: CloseEvent) { clearTimeout(timer); socket.removeEventListener("close", receive); resolve(event); }
    socket.addEventListener("close", receive);
  });
}

async function ping(socket: WebSocket) {
  const response = next(socket); socket.send(JSON.stringify({ type: "ping" }));
  expect(await response).toEqual({ type: "pong" });
}

function messages(socket: WebSocket) {
  const received: unknown[] = [];
  socket.addEventListener("message", event => { received.push(JSON.parse(String(event.data))); });
  return received;
}

const frameSurface = { ...surface, width: 1280, height: 720, transport: "frames-v1" };
const frame = { type: "frame", jpeg: "/9j/AAAA", width: 1280, height: 720 };
const framesHost = (machine = "sandbox", owner = crypto.randomUUID()) => host(machine, owner, [frameSurface]);
async function requestFrame(publisher: Awaited<ReturnType<typeof host>>, viewer: Awaited<ReturnType<typeof view>>) {
  const request = next(publisher.socket); viewer.socket.send(JSON.stringify({ type: "frame_request" }));
  expect(await request).toEqual({ type: "frame_request", viewer_id: viewer.state.connection_id });
}

describe("interactive hand signaling on a real Durable Object", () => {
  it("preserves device inventory while the account publishes remote screens", async () => {
    const owner = crypto.randomUUID();
    const { socket, state } = await host("remote-only", owner);
    const principal: Principal = {
      kind: "api_key", userId: owner, organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
      role: "owner", subjectId: `user:${owner}`, credentialId: "test", authorizationEpoch: 1,
      capabilities: ["agents:read", "agents:write", "tools:use"],
    };
    const call = (path: string, actor = principal) => worker.fetch(
      new Request("https://nanocodex.example/v1/account/hands" + path),
      env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
    );
    try {
      expect(await (await call("")).json()).toEqual({ data: [] });
      expect(await (await call("/screens")).json()).toEqual({ surfaces: [{
        ...surface, machine_id: "remote-only", machine_name: "remote-only", generation: state.generation,
      }] });
      expect(await (await call("/screens", { ...principal, userId: crypto.randomUUID() })).json()).toEqual({ surfaces: [] });
      expect((await call("/screens", { ...principal, capabilities: ["agents:read"] })).status).toBe(403);
    } finally { socket.close(1000, "Done"); }
  });

  it("fences VM publishers by machine, route, role, and allocation lease", async () => {
    const stub = namespace().getByName(A);
    const scope = { machineId: "vm:allocated", routeId: "vm-host:allocation:1", expiresAt: Date.now() + 20_000 };
    const scopedHeaders = (value = scope) => ({ ...headers(), "x-nanocodex-capabilities": '["agents:write"]',
      "x-nanocodex-remote-vm": JSON.stringify(value) });
    const open = async () => {
      const response = await stub.fetch("https://account-tools.internal/hands/host", { headers: { ...scopedHeaders(), upgrade: "websocket" } });
      expect(response.status).toBe(101);
      const socket = response.webSocket!, pending = next(socket); socket.accept();
      return { socket, state: await pending };
    };
    const wrong = await open();
    const closed = new Promise<CloseEvent>(resolve => wrong.socket.addEventListener("close", resolve, { once: true }));
    wrong.socket.send(JSON.stringify({ type: "catalog", machine_id: "another-machine", machine_name: "VM", surfaces: [{ ...surface, kind: "vm" }] }));
    expect((await closed).code).toBe(1008);
    const vm = await open();
    expect(vm.state.expires_at).toBe(scope.expiresAt);
    const published = next(vm.socket);
    vm.socket.send(JSON.stringify({ type: "catalog", machine_id: scope.machineId, machine_name: "VM", surfaces: [{ ...surface, kind: "vm" }] }));
    await published;
    const ordinary = await host("personal-mac");
    const renew = (id: string, auth: Record<string, string>) => stub.fetch("https://account-tools.internal/hands/renew", {
      method: "POST", headers: auth, body: JSON.stringify({ connection_id: id }),
    });
    expect((await renew(ordinary.state.connection_id, scopedHeaders())).status).toBe(403);
    expect((await renew(vm.state.connection_id, scopedHeaders({ ...scope, routeId: "vm-host:allocation:2" }))).status).toBe(403);
    expect((await renew(vm.state.connection_id, { ...headers(), "x-nanocodex-capabilities": '["agents:write"]' })).status).toBe(403);
    const renewed = next(vm.socket);
    expect((await renew(vm.state.connection_id, scopedHeaders())).status).toBe(200); await renewed;
    expect((await stub.fetch("https://account-tools.internal/hands/screens", { headers: scopedHeaders() })).status).toBe(403);
    vm.socket.close(); ordinary.socket.close();
  });

  it("routes SDP and ICE to the exact surface generation without storing media", async () => {
    const { stub, socket, state } = await host("mac");
    const list = await stub.fetch("https://account-tools.internal/hands/screens", { headers: headers() });
    expect(await list.json()).toMatchObject({ surfaces: [{ ...surface, machine_id: "mac", generation: state.generation }] });
    const joined = next(socket);
    const response = await stub.fetch(`https://account-tools.internal/hands/view?machine_id=mac&surface_id=screen&generation=${state.generation}`, { headers: { ...headers(), upgrade: "websocket" } });
    expect(response.status).toBe(101);
    const viewer = response.webSocket!;
    const viewerReady = next(viewer); viewer.accept(); await viewerReady;
    const connection = await joined;
    expect(connection).toMatchObject({ type: "viewer", surface_id: "screen", generation: state.generation });
    const offered = next(viewer);
    socket.send(JSON.stringify({ type: "signal", viewer_id: connection.viewer_id, signal: { type: "offer", sdp: "v=0\r\n" } }));
    expect(await offered).toEqual({ type: "signal", signal: { type: "offer", sdp: "v=0\r\n" } });
    const answered = next(socket);
    viewer.send(JSON.stringify({ type: "signal", signal: { type: "answer", sdp: "v=0\r\n" } }));
    expect(await answered).toEqual({ type: "signal", viewer_id: connection.viewer_id, signal: { type: "answer", sdp: "v=0\r\n" } });
    const renewed = next(viewer);
    const renewal = await stub.fetch("https://account-tools.internal/hands/renew", { method: "POST", headers: headers(), body: JSON.stringify({ connection_id: connection.viewer_id }) });
    expect(renewal.status).toBe(200); expect(await renewed).toMatchObject({ type: "renewed" });
    const left = next(socket); viewer.close();
    expect(await left).toEqual({ type: "viewer_left", viewer_id: connection.viewer_id });
    socket.close();
  });

  it("denies another owner and never switches a stale viewer to a replacement host", async () => {
    const first = await host("replace");
    const forbidden = await first.stub.fetch("https://account-tools.internal/hands/screens", { headers: headers(B) });
    expect(forbidden.status).toBe(404);
    const second = await host("replace");
    expect(second.state.generation).not.toBe(first.state.generation);
    const stale = await first.stub.fetch(`https://account-tools.internal/hands/view?machine_id=replace&surface_id=screen&generation=${first.state.generation}`, { headers: { ...headers(), upgrade: "websocket" } });
    expect(stale.status).toBe(409);
    const other = namespace().getByName(B);
    const crossAccount = await other.fetch(`https://account-tools.internal/hands/view?machine_id=replace&surface_id=screen&generation=${second.state.generation}`, { headers: { ...headers(B), upgrade: "websocket" } });
    expect(crossAccount.status).toBe(409);
    second.socket.close();
  });

  it("rejects viewer attempts to publish SDP offers or choose another recipient", async () => {
    const { stub, socket, state } = await host("isolate");
    const joined = next(socket);
    const response = await stub.fetch(`https://account-tools.internal/hands/view?machine_id=isolate&surface_id=screen&generation=${state.generation}`, { headers: { ...headers(), upgrade: "websocket" } });
    const viewer = response.webSocket!;
    const ready = next(viewer); viewer.accept(); await ready; await joined;
    const closed = new Promise<CloseEvent>(resolve => viewer.addEventListener("close", resolve, { once: true }));
    viewer.send(JSON.stringify({ type: "signal", viewer_id: "someone-else", signal: { type: "offer", sdp: "v=0" } }));
    expect((await closed).code).toBe(1008);
    socket.close();
  });

  it("lets a host close only its own viewer without interrupting another host", async () => {
    const first = await host("close-first"), second = await host("close-second");
    const joined = next(second.socket);
    const response = await second.stub.fetch(`https://account-tools.internal/hands/view?machine_id=close-second&surface_id=screen&generation=${second.state.generation}`, { headers: { ...headers(), upgrade: "websocket" } });
    const viewer = response.webSocket!;
    const ready = next(viewer); viewer.accept(); await ready;
    const connection = await joined;
    first.socket.send(JSON.stringify({ type: "close_viewer", viewer_id: connection.viewer_id }));
    const pong = next(viewer); viewer.send(JSON.stringify({ type: "ping" }));
    expect(await pong).toEqual({ type: "pong" });
    const closed = new Promise<CloseEvent>(resolve => viewer.addEventListener("close", resolve, { once: true }));
    second.socket.send(JSON.stringify({ type: "close_viewer", viewer_id: connection.viewer_id }));
    expect((await closed).code).toBe(1008);
    first.socket.close(); second.socket.close();
  });
});

describe("pull-based hand frames on a real Durable Object", () => {
  it("selects transport per surface and retains authenticated connection leases", async () => {
    const publisher = await host("mixed", crypto.randomUUID(), [frameSurface, { ...surface, id: "webrtc" }]);
    const viewer = await view(publisher), rtc = await view(publisher, "webrtc");
    try {
      const catalog = await publisher.stub.fetch("https://account-tools.internal/hands/screens", { headers: headers(publisher.owner) });
      expect(await catalog.json()).toEqual({ surfaces: [frameSurface, { ...surface, id: "webrtc" }].map(item => ({
        ...item, machine_id: publisher.machine, machine_name: publisher.machine, generation: publisher.state.generation,
      })) });
      expect(viewer.state).toMatchObject({ type: "ready", generation: publisher.state.generation });
      expect(viewer.state.expires_at).toBeGreaterThan(Date.now());
      await requestFrame(publisher, viewer);
      const received = next(viewer.socket);
      publisher.socket.send(JSON.stringify({ ...frame, viewer_id: viewer.state.connection_id }));
      expect(await received).toEqual(frame);
      const answer = next(publisher.socket);
      rtc.socket.send(JSON.stringify({ type: "signal", signal: { type: "answer", sdp: "v=0\r\n" } }));
      expect(await answer).toEqual({ type: "signal", viewer_id: rtc.state.connection_id, signal: { type: "answer", sdp: "v=0\r\n" } });
      const offer = next(rtc.socket);
      publisher.socket.send(JSON.stringify({ type: "signal", viewer_id: rtc.state.connection_id, signal: { type: "offer", sdp: "v=0\r\n" } }));
      expect(await offer).toEqual({ type: "signal", signal: { type: "offer", sdp: "v=0\r\n" } });
      const rtcMessages = messages(rtc.socket);
      publisher.socket.send(JSON.stringify({ ...frame, viewer_id: rtc.state.connection_id }));
      publisher.socket.send(JSON.stringify({ type: "control", viewer_id: rtc.state.connection_id, data: { type: "granted", generation: "frames-only" } }));
      await ping(publisher.socket);
      await ping(rtc.socket);
      expect(rtcMessages).toEqual([{ type: "pong" }]);
      const renewed = next(viewer.socket);
      expect((await publisher.stub.fetch("https://account-tools.internal/hands/renew", {
        method: "POST", headers: headers(publisher.owner), body: JSON.stringify({ connection_id: viewer.state.connection_id }),
      })).status).toBe(200);
      expect(await renewed).toMatchObject({ type: "renewed" });
      await ping(viewer.socket);
    } finally { viewer.socket.close(); rtc.socket.close(); publisher.socket.close(); }
  });

  it.each([
    { type: "frame_request" },
    { type: "input", data: { kind: "text", text: "hello", generation: "control-1", sequence: 1 } },
    { type: "control", data: { type: "acquire" } },
  ])("rejects $type from a surface with the default WebRTC transport", async message => {
    const publisher = await host("webrtc", crypto.randomUUID()), viewer = await view(publisher);
    try {
      const disconnected = closed(viewer.socket), left = next(publisher.socket);
      viewer.socket.send(JSON.stringify(message));
      expect((await disconnected).code).toBe(1008);
      expect(await left).toEqual({ type: "viewer_left", viewer_id: viewer.state.connection_id });
      await ping(publisher.socket);
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it.each([
    { type: "answer", sdp: "v=0\r\n" },
    { type: "candidate", candidate: "candidate:test", sdpMid: "0", sdpMLineIndex: 0 },
  ])("rejects WebRTC $type from a frames viewer", async signal => {
    const publisher = await framesHost(), viewer = await view(publisher);
    try {
      const disconnected = closed(viewer.socket), left = next(publisher.socket);
      viewer.socket.send(JSON.stringify({ type: "signal", signal }));
      expect((await disconnected).code).toBe(1008);
      expect(await left).toEqual({ type: "viewer_left", viewer_id: viewer.state.connection_id });
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it("rejects a host sending WebRTC signaling to a frames viewer", async () => {
    const publisher = await framesHost(), viewer = await view(publisher);
    try {
      const hostClosed = closed(publisher.socket), viewerClosed = closed(viewer.socket);
      publisher.socket.send(JSON.stringify({ type: "signal", viewer_id: viewer.state.connection_id, signal: { type: "offer", sdp: "v=0\r\n" } }));
      expect((await hostClosed).code).toBe(1008);
      expect((await viewerClosed).code).toBe(1008);
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it("allows one outstanding pull per viewer and admits the next only after a response", async () => {
    const publisher = await framesHost(), first = await view(publisher), second = await view(publisher);
    const received = messages(publisher.socket);
    try {
      first.socket.send(JSON.stringify({ type: "frame_request" }));
      first.socket.send(JSON.stringify({ type: "frame_request" }));
      await ping(first.socket);
      second.socket.send(JSON.stringify({ type: "frame_request" }));
      await ping(second.socket);
      await ping(publisher.socket);
      expect(received).toEqual([
        { type: "frame_request", viewer_id: first.state.connection_id },
        { type: "frame_request", viewer_id: second.state.connection_id },
        { type: "pong" },
      ]);
      const firstFrame = next(first.socket);
      publisher.socket.send(JSON.stringify({ ...frame, viewer_id: first.state.connection_id }));
      expect(await firstFrame).toEqual(frame);
      await requestFrame(publisher, first);
      const secondFrame = next(second.socket);
      publisher.socket.send(JSON.stringify({ ...frame, viewer_id: second.state.connection_id }));
      expect(await secondFrame).toEqual(frame);
      await requestFrame(publisher, second);
    } finally { first.socket.close(); second.socket.close(); publisher.socket.close(); }
  });

  it("accepts the 700,000-character JPEG limit without exposing the viewer identifier", async () => {
    const publisher = await framesHost(), viewer = await view(publisher);
    try {
      await requestFrame(publisher, viewer);
      const received = next(viewer.socket), maximum = { ...frame, jpeg: "/9j/" + "A".repeat(699_996), height: 1280 };
      publisher.socket.send(JSON.stringify({ ...maximum, viewer_id: viewer.state.connection_id }));
      expect(await received).toEqual(maximum);
      await requestFrame(publisher, viewer);
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it.each([
    { name: "oversized JPEG", value: { jpeg: "/9j/" + "A".repeat(700_000) } },
    { name: "non-JPEG data", value: { jpeg: "aGVsbG8=" } },
    { name: "oversized dimensions", value: { width: 1281 } },
    { name: "zero dimensions", value: { height: 0 } },
    { name: "fractional dimensions", value: { width: 1.5 } },
  ])("revokes the host and its viewers for $name", async ({ value }) => {
    const publisher = await framesHost(), viewer = await view(publisher);
    try {
      await requestFrame(publisher, viewer);
      const hostClosed = closed(publisher.socket), viewerClosed = closed(viewer.socket);
      publisher.socket.send(JSON.stringify({ ...frame, ...value, viewer_id: viewer.state.connection_id }));
      expect((await hostClosed).code).toBe(1008);
      expect((await viewerClosed).code).toBe(1008);
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it("rejects an unsolicited second frame after the outstanding pull was fulfilled", async () => {
    const publisher = await framesHost(), viewer = await view(publisher);
    try {
      await requestFrame(publisher, viewer);
      const received = next(viewer.socket);
      publisher.socket.send(JSON.stringify({ ...frame, viewer_id: viewer.state.connection_id }));
      expect(await received).toEqual(frame);
      const hostClosed = closed(publisher.socket), viewerClosed = closed(viewer.socket);
      publisher.socket.send(JSON.stringify({ ...frame, viewer_id: viewer.state.connection_id }));
      expect((await hostClosed).code).toBe(1008);
      expect((await viewerClosed).code).toBe(1008);
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it("bounds input data by UTF-8 bytes and closes only the oversized sender", async () => {
    const publisher = await framesHost(), viewer = await view(publisher);
    try {
      const data = { kind: "text", text: "", generation: "control-1", sequence: 1 };
      const room = 8192 - new TextEncoder().encode(JSON.stringify(data)).length;
      data.text = "é".repeat(Math.floor(room / 2)) + "a".repeat(room % 2);
      expect(new TextEncoder().encode(JSON.stringify(data)).length).toBe(8192);
      const received = next(publisher.socket);
      viewer.socket.send(JSON.stringify({ type: "input", data }));
      expect(await received).toEqual({ type: "input", data, viewer_id: viewer.state.connection_id });
      const viewerClosed = closed(viewer.socket), left = next(publisher.socket);
      viewer.socket.send(JSON.stringify({ type: "input", data: { ...data, text: data.text + "a" } }));
      expect((await viewerClosed).code).toBe(1008);
      expect(await left).toEqual({ type: "viewer_left", viewer_id: viewer.state.connection_id });
      await ping(publisher.socket);
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it.each([
    { type: "frame_request" },
    { type: "input", data: { kind: "text", text: "hello", generation: "control-1", sequence: 1 } },
    { type: "control", data: { type: "acquire" } },
  ])("rejects a client-supplied viewer identifier on $type without disturbing its target", async message => {
    const publisher = await framesHost(), sender = await view(publisher), target = await view(publisher);
    try {
      const senderClosed = closed(sender.socket), left = next(publisher.socket);
      sender.socket.send(JSON.stringify({ ...message, viewer_id: target.state.connection_id }));
      expect((await senderClosed).code).toBe(1008);
      expect(await left).toEqual({ type: "viewer_left", viewer_id: sender.state.connection_id });
      await requestFrame(publisher, target);
      const received = next(target.socket);
      publisher.socket.send(JSON.stringify({ ...frame, viewer_id: target.state.connection_id }));
      expect(await received).toEqual(frame);
    } finally { sender.socket.close(); target.socket.close(); publisher.socket.close(); }
  });

  it("routes control and input only to the selected host and strips routing identifiers from replies", async () => {
    const publisher = await framesHost(), viewer = await view(publisher);
    try {
      for (const data of [{ type: "acquire" }, { type: "renew", generation: "control-1" }, { type: "release", generation: "control-1" }]) {
        const received = next(publisher.socket);
        viewer.socket.send(JSON.stringify({ type: "control", data }));
        expect(await received).toEqual({ type: "control", data, viewer_id: viewer.state.connection_id });
      }
      for (const data of [{ type: "granted", generation: "control-1" }, { type: "denied" }, { type: "revoked", generation: "control-1" }]) {
        const received = next(viewer.socket);
        publisher.socket.send(JSON.stringify({ type: "control", data, viewer_id: viewer.state.connection_id }));
        expect(await received).toEqual({ type: "control", data });
      }
      const data = { kind: "key", key: 40, down: true, generation: "control-1", sequence: 1 };
      const received = next(publisher.socket);
      viewer.socket.send(JSON.stringify({ type: "input", data }));
      expect(await received).toEqual({ type: "input", data, viewer_id: viewer.state.connection_id });
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it("requires a generation for a control grant", async () => {
    const publisher = await framesHost(), viewer = await view(publisher);
    try {
      const hostClosed = closed(publisher.socket), viewerClosed = closed(viewer.socket);
      publisher.socket.send(JSON.stringify({ type: "control", viewer_id: viewer.state.connection_id, data: { type: "granted" } }));
      expect((await hostClosed).code).toBe(1008);
      expect((await viewerClosed).code).toBe(1008);
    } finally { viewer.socket.close(); publisher.socket.close(); }
  });

  it("ignores frame and control replies naming another host's viewer", async () => {
    const owner = crypto.randomUUID(), first = await framesHost("first", owner), second = await framesHost("second", owner);
    const viewer = await view(second), received = messages(viewer.socket);
    try {
      await requestFrame(second, viewer);
      first.socket.send(JSON.stringify({ ...frame, viewer_id: viewer.state.connection_id }));
      first.socket.send(JSON.stringify({ type: "control", viewer_id: viewer.state.connection_id, data: { type: "granted", generation: "foreign" } }));
      await ping(first.socket);
      await ping(viewer.socket);
      expect(received).toEqual([{ type: "pong" }]);
      const legitimate = next(viewer.socket);
      second.socket.send(JSON.stringify({ ...frame, viewer_id: viewer.state.connection_id }));
      expect(await legitimate).toEqual(frame);
    } finally { viewer.socket.close(); first.socket.close(); second.socket.close(); }
  });

  it("revokes a replaced generation and never replays pending pulls or control to its successor", async () => {
    const publisher = await framesHost(), viewer = await view(publisher);
    let successor: Awaited<ReturnType<typeof host>> | undefined, reconnected: Awaited<ReturnType<typeof view>> | undefined;
    try {
      await requestFrame(publisher, viewer);
      const granted = next(viewer.socket);
      publisher.socket.send(JSON.stringify({ type: "control", viewer_id: viewer.state.connection_id, data: { type: "granted", generation: "old-control" } }));
      expect(await granted).toEqual({ type: "control", data: { type: "granted", generation: "old-control" } });
      const hostClosed = closed(publisher.socket), viewerClosed = closed(viewer.socket);
      successor = await framesHost(publisher.machine, publisher.owner);
      expect((await hostClosed).code).toBe(1008);
      expect((await viewerClosed).code).toBe(1008);
      expect(successor.state.generation).not.toBe(publisher.state.generation);
      const staleQuery = new URLSearchParams({ machine_id: publisher.machine, surface_id: "screen", generation: publisher.state.generation });
      expect((await publisher.stub.fetch(`https://account-tools.internal/hands/view?${staleQuery}`, {
        headers: { ...headers(publisher.owner), upgrade: "websocket" },
      })).status).toBe(409);
      expect((await publisher.stub.fetch("https://account-tools.internal/hands/renew", {
        method: "POST", headers: headers(publisher.owner), body: JSON.stringify({ connection_id: viewer.state.connection_id }),
      })).status).toBe(409);
      const successorMessages = messages(successor.socket);
      reconnected = await view(successor);
      const viewerMessages = messages(reconnected.socket);
      successor.socket.send(JSON.stringify({ ...frame, viewer_id: viewer.state.connection_id }));
      successor.socket.send(JSON.stringify({ type: "control", viewer_id: viewer.state.connection_id, data: { type: "granted", generation: "old-control" } }));
      await ping(successor.socket);
      await ping(reconnected.socket);
      expect(successorMessages).toEqual([
        { type: "viewer", viewer_id: reconnected.state.connection_id, surface_id: "screen", generation: successor.state.generation },
        { type: "pong" },
      ]);
      expect(viewerMessages).toEqual([{ type: "pong" }]);
      await requestFrame(successor, reconnected);
      const received = next(reconnected.socket);
      successor.socket.send(JSON.stringify({ ...frame, viewer_id: reconnected.state.connection_id }));
      expect(await received).toEqual(frame);
    } finally { reconnected?.socket.close(); successor?.socket.close(); viewer.socket.close(); publisher.socket.close(); }
  });
});
