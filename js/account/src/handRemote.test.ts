import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { listRemoteHands, RemoteBrowserSession, type RemoteHand } from "./handRemote.ts";

const screen: RemoteHand = {
  id: "desktop", name: "Desktop", kind: "desktop", width: 1600, height: 900, controllable: true,
  machine_id: "server:018f0000-0000-7000-8000-000000000001", machine_name: "Linux server", generation: "first",
};
const flush = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}

function fixture(t: TestContext, hand: RemoteHand = screen) {
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval", "Date"], now: 1000 });
  t.mock.method(performance, "now", () => Date.now());
  const peers: Peer[] = [], sockets: Socket[] = [];
  let catalog: readonly RemoteHand[] = [hand], status = 200, catalogReads = 0;
  let iceResponse = async (): Promise<Response> => Response.json({ iceServers: [] });
  const requests: { path: string; signal?: AbortSignal | null }[] = [];
  class Channel {
    readyState = "open"; bufferedAmount = 0; maxPacketLifeTime = null;
    onopen?: () => void; onclose?: () => void; onmessage?: (event: { data: string }) => void;
    sent: any[] = []; label: string; ordered: boolean; maxRetransmits: number | null;
    constructor(motion = false) { this.label = motion ? "remote-motion-v1" : "remote-control-v1"; this.ordered = !motion; this.maxRetransmits = motion ? 0 : null; }
    send(value: string) { this.sent.push(JSON.parse(value)); }
    close() { this.readyState = "closed"; this.onclose?.(); }
    message(value: unknown) { this.onmessage?.({ data: JSON.stringify(value) }); }
  }
  class Peer {
    connectionState = "new";
    remoteDescription?: RTCSessionDescriptionInit; localDescription?: RTCSessionDescriptionInit;
    appliedCandidates: unknown[] = []; calls: string[] = [];
    config: RTCConfiguration;
    onconnectionstatechange?: (() => void) | null; ondatachannel?: ((event: { channel: Channel }) => void) | null;
    ontrack?: ((event: { track: unknown }) => void) | null;
    reliable = new Channel(); motion = new Channel(true);
    constructor(config: RTCConfiguration) { this.config = config; peers.push(this); }
    getConfiguration() { return this.config; }
    setConfiguration(config: RTCConfiguration) { this.config = config; this.calls.push("configuration"); }
    async setRemoteDescription(description: RTCSessionDescriptionInit) { this.remoteDescription = description; this.calls.push("offer"); }
    async addIceCandidate(candidate: unknown) { this.appliedCandidates.push(candidate); this.calls.push("candidate"); }
    async createAnswer() { this.calls.push("answer"); return { type: "answer" as const, sdp: "answer" }; }
    async setLocalDescription(description: RTCSessionDescriptionInit) { this.localDescription = description; }
    open() { this.connectionState = "connected"; this.ondatachannel?.({ channel: this.reliable }); this.ondatachannel?.({ channel: this.motion }); this.onconnectionstatechange?.(); }
    fail() { this.connectionState = "failed"; this.onconnectionstatechange?.(); }
    close() { this.connectionState = "closed"; this.reliable.close(); this.motion.close(); this.onconnectionstatechange?.(); }
  }
  class Socket {
    static OPEN = 1; readyState = 1; bufferedAmount = 0; url: URL;
    onclose?: (() => void) | null; onerror?: (() => void) | null; onmessage?: ((event: { data: string }) => void) | null;
    constructor(url: URL) { this.url = url; sockets.push(this); }
    close() { this.readyState = 3; this.onclose?.(); }
    sent: any[] = [];
    send(value: string) { this.sent.push(JSON.parse(value)); }
    message(value: unknown) { this.onmessage?.({ data: JSON.stringify(value) }); }
  }
  type Bitmap = { width: number; height: number; close(): void };
  const drawn: Bitmap[] = [], decoded: Blob[] = [];
  let decode = async (_source: Blob): Promise<Bitmap> => ({ width: 640, height: 360, close() {} });
  const canvas = { width: 0, height: 0, getContext: () => ({ drawImage(bitmap: Bitmap) { drawn.push(bitmap); } }) };
  const globals = {
    location: new URL("https://account.example"), RTCPeerConnection: Peer, WebSocket: Socket,
    MediaStream: class { constructor(publicTracks: unknown[]) { void publicTracks; } },
    createImageBitmap: (source: Blob) => { decoded.push(source); return decode(source); },
  };
  for (const [name, value] of Object.entries(globals)) {
    const previous = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
    t.after(() => { if (previous) Object.defineProperty(globalThis, name, previous); else Reflect.deleteProperty(globalThis, name); });
  }
  t.mock.method(globalThis, "fetch", async (path: string, options?: RequestInit) => {
    requests.push({ path, signal: options?.signal });
    if (status !== 200) return Response.json({}, { status });
    if (path.endsWith("/screens")) { catalogReads++; return Response.json({ surfaces: catalog }); }
    if (path.endsWith("/ice")) return iceResponse();
    return Response.json({ iceServers: [] });
  });
  const video = { srcObject: null as unknown, play: async () => {} };
  const session = new RemoteBrowserSession(hand, video as HTMLVideoElement, () => {}, canvas as unknown as HTMLCanvasElement);
  t.after(() => session.close());
  return {
    peers, sockets, requests, session, video, canvas, drawn, decoded,
    setDecode(value: typeof decode) { decode = value; },
    setIceResponse(value: typeof iceResponse) { iceResponse = value; },
    get catalogReads() { return catalogReads; },
    setCatalog(value: readonly RemoteHand[]) { catalog = value; },
    setStatus(value: number) { status = value; },
    async tick(ms: number) { t.mock.timers.tick(ms); await flush(); },
  };
}

test("WebRTC opens its viewer socket during ICE lookup and answers the initial offer with one credential request", async t => {
  const f = fixture(t), ice = deferred<Response>();
  f.setIceResponse(() => ice.promise);
  const connecting = f.session.connect();
  assert.equal(f.sockets.length, 1, "socket handshake must not wait for ICE HTTP response");
  assert.equal(f.peers.length, 0);
  f.sockets[0]!.message({ type: "ready", connection_id: "viewer" });
  const candidate = { type: "candidate", candidate: "candidate:test", sdpMid: "0", sdpMLineIndex: 0 };
  f.sockets[0]!.message({ type: "signal", signal: candidate });
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "initial" } });
  await flush(); assert.deepEqual(f.sockets[0]!.sent, []);
  ice.resolve(Response.json({ iceServers: [{ urls: "stun:first.example" }] }));
  await connecting; await flush();
  assert.deepEqual(f.peers[0]!.config.iceServers, [{ urls: "stun:first.example" }]);
  assert.deepEqual(f.peers[0]!.calls, ["offer", "candidate", "answer"]);
  assert.deepEqual(f.peers[0]!.appliedCandidates, [candidate]);
  assert.equal(f.requests.filter(r => r.path.endsWith("/ice")).length, 1);
  assert.deepEqual(f.sockets[0]!.sent, [{ type: "signal", signal: { type: "answer", sdp: "answer" } }]);
});

test("WebRTC refreshes credentials before answering a later ICE restart offer", async t => {
  const f = fixture(t);
  await f.session.connect();
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "initial" } }); await flush();
  const ice = deferred<Response>(); f.setIceResponse(() => ice.promise);
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "restart" } }); await flush();
  assert.equal(f.sockets[0]!.sent.length, 1);
  assert.equal(f.peers[0]!.remoteDescription?.sdp, "initial");
  ice.resolve(Response.json({ iceServers: [{ urls: "stun:refreshed.example" }] })); await flush();
  assert.equal(f.requests.filter(r => r.path.endsWith("/ice")).length, 2);
  assert.deepEqual(f.peers[0]!.config.iceServers, [{ urls: "stun:refreshed.example" }]);
  assert.deepEqual(f.peers[0]!.calls, ["offer", "answer", "configuration", "offer", "answer"]);
  assert.equal(f.sockets[0]!.sent.length, 2);
});

test("closing while ICE is pending aborts it and discards queued offers and late credentials", async t => {
  const f = fixture(t), ice = deferred<Response>(); f.setIceResponse(() => ice.promise);
  const connecting = f.session.connect();
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "stale" } }); await flush();
  f.session.close();
  assert.equal(f.requests[0]!.signal!.aborted, true);
  ice.resolve(Response.json({ iceServers: [] })); await connecting; await flush();
  assert.equal(f.peers.length, 0); assert.deepEqual(f.sockets[0]!.sent, []);
  assert.equal(f.sockets[0]!.readyState, 3);
});

test("initial ICE authorization failure closes the concurrent viewer socket without applying its offer", async t => {
  const f = fixture(t), ice = deferred<Response>(); f.setIceResponse(() => ice.promise);
  const connecting = f.session.connect();
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "unauthorized" } });
  ice.resolve(Response.json({}, { status: 403 })); await connecting; await flush();
  assert.equal(f.session.state.status, "This remote session is no longer authorized.");
  assert.equal(f.sockets[0]!.readyState, 3); assert.equal(f.peers.length, 0);
  await f.tick(100_000); assert.equal(f.sockets.length, 1);
});

test("suspension during restart credential refresh cannot answer from the retired peer", async t => {
  const f = fixture(t);
  await f.session.connect();
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "initial" } }); await flush();
  const ice = deferred<Response>(); f.setIceResponse(() => ice.promise);
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "restart" } }); await flush();
  f.session.suspend(); assert.equal(f.requests.at(-1)!.signal!.aborted, true);
  ice.resolve(Response.json({ iceServers: [{ urls: "stun:late.example" }] })); await flush();
  assert.equal(f.session.state.status, "Paused");
  assert.equal(f.sockets[0]!.sent.length, 1);
  assert.deepEqual(f.peers[0]!.calls, ["offer", "answer"]);
  assert.equal(f.peers[0]!.connectionState, "closed");
});

test("messages queued behind a slow ICE lookup are bounded and cannot negotiate after teardown", async t => {
  const f = fixture(t), ice = deferred<Response>(); f.setIceResponse(() => ice.promise);
  const connecting = f.session.connect();
  for (let i = 0; i < 129; i++) f.sockets[0]!.message({ type: "signal", signal: { type: "candidate", candidate: String(i) } });
  assert.equal(f.session.state.connecting, false); assert.equal(f.sockets[0]!.readyState, 3);
  ice.resolve(Response.json({ iceServers: [] })); await connecting; await flush();
  assert.equal(f.peers.length, 0); assert.deepEqual(f.sockets[0]!.sent, []);
});

test("suspending a server screen releases control, clears its frame, and resumes with the current publication", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open();
  assert.equal(f.session.state.connected, true);
  f.session.takeControl(); f.peers[0]!.reliable.message({ type: "granted", generation: "lease1" });
  f.session.input({ kind: "key", key: 4, down: true });
  assert.equal(f.session.state.controlling, true);
  f.video.srcObject = { frame: "old" };
  const oldMessage = f.peers[0]!.reliable.onmessage!;
  f.session.suspend();
  assert.equal(f.session.state.status, "Paused");
  assert.equal(f.session.state.controlling, false);
  assert.equal(f.video.srcObject, null);
  assert.deepEqual(f.peers[0]!.reliable.sent.at(-1), { type: "release", generation: "lease1" });
  await f.tick(100_000);
  assert.equal(f.peers.length, 1);
  f.setCatalog([{ ...screen, generation: "after-restart" }]);
  f.session.resume(); await flush(); f.peers[1]!.open();
  assert.equal(f.sockets[1]!.url.searchParams.get("generation"), "after-restart");
  assert.equal(f.session.state.connected, true);
  assert.equal(f.session.state.controlling, false);
  oldMessage({ data: JSON.stringify({ type: "granted", generation: "stale" }) });
  assert.equal(f.session.state.controlling, false);
  assert.deepEqual(f.peers[1]!.reliable.sent, []);
});

test("automatic recovery waits through a VM restart longer than three short retries", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.peers[0]!.fail();
  f.setCatalog([]);
  for (const delay of [1000, 2000, 4000]) await f.tick(delay);
  assert.equal(f.catalogReads, 3);
  assert.equal(f.session.state.connecting, true);
  assert.equal(f.session.hand.machine_id, screen.machine_id);
  f.setCatalog([{ ...screen, generation: "new" }]);
  await f.tick(8000); f.peers[1]!.open();
  assert.equal(f.sockets[1]!.url.searchParams.get("generation"), "new");
  assert.equal(f.session.state.connected, true);
  assert.equal(f.session.state.controlling, false);
});

test("recovery stops at 90 seconds, retains the screen, and permits explicit retry", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.peers[0]!.fail(); f.setCatalog([]);
  for (let i = 0; i < 90; i++) await f.tick(1000);
  assert.equal(f.session.state.connecting, false);
  assert.equal(f.session.state.connected, false);
  assert.equal(f.session.hand.id, screen.id);
  const reads = f.catalogReads;
  await f.tick(120_000); assert.equal(f.catalogReads, reads);
  f.setCatalog([{ ...screen, generation: "manual" }]);
  f.session.reconnect(); await flush(); f.peers[1]!.open();
  assert.equal(f.session.state.connected, true);
  assert.equal(f.session.hand.generation, "manual");
});

test("authorization loss is terminal and a closed viewer never resumes", async t => {
  const f = fixture(t); f.setStatus(401);
  await f.session.connect();
  assert.equal(f.session.state.status, "This remote session is no longer authorized.");
  assert.equal(f.session.state.connecting, false);
  await f.tick(100_000); assert.equal(f.requests.length, 1);
  f.session.close(); f.setStatus(200); f.session.reconnect(); f.session.resume();
  await flush(); assert.equal(f.requests.length, 1);
});

test("a stalled reconnect is aborted in ten seconds and late callbacks cannot clear a later connection", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.peers[0]!.fail();
  await f.tick(1000);
  const staleClose = f.sockets[1]!.onclose!;
  const staleTrack = f.peers[1]!.ontrack!;
  const signal = f.requests.at(-1)!.signal!;
  await f.tick(10_000);
  assert.equal(signal.aborted, true);
  assert.equal(f.peers[1]!.connectionState, "closed");
  await f.tick(2000); f.peers[2]!.open();
  staleClose(); staleTrack({ track: "stale" });
  assert.equal(f.session.state.connected, true);
  assert.equal(f.video.srcObject, null);
});

test("malformed screen catalogs stop recovery instead of selecting an invalid publication", async t => {
  const f = fixture(t); f.setCatalog([{ ...screen, generation: "" }]);
  await assert.rejects(listRemoteHands(), /Invalid screen catalog/);
  f.session.reconnect(); await flush();
  assert.equal(f.session.state.connecting, false);
  assert.equal(f.session.state.status, "Invalid screen catalog.");
  await f.tick(100_000); assert.equal(f.peers.length, 0);
});

test("signaling pongs do not extend the authorization lease", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open();
  await f.tick(20_000);
  f.sockets[0]!.onmessage!({ data: JSON.stringify({ type: "pong" }) }); await flush();
  assert.equal(f.session.state.connected, true);
  await f.tick(5000);
  assert.equal(f.session.state.connected, false);
  assert.equal(f.session.state.connecting, false);
  assert.equal(f.session.state.status, "This remote session is no longer authorized.");
});

const frameHand: RemoteHand = { ...screen, machine_id: "sandbox:desktop", transport: "frames-v1" };
// A SOF header for the allocation boundary tests; the mocked bitmap decoder
// below is replaced by the real browser JPEG decoder in runtime checks.
function frame(width = 640, height = 360) {
  const jpeg = Buffer.from([0xff, 0xd8, 0xff, 0xc0, 0, 17, 8, height >> 8, height & 255, width >> 8, width & 255,
    3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1, 0xff, 0xd9]).toString("base64");
  return { type: "frame", jpeg, width, height };
}

for (const transport of ["webrtc", "frames-v1"] as const) {
  async function controlFixture(t: TestContext) {
    const f = fixture(t, transport === "frames-v1" ? frameHand : screen);
    await f.session.connect();
    if (transport === "frames-v1") {
      f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
      f.sockets[0]!.message(frame()); await flush();
    } else f.peers[0]!.open();
    return {
      ...f,
      async receive(value: unknown) {
        if (transport === "frames-v1") f.sockets[0]!.message({ type: "control", data: value });
        else f.peers[0]!.reliable.message(value);
        await flush();
      },
      messages() {
        return transport === "frames-v1" ? f.sockets[0]!.sent.filter(message => message.type === "control" || message.type === "input").map(message => message.data) : f.peers[0]!.reliable.sent;
      },
    };
  }

  test(`${transport}: explicit retake waits for release acknowledgement and starts a fresh input sequence`, async t => {
    const f = await controlFixture(t);
    f.session.takeControl(); await f.receive({ type: "granted", generation: "first" });
    f.session.input({ kind: "text", text: "before" });
    f.session.releaseControl(); f.session.takeControl(); f.session.takeControl();
    f.session.input({ kind: "text", text: "discard" });
    assert.equal(f.session.state.controlling, false);
    assert.deepEqual(f.messages().map(message => message.type ?? message.kind), ["acquire", "text", "release"]);
    await f.receive({ type: "revoked", generation: "stale" });
    assert.equal(f.messages().length, 3);
    // Legacy hosts omit the generation on their release acknowledgement.
    await f.receive({ type: "revoked" });
    assert.deepEqual(f.messages().at(-1), { type: "acquire" });
    await f.receive({ type: "granted", generation: "second" });
    f.session.input({ kind: "text", text: "after" });
    assert.deepEqual(f.messages().at(-1), { kind: "text", text: "after", generation: "second", sequence: 1 });
    await f.receive({ type: "revoked", generation: "first" });
    assert.equal(f.session.state.controlling, true);
    const sent = f.messages().length;
    await f.receive({ type: "revoked", generation: "second" });
    assert.equal(f.session.state.controlling, false);
    assert.equal(f.messages().length, sent, "host revocation must neither echo release nor reacquire");
  });

  test(`${transport}: a cancelled pending grant is released before an explicit retake`, async t => {
    const f = await controlFixture(t);
    f.session.takeControl(); f.session.releaseControl(); f.session.takeControl();
    assert.deepEqual(f.messages(), [{ type: "acquire" }]);
    await f.receive({ type: "granted", generation: "cancelled" });
    assert.equal(f.session.state.controlling, false);
    assert.deepEqual(f.messages().at(-1), { type: "release", generation: "cancelled" });
    f.session.input({ kind: "key", key: 4, down: true });
    assert.equal(f.messages().length, 2);
    await f.receive({ type: "revoked", generation: "cancelled" });
    assert.deepEqual(f.messages().at(-1), { type: "acquire" });
    await f.receive({ type: "granted", generation: "fresh" });
    assert.equal(f.session.state.controlling, true);
  });

  test(`${transport}: releasing again cancels a queued retake`, async t => {
    const f = await controlFixture(t);
    f.session.takeControl(); await f.receive({ type: "granted", generation: "first" });
    f.session.releaseControl(); f.session.takeControl(); f.session.releaseControl();
    await f.receive({ type: "revoked", generation: "first" });
    assert.deepEqual(f.messages(), [{ type: "acquire" }, { type: "release", generation: "first" }]);
    assert.equal(f.session.state.controlling, false);
  });

  test(`${transport}: an unsolicited revoke cancels a pending grant without reacquiring`, async t => {
    const f = await controlFixture(t);
    f.session.takeControl(); await f.receive({ type: "revoked" });
    await f.receive({ type: "granted", generation: "late" });
    assert.equal(f.session.state.controlling, false);
    await f.receive({ type: "revoked", generation: "late" });
    assert.deepEqual(f.messages(), [{ type: "acquire" }, { type: "release", generation: "late" }]);
    f.session.takeControl(); await f.receive({ type: "denied" });
    assert.equal(f.session.state.status, "Another viewer is controlling this screen.");
    assert.equal(f.messages().length, 3);
  });

  test(`${transport}: denial of a cancelled acquire admits only a separately requested retake`, async t => {
    const f = await controlFixture(t);
    f.session.takeControl(); f.session.releaseControl(); f.session.takeControl();
    await f.receive({ type: "denied" });
    assert.deepEqual(f.messages(), [{ type: "acquire" }, { type: "acquire" }]);
    await f.receive({ type: "granted", generation: "second" });
    assert.equal(f.session.state.controlling, true);
  });

  test(`${transport}: disconnect discards a queued retake and late acknowledgement`, async t => {
    const f = await controlFixture(t);
    f.session.takeControl(); await f.receive({ type: "granted", generation: "first" });
    f.session.releaseControl(); f.session.takeControl();
    const late = transport === "frames-v1" ? f.sockets[0]!.onmessage! : f.peers[0]!.reliable.onmessage!;
    f.session.suspend(); f.session.resume(); await flush();
    late({ data: JSON.stringify(transport === "frames-v1" ? { type: "control", data: { type: "revoked" } } : { type: "revoked" }) });
    await flush();
    assert.equal(f.session.state.controlling, false);
    const sent = transport === "frames-v1" ? f.sockets[1]!.sent : f.peers[1]!.reliable.sent;
    assert.deepEqual(sent, []);
  });
}

test("frames-v1 establishes readiness only after rendering and paces a single outstanding pull without ICE", async t => {
  const f = fixture(t, frameHand);
  await f.session.connect();
  assert.equal(f.peers.length, 0); assert.equal(f.requests.length, 0);
  assert.equal(f.session.state.connected, false);
  f.sockets[0]!.message({ type: "ready", connection_id: "frame-viewer" }); await flush();
  assert.deepEqual(f.sockets[0]!.sent, [{ type: "frame_request" }]);
  let finish!: (bitmap: { width: number; height: number; close(): void }) => void;
  f.setDecode(() => new Promise(resolve => { finish = resolve; }));
  f.sockets[0]!.message(frame()); await flush();
  await f.tick(500);
  assert.equal(f.session.state.connected, false); assert.equal(f.sockets[0]!.sent.length, 1);
  finish({ width: 640, height: 360, close() {} }); await flush();
  assert.equal(f.session.state.connected, true); assert.equal(f.drawn.length, 1);
  assert.deepEqual([f.canvas.width, f.canvas.height], [640, 360]);
  await f.tick(0); assert.equal(f.sockets[0]!.sent.length, 2);
  f.sockets[0]!.message(frame()); await flush(); finish({ width: 640, height: 360, close() {} }); await flush();
  await f.tick(99); assert.equal(f.sockets[0]!.sent.length, 2);
  await f.tick(1); assert.equal(f.sockets[0]!.sent.length, 3);
  assert.equal(f.requests.length, 0);
});

test("frame control and input retain their lease and sequence, and suspension resumes viewing a fresh publication", async t => {
  const f = fixture(t, frameHand);
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer1" }); await flush();
  f.sockets[0]!.message(frame()); await flush(); f.session.takeControl();
  assert.deepEqual(f.sockets[0]!.sent.at(-1), { type: "control", data: { type: "acquire" } });
  f.sockets[0]!.message({ type: "control", data: { type: "granted", generation: "lease1" } }); await flush();
  f.session.input({ kind: "text", text: "hello" });
  assert.deepEqual(f.sockets[0]!.sent.at(-1), { type: "input", data: { kind: "text", text: "hello", sequence: 1, generation: "lease1" } });
  f.session.suspend();
  assert.equal(f.session.state.controlling, false); assert.deepEqual([f.canvas.width, f.canvas.height], [0, 0]);
  assert.deepEqual(f.sockets[0]!.sent.at(-1), { type: "control", data: { type: "release", generation: "lease1" } });
  const sent = f.sockets[0]!.sent.length;
  await f.tick(20_000); assert.equal(f.sockets[0]!.sent.length, sent);
  f.setCatalog([{ ...frameHand, generation: "new-publication" }]);
  f.session.resume(); await flush(); f.sockets[1]!.message({ type: "ready", connection_id: "viewer2" }); await flush();
  f.sockets[1]!.message(frame()); await flush();
  assert.equal(f.session.state.status, "Watching"); assert.equal(f.session.state.controlling, false);
  assert.equal(f.sockets[1]!.url.searchParams.get("generation"), "new-publication");
  assert.deepEqual(f.sockets[1]!.sent, [{ type: "frame_request" }]);
  assert.ok(f.requests.every(request => request.path.endsWith("/screens")));
});

test("a bitmap decoded after disconnect is closed without painting over the next publication", async t => {
  const f = fixture(t, frameHand);
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "old" }); await flush();
  let finish!: (bitmap: { width: number; height: number; close(): void }) => void;
  f.setDecode(() => new Promise(resolve => { finish = resolve; }));
  f.sockets[0]!.message(frame()); await flush(); f.session.suspend();
  f.setDecode(async () => ({ width: 640, height: 360, close() {} }));
  f.session.resume(); await flush(); f.sockets[1]!.message({ type: "ready", connection_id: "new" }); await flush();
  f.sockets[1]!.message(frame()); await flush();
  let closed = false; finish({ width: 640, height: 360, close() { closed = true; } }); await flush();
  assert.equal(closed, true); assert.equal(f.drawn.length, 1); assert.equal(f.session.state.connected, true);
});

test("frames reject oversize decoded dimensions before allocating a bitmap", async t => {
  const f = fixture(t, frameHand);
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
  f.sockets[0]!.message({ ...frame(3000, 2000), width: 640, height: 360 }); await flush();
  assert.equal(f.decoded.length, 0); assert.equal(f.session.state.connected, false);
  assert.equal(f.session.state.connecting, false); assert.equal(f.canvas.width, 0);
});

test("an unsolicited second frame cannot queue behind an in-progress decode", async t => {
  const f = fixture(t, frameHand);
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
  let finish!: (bitmap: { width: number; height: number; close(): void }) => void;
  f.setDecode(() => new Promise(resolve => { finish = resolve; }));
  f.sockets[0]!.message(frame()); await flush(); f.sockets[0]!.message(frame());
  assert.equal(f.session.state.connecting, false); assert.equal(f.sockets[0]!.readyState, 3);
  finish({ width: 640, height: 360, close() {} }); await flush();
  assert.equal(f.decoded.length, 1); assert.equal(f.drawn.length, 0);
});

test("a stalled frame stream clears the picture and retries without falling back to WebRTC", async t => {
  const f = fixture(t, frameHand);
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
  f.sockets[0]!.message(frame()); await flush(); await f.tick(100);
  await f.tick(10_000);
  assert.equal(f.session.state.connected, false); assert.equal(f.session.state.connecting, true); assert.equal(f.canvas.width, 0);
  await f.tick(1000);
  assert.equal(f.sockets.length, 2); assert.equal(f.peers.length, 0);
  assert.ok(f.requests.every(request => !request.path.endsWith("/ice")));
});
