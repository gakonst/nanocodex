import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { canStartBroadcast, listRemoteHands, RemoteBrowserSession, type RemoteHand } from "./handRemote.ts";

const screen: RemoteHand = {
  id: "desktop", name: "Desktop", kind: "desktop", width: 1600, height: 900, controllable: true,
  machine_id: "server:018f0000-0000-7000-8000-000000000001", machine_name: "Linux server", generation: "first",
};
const flush = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };

test("relative control is negotiated per lease and deltas use reliable ordering", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open();
  f.session.takeControl();
  assert.equal(f.session.state.controlPending, true);
  f.peers[0]!.reliable.message({ type: "granted", generation: "relative", relativePointer: true });
  assert.equal(f.session.state.relativePointer, true);
  assert.equal(f.session.state.controlPending, false);
  f.session.input({ kind: "relativeMove", deltaX: 12.5, deltaY: -2 });
  f.session.input({ kind: "button", button: 0, down: true });
  assert.deepEqual(f.peers[0]!.reliable.sent.slice(-2), [
    { kind: "relativeMove", deltaX: 12.5, deltaY: -2, sequence: 1, generation: "relative" },
    { kind: "button", button: 0, down: true, sequence: 2, generation: "relative" },
  ]);
  assert.equal(f.peers[0]!.motion.sent.length, 0, "relative deltas must not be dropped or reordered as absolute motion");
  f.session.releaseControl();
  assert.equal(f.session.state.relativePointer, false);
});

test("legacy grants and rejected control never leave mouse capture pending", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open();
  f.session.takeControl(); f.peers[0]!.reliable.message({ type: "denied" });
  assert.equal(f.session.state.controlPending, false);
  f.session.takeControl(); f.peers[0]!.reliable.message({ type: "granted", generation: "legacy" });
  assert.equal(f.session.state.relativePointer, false);
  assert.equal(f.session.state.controlPending, false);
});

test("malformed relative capability tears down capture intent", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open();
  f.session.takeControl(); f.peers[0]!.reliable.message({ type: "granted", generation: "bad", relativePointer: "yes" });
  assert.equal(f.session.state.connected, false);
  assert.equal(f.session.state.controlPending, false);
});
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
  let catalogResponse = async (): Promise<Response> => Response.json({ surfaces: catalog });
  let iceResponse = async (): Promise<Response> => Response.json({ iceServers: [] });
  const requests: { path: string; signal?: AbortSignal | null }[] = [];
  class Channel {
    readyState = "open"; bufferedAmount = 0; maxPacketLifeTime = null;
    bufferedAmountLowThreshold = 0; onbufferedamountlow?: () => void;
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
    transceivers: { receiver: { track: { kind: string } }; sender: { replaceTrack(track: unknown): Promise<void> }; stopped: boolean; direction: string; currentDirection: string | null }[] = [];
    getTransceivers() { return this.transceivers; }
    statsCalls = 0;
    stats = async (): Promise<RTCStatsReport> => new Map() as RTCStatsReport;
    getStats() { this.statsCalls++; return this.stats(); }
    config: RTCConfiguration;
    onconnectionstatechange?: (() => void) | null; ondatachannel?: ((event: { channel: Channel }) => void) | null;
    ontrack?: ((event: { track: unknown; receiver?: unknown }) => void) | null;
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
    onopen?: (() => void) | null;
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
  let capture = async (): Promise<MediaStream> => new MediaStream();
  const captures: MediaStreamConstraints[] = [];
  const globals = {
    navigator: { mediaDevices: { getUserMedia: (constraints: MediaStreamConstraints) => { captures.push(constraints); return capture(); } } },
    location: new URL("https://account.example"), RTCPeerConnection: Peer, WebSocket: Socket,
    MediaStream: class {
      tracks: any[] = [];
      getTracks() { return this.tracks; }
      getAudioTracks() { return this.tracks.filter(track => track.kind === "audio"); }
      addTrack(track: any) { this.tracks.push(track); }
      removeTrack(track: any) { this.tracks = this.tracks.filter(value => value !== track); }
    },
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
    if (path.endsWith("/screens")) { catalogReads++; return catalogResponse(); }
    if (path.endsWith("/ice")) return iceResponse();
    return Response.json({ iceServers: [] });
  });
  const frames = new Map<number, () => void>();
  let nextFrame = 0;
  const video = Object.assign(new EventTarget(), {
    muted: true, srcObject: null as unknown, readyState: 0, videoWidth: 0, videoHeight: 0, play: async () => {},
    requestVideoFrameCallback(callback: () => void) { const id = ++nextFrame; frames.set(id, callback); return id; },
    cancelVideoFrameCallback(id: number) { frames.delete(id); },
  });
  let changed = (_state: RemoteBrowserSession["state"]) => {};
  const session = new RemoteBrowserSession(hand, video as HTMLVideoElement, state => changed(state), canvas as unknown as HTMLCanvasElement);
  t.after(() => session.close());
  return {
    peers, sockets, requests, session, video, canvas, drawn, decoded, captures, frames,
    // Long-lived control/auth tests need actual advancing video, independently
    // of the presentation callback tests and optional diagnostics.
    playVideo() {
      peers.at(-1)!.ontrack?.({ track: { kind: "video", id: "playing-picture", stop() {} } });
      Object.assign(video, { getVideoPlaybackQuality: () => ({ totalVideoFrames: Date.now(), droppedVideoFrames: 0 }) });
    },
    setChanged(value: typeof changed) { changed = value; },
    setCapture(value: typeof capture) { capture = value; },
    setDecode(value: typeof decode) { decode = value; },
    setCatalogResponse(value: typeof catalogResponse) { catalogResponse = value; },
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
  await flush(); assert.equal(f.sockets[0]!.sent.length, 0);
  ice.resolve(Response.json({ iceServers: [{ urls: "stun:first.example" }] }));
  await connecting; await flush();
  assert.deepEqual(f.peers[0]!.config.iceServers, [{ urls: "stun:first.example" }]);
  assert.equal(f.peers[0]!.config.iceCandidatePoolSize, 1);
  assert.deepEqual(f.peers[0]!.calls, ["offer", "answer", "candidate"]);
  assert.deepEqual(f.peers[0]!.appliedCandidates, [candidate]);
  assert.equal(f.requests.filter(r => r.path.endsWith("/ice")).length, 1);
  assert.deepEqual(f.sockets[0]!.sent, [{ type: "signal", signal: { type: "answer", sdp: "answer" } }]);
});

test("reconnect overlaps fresh TURN lookup with discovery and waits for the current publication before opening", async t => {
  const f = fixture(t); await f.session.connect();
  const catalog = deferred<Response>(), ice = deferred<Response>();
  f.setCatalogResponse(() => catalog.promise); f.setIceResponse(() => ice.promise);
  f.session.reconnect();
  assert.equal(f.requests.filter(r => r.path.endsWith("/ice")).length, 2, "fresh TURN starts before discovery completes");
  assert.equal(f.catalogReads, 1);
  assert.equal(f.sockets.length, 1, "the old publication must never open a new socket");
  ice.resolve(Response.json({ iceServers: [{ urls: "turn:pool.example", username: "fresh", credential: "fresh" }] }));
  await flush(); assert.equal(f.peers.length, 1);
  catalog.resolve(Response.json({ surfaces: [{ ...screen, generation: "new" }] })); await flush();
  assert.equal(f.sockets[1]!.url.searchParams.get("generation"), "new");
  assert.equal(f.peers[1]!.config.iceCandidatePoolSize, 1, "gather this attempt's candidates before waiting for the offer");
  assert.equal(f.peers[1]!.remoteDescription, undefined);
  assert.equal(f.peers[1]!.config.iceTransportPolicy, "all");
  assert.equal(f.peers[1]!.config.iceServers![0]!.username, "fresh");
});

test("TURN authorization loss aborts concurrent discovery immediately", async t => {
  const f = fixture(t); await f.session.connect();
  const catalog = deferred<Response>(); f.setCatalogResponse(() => catalog.promise);
  f.setIceResponse(async () => Response.json({}, { status: 403 }));
  f.session.reconnect(); await flush();
  assert.equal(f.session.state.status, "This remote session is no longer authorized.");
  assert.equal(f.requests.filter(r => r.path.endsWith("/screens")).at(-1)!.signal!.aborted, true);
  catalog.resolve(Response.json({ surfaces: [{ ...screen, transport: "frames-v1" }] })); await flush();
  await f.tick(100_000);
  assert.equal(f.sockets.length, 1); assert.equal(f.peers.length, 1);
  assert.equal(f.session.state.connecting, false);
});

test("a reconnect can switch to frames-v1 even if speculative TURN is unavailable", async t => {
  const f = fixture(t); await f.session.connect();
  f.setCatalog([{ ...screen, transport: "frames-v1", generation: "frames" }]);
  f.setIceResponse(async () => Response.json({}, { status: 503 }));
  f.session.reconnect(); await flush();
  assert.equal(f.peers.length, 1);
  assert.equal(f.sockets.length, 2);
  assert.equal(f.session.hand.transport, "frames-v1");
  assert.equal(f.sockets[1]!.readyState, 1);
  assert.equal(f.session.state.connecting, true);
});

test("a frames-v1 publication switching to WebRTC starts a fresh candidate pool after discovery", async t => {
  const f = fixture(t, { ...screen, transport: "frames-v1" }); await f.session.connect();
  f.setCatalog([screen]); f.session.reconnect(); await flush();
  assert.equal(f.requests.filter(r => r.path.endsWith("/ice")).length, 1);
  assert.equal(f.peers[0]!.config.iceCandidatePoolSize, 1);
  assert.equal(f.sockets.length, 2);
});

test("retiring during overlapping discovery and TURN lookup cannot create a pooled peer or socket", async t => {
  const f = fixture(t); await f.session.connect();
  const catalog = deferred<Response>(), ice = deferred<Response>();
  f.setCatalogResponse(() => catalog.promise); f.setIceResponse(() => ice.promise);
  f.session.reconnect(); await flush(); f.session.suspend();
  for (const request of f.requests.slice(-2)) assert.equal(request.signal!.aborted, true);
  catalog.resolve(Response.json({ surfaces: [screen] }));
  ice.resolve(Response.json({ iceServers: [] })); await flush();
  assert.equal(f.peers.length, 1); assert.equal(f.sockets.length, 1);
  assert.equal(f.session.state.status, "Paused");
});

for (const retire of [false, true]) {
  test(`WebRTC sends its answer before 32 early candidates drain${retire ? " and fences retirement mid-drain" : " in order"}`, async t => {
    const f = fixture(t); await f.session.connect();
    const peer = f.peers[0]!, gate = deferred<void>();
    const candidates = Array.from({ length: 32 }, (_, index) => ({
      type: "candidate", candidate: "candidate:" + index, sdpMid: "0", sdpMLineIndex: 0,
    }));
    for (const candidate of candidates) { f.sockets[0]!.message({ type: "signal", signal: candidate }); await flush(); }
    let attempts = 0;
    const add = peer.addIceCandidate.bind(peer);
    t.mock.method(peer, "addIceCandidate", async (candidate: unknown) => { attempts++; await gate.promise; await add(candidate); });
    f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "initial" } }); await flush();
    assert.equal(attempts, 1);
    assert.deepEqual(peer.appliedCandidates, []);
    assert.deepEqual(f.sockets[0]!.sent, [{ type: "signal", signal: { type: "answer", sdp: "answer" } }],
      "answer must be sent while the first queued addIceCandidate is still blocked");
    if (retire) { f.session.suspend(); f.session.resume(); await flush(); }
    gate.resolve();
    for (let i = 0; i < candidates.length; i++) await flush();
    assert.deepEqual(peer.appliedCandidates, retire ? candidates.slice(0, 1) : candidates);
    assert.equal(attempts, retire ? 1 : 32);
    if (retire) {
      assert.deepEqual(f.peers[1]!.appliedCandidates, []);
      assert.deepEqual(f.sockets[1]!.sent, []);
    }
  });
}

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
  assert.equal(f.peers.length, 0); assert.equal(f.sockets[0]!.sent.length, 0);
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
  assert.equal(f.peers.length, 0); assert.equal(f.sockets[0]!.sent.length, 0);
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

test("a stalled reconnect is aborted in twenty-five seconds and late callbacks cannot clear a later connection", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.peers[0]!.fail();
  await f.tick(1000);
  const staleClose = f.sockets[1]!.onclose!;
  const staleTrack = f.peers[1]!.ontrack!;
  const signal = f.requests.at(-1)!.signal!;
  await f.tick(25_000);
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
  await f.session.connect(); f.peers[0]!.open(); f.playVideo();
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
  await f.tick(33); assert.equal(f.sockets[0]!.sent.length, 2);
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

test("windowed JPEG bursts decode only the active and newest image and return every consumed credit after painting", async t => {
  const f = fixture(t, { ...frameHand, frame_window: 6 });
  await f.session.connect();
  f.sockets[0]!.message({ type: "ready", connection_id: "window-viewer" });
  assert.equal(f.sockets[0]!.url.searchParams.get("frame_window"), "6");
  assert.equal(f.sockets[0]!.sent.length, 0);
  const decodes: ((bitmap: { width: number; height: number; close(): void }) => void)[] = [];
  let closed = 0;
  f.setDecode(() => new Promise(resolve => { decodes.push(resolve); }));
  for (let i = 0; i < 6; i++) f.sockets[0]!.message(frame(640 + i));
  assert.equal(f.session.state.connected, false);
  assert.equal(decodes.length, 1, "decode one image at a time");
  assert.equal(f.sockets[0]!.sent.length, 0, "coalescing must not grant new credits");
  decodes[0]!({ width: 640, height: 360, close() { closed++; } }); await flush();
  assert.equal(decodes.length, 2);
  assert.equal(f.drawn.length, 0, "a decoded image superseded during decode must never paint");
  assert.equal(f.session.state.mediaReady, false);
  assert.equal(f.sockets[0]!.sent.length, 0, "skipping a stale bitmap must not grant new credits");
  assert.equal(Buffer.from(await f.decoded[1]!.arrayBuffer()).toString("base64"), frame(645).jpeg);
  decodes[1]!({ width: 645, height: 360, close() { closed++; } }); await flush();
  assert.equal(closed, 2, "both stale and presented bitmaps must close exactly once");
  assert.deepEqual(f.drawn.map(bitmap => bitmap.width), [645]);
  assert.deepEqual([f.canvas.width, f.canvas.height], [645, 360]);
  assert.deepEqual(f.sockets[0]!.sent, [{ type: "frame_request", count: 6 }]);
  assert.equal(f.session.state.connected, true);
  f.session.suspend();
  await f.tick(1000);
  assert.equal(f.sockets[0]!.sent.length, 1);
});

for (const frame_window of [2, 3, 6]) {
  test(`window ${frame_window} stops arrivals from starving presentation and grants credits only when current`, async t => {
    const f = fixture(t, { ...frameHand, frame_window });
    await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer" });
    const decodes: ((bitmap: { width: number; height: number; close(): void }) => void)[] = [];
    f.setDecode(() => new Promise(resolve => { decodes.push(resolve); }));
    f.sockets[0]!.message(frame(640));
    let closed = 0;
    for (let i = 0; i < frame_window - 1; i++) {
      f.sockets[0]!.message(frame(641 + i));
      decodes[i]!({ width: 640 + i, height: 360, close() { closed++; } }); await flush();
      assert.equal(f.drawn.length, 0);
      assert.equal(f.sockets[0]!.sent.length, 0, "supersession cannot replenish an unbounded stream");
    }
    decodes.at(-1)!({ width: 639 + frame_window, height: 360, close() { closed++; } }); await flush();
    assert.equal(closed, frame_window);
    assert.deepEqual(f.drawn.map(bitmap => bitmap.width), [639 + frame_window]);
    assert.deepEqual(f.sockets[0]!.sent, [{ type: "frame_request", count: frame_window }]);
  });
}

test("a partial JPEG window returns only consumed credits and preserves unused credits across batches", async t => {
  const f = fixture(t, { ...frameHand, frame_window: 6 });
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer" });
  const decodes: ((bitmap: { width: number; height: number; close(): void }) => void)[] = [];
  f.setDecode(() => new Promise(resolve => { decodes.push(resolve); }));
  for (let i = 0; i < 3; i++) f.sockets[0]!.message(frame());
  decodes[0]!({ width: 640, height: 360, close() {} }); await flush();
  decodes[1]!({ width: 640, height: 360, close() {} }); await flush();
  assert.deepEqual(f.sockets[0]!.sent, [{ type: "frame_request", count: 3 }]);
  for (let i = 0; i < 6; i++) { f.sockets[0]!.message(frame()); await flush(); }
  assert.equal(f.sockets[0]!.readyState, 1, "three unused plus three returned credits admit six images");
  assert.equal(decodes.length, 3);
  f.sockets[0]!.message(frame());
  assert.equal(f.sockets[0]!.readyState, 3, "coalesced frames still consume credits and the seventh is rejected");
  decodes[2]!({ width: 640, height: 360, close() {} }); await flush();
  assert.equal(f.drawn.length, 1);
  assert.equal(f.sockets[0]!.sent.length, 1, "teardown never returns retired credits again");
});

test("coalesced JPEGs do not extend a stalled decoder's deadline", async t => {
  const f = fixture(t, { ...frameHand, frame_window: 6 });
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer" });
  const pending = deferred<{ width: number; height: number; close(): void }>();
  f.setDecode(() => pending.promise);
  f.sockets[0]!.message(frame());
  await f.tick(9000);
  for (let i = 0; i < 5; i++) f.sockets[0]!.message(frame());
  await f.tick(1000);
  assert.equal(f.sockets[0]!.readyState, 3);
  assert.equal(f.session.state.connecting, true);
  let closed = 0;
  pending.resolve({ width: 640, height: 360, close() { closed++; } }); await flush();
  assert.equal(closed, 1); assert.equal(f.decoded.length, 1); assert.equal(f.drawn.length, 0);
  assert.equal(f.sockets[0]!.sent.length, 0);
});

test("a frame received synchronously during readiness notification retains its own credit until painted", async t => {
  const f = fixture(t, { ...frameHand, frame_window: 6 });
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer" });
  const next = deferred<{ width: number; height: number; close(): void }>();
  f.setChanged(state => {
    if (!state.mediaReady) return;
    f.setChanged(() => {}); f.setDecode(() => next.promise);
    f.sockets[0]!.message(frame(641));
  });
  f.sockets[0]!.message(frame()); await flush();
  assert.deepEqual(f.drawn.map(bitmap => bitmap.width), [640]);
  assert.deepEqual(f.sockets[0]!.sent, [{ type: "frame_request", count: 1 }]);
  next.resolve({ width: 641, height: 360, close() {} }); await flush();
  assert.deepEqual(f.drawn.map(bitmap => bitmap.width), [640, 641]);
  assert.deepEqual(f.sockets[0]!.sent, [{ type: "frame_request", count: 1 }, { type: "frame_request", count: 1 }]);
});

test("windowed frames reject an unsolicited seventh image while decoding is blocked", async t => {
  const f = fixture(t, { ...frameHand, frame_window: 6 });
  await f.session.connect();
  f.sockets[0]!.message({ type: "ready", connection_id: "window-viewer" }); await flush();
  f.setDecode(() => new Promise(() => {}));
  for (let i = 0; i < 7; i++) f.sockets[0]!.message(frame());
  await flush();
  assert.equal(f.session.state.connected, false);
  assert.equal(f.session.state.connecting, false);
  assert.match(f.session.state.status, /Invalid remote signal/);
});

test("a background viewer retains its connection across a long tab switch while renewing", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.playVideo();
  f.session.takeControl(); f.peers[0]!.reliable.message({ type: "granted", generation: "lease" });
  f.session.releaseControl();
  assert.equal(f.session.state.controlling, false);
  assert.equal(f.peers[0]!.reliable.sent.at(-1).type, "release");
  for (let i = 0; i < 6; i++) {
    await f.tick(10_000);
    f.sockets[0]!.message({ type: "renewed" }); await flush();
  }
  f.session.resume(); await flush();
  assert.equal(f.session.state.connected, true);
  assert.equal(f.session.state.controlling, false);
  assert.equal(f.peers.length, 1);
  assert.equal(f.sockets.length, 1);
  assert.equal(f.catalogReads, 0);
});

test("foregrounding an expired background viewer cannot renew its authorization", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.playVideo();
  f.session.releaseControl();
  await f.tick(25_000);
  assert.equal(f.session.state.connected, false);
  assert.equal(f.session.state.status, "This remote session is no longer authorized.");
  f.session.resume(); await flush();
  assert.equal(f.session.state.connecting, false);
  assert.equal(f.sockets.length, 1);
  assert.equal(f.catalogReads, 0);
});

test("a brief background switch releases control and resumes the existing connection", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.playVideo();
  f.session.takeControl(); f.peers[0]!.reliable.message({ type: "granted", generation: "lease" });
  f.session.suspend(15_000);
  assert.equal(f.session.state.controlling, false);
  assert.equal(f.peers[0]!.reliable.sent.at(-1).type, "release");
  await f.tick(500); f.session.resume(); await f.tick(15_000);
  assert.equal(f.session.state.connected, true);
  assert.equal(f.peers.length, 1);
  assert.equal(f.sockets.length, 1);
});

test("long background pauses still detach and resume with a fresh authorized publication", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open();
  f.session.suspend(15_000); await f.tick(15_000);
  assert.equal(f.session.state.status, "Paused");
  assert.equal(f.sockets[0]!.readyState, 3);
  f.session.resume(); await flush();
  assert.equal(f.sockets.length, 2);
  assert.equal(f.catalogReads, 1);
});

test("temporary WebRTC disconnects recover without replacing the peer or granting input", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open();
  const peer = f.peers[0]!;
  peer.connectionState = "disconnected"; peer.onconnectionstatechange!();
  f.session.takeControl(); assert.equal(peer.reliable.sent.length, 0);
  await f.tick(2000);
  peer.connectionState = "connected"; peer.onconnectionstatechange!();
  await f.tick(2000);
  assert.equal(f.session.state.connected, true);
  assert.equal(f.peers.length, 1);
  assert.equal(f.sockets.length, 1);
});

test("a sustained WebRTC disconnect still replaces the peer after a bounded grace", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open();
  f.peers[0]!.connectionState = "disconnected"; f.peers[0]!.onconnectionstatechange!();
  await f.tick(3000);
  assert.equal(f.peers[0]!.connectionState, "closed");
  await f.tick(1000);
  assert.equal(f.peers.length, 2);
});

test("a transient renewal failure retries inside the original lease without disconnecting", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.playVideo();
  f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
  f.setStatus(503); await f.tick(10_000);
  assert.equal(f.session.state.connected, true);
  f.setStatus(200); await f.tick(500);
  assert.equal(f.requests.filter(r => r.path.endsWith("/renew")).length, 2);
  f.sockets[0]!.message({ type: "renewed" }); await flush();
  await f.tick(15_000);
  assert.equal(f.session.state.connected, true);
  assert.equal(f.sockets.length, 1);
});

test("renewal retries never extend authorization without a fresh authenticated renewal", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.playVideo();
  f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
  f.setStatus(503); await f.tick(10_000); await f.tick(500); await f.tick(14_500);
  assert.equal(f.session.state.connected, false);
  assert.equal(f.session.state.connecting, false);
  assert.equal(f.session.state.status, "This remote session is no longer authorized.");
});

for (const status of [401, 403, 409]) {
  test(`renewal HTTP ${status} fails immediately instead of retrying a missing or revoked lease`, async t => {
    const f = fixture(t);
    await f.session.connect(); f.peers[0]!.open();
    f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
    f.setStatus(status); await f.tick(10_000);
    assert.equal(f.sockets[0]!.readyState, 3);
    assert.equal(f.session.state.connected, false);
  });
}


test("audio and video tracks share a stream in either arrival order", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const audio = { kind: "audio", stop() {} }, video = { kind: "video", stop() {} };
  f.peers[0]!.ontrack!({ track: audio });
  const stream = f.video.srcObject as MediaStream;
  f.peers[0]!.ontrack!({ track: video });
  assert.equal(f.video.srcObject, stream);
  assert.equal(stream.getTracks().length, 2);
  assert.equal(f.session.state.audioAvailable, true);
  await f.session.setAudioEnabled(true);
  assert.equal(f.video.muted, false);
  assert.equal(f.session.state.audioEnabled, true);
  await f.session.setAudioEnabled(false);
  assert.equal(f.video.muted, true);
  f.video.srcObject = null;
  f.peers[0]!.ontrack!({ track: video });
  f.peers[0]!.ontrack!({ track: audio });
  assert.equal((f.video.srcObject as MediaStream).getTracks().length, 2);
  f.session.close(); assert.equal(f.session.state.audioAvailable, false);
});

for (const order of [["video", "audio"], ["audio", "video"]]) {
  test(`WebRTC ${order.join(" then ")} attaches once and updates tracks without reloading playback`, async t => {
    const f = fixture(t); await f.session.connect();
    let source: unknown = null, assignments = 0;
    Object.defineProperty(f.video, "srcObject", {
      get() { return source; }, set(value: unknown) { source = value; assignments++; },
    });
    const tracks = order.map(kind => ({ kind, stops: 0, stop() { this.stops++; } }));
    for (const track of tracks) f.peers[0]!.ontrack!({ track });
    const stream = f.video.srcObject as MediaStream;
    assert.equal(assignments, 1, "adding audio or video must not reset srcObject");
    for (const track of tracks) f.peers[0]!.ontrack!({ track });
    assert.equal(assignments, 1);
    assert.deepEqual(tracks.map(track => track.stops), [0, 0], "duplicate track delivery must not stop active tracks");
    assert.deepEqual(stream.getTracks(), tracks);
    const replacement = { kind: order[0], stops: 0, stop() { this.stops++; } };
    f.peers[0]!.ontrack!({ track: replacement });
    assert.equal(assignments, 1);
    assert.equal(tracks[0]!.stops, 1);
    assert.deepEqual(stream.getTracks(), [tracks[1], replacement]);
    assert.equal(f.session.state.audioAvailable, true);
    f.session.suspend();
    assert.equal(f.video.srcObject, null); assert.equal(assignments, 2);
    f.session.resume(); await flush();
    f.peers[1]!.ontrack!({ track: { kind: "video", stop() {} } });
    assert.notEqual(f.video.srcObject, stream, "a new peer needs a fresh playback stream");
  });
}

test("blocked sound falls back to muted video without reconnecting", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  let attempts = 0;
  f.video.play = async () => { attempts++; if (!f.video.muted) throw new Error("autoplay blocked"); };
  await f.session.setAudioEnabled(true);
  assert.equal(attempts, 2);
  assert.equal(f.video.muted, true);
  assert.equal(f.session.state.audioEnabled, false);
  assert.equal(f.session.state.connected, true);
  assert.equal(f.sockets.length, 1);
});

test("broadcast validation keeps endpoint secrets out of state and refuses credentials", async t => {
  const f = fixture(t, { ...screen, broadcast: true });
  await f.session.connect(); f.peers[0]!.open();
  for (const url of ["rtmp://:@host/key", "rtmp://host/key#", "rtmp://host/", "rtmp://host", "https://host/key", "rtmp://user:pass@host/key", "rtmp://host/key#fragment", "rtmp://host/a b", "rtmp://host/" + "é".repeat(2048)]) {
    assert.equal(f.session.broadcast("start", url), false);
  }
  assert.equal(f.sockets[0]!.sent.length, 0);
  assert.equal(f.session.broadcast("start", "rtmps://host/app/secret", "twitch"), true);
  assert.equal(JSON.stringify(f.session.state).includes("secret"), false);
  assert.equal(f.session.broadcast("start", "rtmps://host/app/replacement", "x"), false);
  assert.equal(f.sockets[0]!.sent.length, 1);
  const request = f.sockets[0]!.sent[0];
  f.sockets[0]!.message({ type: "broadcast_result", request_id: "stale", status: "failed" });
  assert.equal(f.session.state.broadcastPending, true);
  f.sockets[0]!.message({ type: "broadcast_result", request_id: request.request_id, status: "live", audio: true }); await flush();
  assert.equal(f.session.state.broadcastPending, false);
  assert.equal(f.session.state.broadcastStatus, "live");
  assert.equal(f.session.state.broadcastAudio, true);
  assert.equal(f.session.broadcast("start", "rtmps://host/app/another"), false);
  f.session.close();
  assert.equal(f.sockets[0]!.sent.some(m => m.action === "stop"), false);
});

test("broadcast polling times out, recovers status and never replays start on reconnect", async t => {
  const f = fixture(t, { ...screen, broadcast: true });
  await f.session.connect(); f.peers[0]!.open(); f.playVideo();
  f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
  const initial = f.sockets[0]!.sent.find(m => m.action === "status");
  assert.ok(initial);
  await f.tick(5000);
  assert.equal(f.sockets[0]!.sent.filter(m => m.action === "status").length, 1);
  f.sockets[0]!.message({ type: "broadcast_result", request_id: initial.request_id, status: "idle" }); await flush();
  assert.equal(f.session.broadcast("start", "rtmps://host/app/private", "x"), true);
  await f.tick(10000);
  assert.equal(f.session.state.broadcastPending, false);
  await f.tick(5000);
  const status = f.sockets[0]!.sent.filter(m => m.action === "status").at(-1);
  f.sockets[0]!.message({ type: "broadcast_result", request_id: status.request_id, status: "reconnecting", audio: false, error: "rtmps://secret" }); await flush();
  assert.equal(f.session.state.broadcastStatus, "reconnecting");
  assert.equal(f.session.state.broadcastAudio, false);
  assert.equal(f.session.state.broadcastError?.includes("secret"), false);
  f.sockets[0]!.close(); await f.tick(2000);
  f.sockets.at(-1)!.message({ type: "ready", connection_id: "new-viewer" }); await flush();
  assert.equal(f.sockets.at(-1)!.sent.some(m => m.action === "start"), false);
  assert.equal(f.sockets.at(-1)!.sent.some(m => m.action === "status"), true);
});


test("stream start UI waits for status and disables active, pending and disconnected states", () => {
  const base = { connected: true, controlling: false, connecting: false, status: "Connected" };
  assert.equal(canStartBroadcast(base), false);
  for (const broadcastStatus of ["starting", "live", "reconnecting"] as const) {
    assert.equal(canStartBroadcast({ ...base, broadcastStatus }), false);
  }
  for (const broadcastStatus of ["idle", "failed", "stopped"] as const) {
    assert.equal(canStartBroadcast({ ...base, broadcastStatus }), true);
    assert.equal(canStartBroadcast({ ...base, broadcastStatus, broadcastPending: true }), false);
    assert.equal(canStartBroadcast({ ...base, broadcastStatus, connected: false }), false);
  }
});


test("authenticated renewal bypasses a pending ICE restart without extending a stale session", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open(); f.playVideo();
  f.sockets[0]!.message({ type: "ready", connection_id: "viewer" });
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "initial" } }); await flush();
  // An earlier renewal was delayed; the next one arrives during ICE refresh.
  await f.tick(20_000);
  const ice = deferred<Response>(); f.setIceResponse(() => ice.promise);
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "restart" } }); await flush();
  f.sockets[0]!.message({ type: "renewed" }); await flush();
  await f.tick(5_000);
  assert.equal(f.session.state.connected, true);
  assert.equal(f.sockets.length, 1);
  ice.resolve(Response.json({ iceServers: [] })); await flush();
  assert.equal(f.sockets[0]!.sent.filter(message => message.type === "signal").length, 2);
  const staleMessage = f.sockets[0]!.onmessage!;
  f.session.suspend();
  staleMessage({ data: JSON.stringify({ type: "renewed" }) });
  await f.tick(25_000);
  assert.equal(f.session.state.status, "Paused");
  assert.equal(f.sockets.length, 1);
});


test("a reconnect that needs twelve seconds retains its peer and completes", async t => {
  const f = fixture(t);
  await f.session.connect(); f.peers[0]!.open(); f.peers[0]!.fail();
  await f.tick(1000);
  const peer = f.peers[1]!;
  f.sockets[1]!.message({ type: "ready", connection_id: "replacement" }); await flush();
  await f.tick(12_000);
  assert.equal(peer.connectionState, "new");
  assert.equal(f.session.state.connecting, true);
  peer.open();
  assert.equal(f.session.state.connected, true);
  assert.equal(f.peers.length, 2);
  assert.equal(f.sockets.length, 2);
});

for (const frame_window of [1, 6]) {
  test(`frames-v1 window ${frame_window}: grants and revocations bypass a pending bitmap decode`, async t => {
    const f = fixture(t, { ...frameHand, frame_window });
    await f.session.connect();
    f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
    f.sockets[0]!.message(frame()); await flush(); await f.tick(34);
    const pending = deferred<{ width: number; height: number; close(): void }>();
    f.setDecode(() => pending.promise);
    f.sockets[0]!.message(frame()); await flush();
    assert.equal(f.decoded.length, 2);
    f.session.takeControl();
    f.sockets[0]!.message({ type: "control", data: { type: "granted", generation: "lease" } });
    assert.equal(f.session.state.controlling, true, "grant cannot wait for bitmap completion or a message queue");
    f.sockets[0]!.message({ type: "control", data: { type: "revoked", generation: "lease" } });
    assert.equal(f.session.state.controlling, false, "revocation cannot wait for bitmap completion");
    const sent = f.sockets[0]!.sent.length;
    f.session.input({ kind: "text", text: "after revoke" });
    assert.equal(f.sockets[0]!.sent.length, sent);
    assert.equal(f.drawn.length, 1);
    let closed = 0;
    pending.resolve({ width: 640, height: 360, close() { closed++; } }); await flush();
    assert.equal(f.drawn.length, 2); assert.equal(closed, 1);
    assert.equal(f.session.state.controlling, false);
  });
}

for (const reason of ["malformed", "duplicate-ready", "overrun", "close"] as const) {
  test(`frames-v1 ${reason} discards queued images and late control while bitmap decode is pending`, async t => {
    const f = fixture(t, { ...frameHand, frame_window: 6 });
    await f.session.connect();
    f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
    f.sockets[0]!.message(frame()); await flush(); f.session.takeControl();
    const pending = deferred<{ width: number; height: number; close(): void }>();
    f.setDecode(() => pending.promise);
    f.sockets[0]!.message(frame()); await flush();
    f.sockets[0]!.message(frame()); await flush();
    const late = f.sockets[0]!.onmessage!;
    if (reason === "malformed") f.sockets[0]!.message({ ...frame(), jpeg: "bad" });
    else if (reason === "duplicate-ready") f.sockets[0]!.message({ type: "ready", connection_id: "duplicate" });
    else if (reason === "overrun") for (let i = 0; i < 5; i++) f.sockets[0]!.message(frame());
    else f.session.close();
    late({ data: JSON.stringify({ type: "control", data: { type: "granted", generation: "late" } }) });
    await flush();
    assert.equal(f.sockets[0]!.readyState, 3);
    assert.equal(f.session.state.connected, false); assert.equal(f.session.state.controlling, false);
    assert.equal(f.session.state.connecting, false);
    const sent = f.sockets[0]!.sent.length;
    let closed = 0;
    pending.resolve({ width: 640, height: 360, close() { closed++; } }); await flush();
    assert.equal(closed, 1); assert.equal(f.decoded.length, 2); assert.equal(f.drawn.length, 1);
    assert.equal(f.session.state.connected, false); assert.equal(f.session.state.controlling, false);
    assert.deepEqual([f.canvas.width, f.canvas.height], [0, 0]);
    assert.equal(f.sockets[0]!.sent.length, sent);
  });
}

test("windowed frames require ready before admitting images to the decoder", async t => {
  const f = fixture(t, { ...frameHand, frame_window: 6 });
  await f.session.connect();
  f.sockets[0]!.message(frame());
  f.sockets[0]!.message({ type: "ready", connection_id: "late" }); await flush();
  assert.equal(f.decoded.length, 0); assert.equal(f.sockets[0]!.readyState, 3);
  assert.equal(f.session.state.connected, false); assert.equal(f.session.state.connecting, false);
});

test("retired decoder completion cannot drain or change a reconnect's pending frame queue", async t => {
  const f = fixture(t, { ...frameHand, frame_window: 6 });
  await f.session.connect();
  f.sockets[0]!.message({ type: "ready", connection_id: "old" }); await flush();
  const old = deferred<{ width: number; height: number; close(): void }>();
  f.setDecode(() => old.promise);
  f.sockets[0]!.message(frame()); f.sockets[0]!.message(frame()); await flush();
  const late = f.sockets[0]!.onmessage!;
  f.session.suspend(); f.session.resume(); await flush();
  f.sockets[1]!.message({ type: "ready", connection_id: "new" }); await flush();
  const fresh = deferred<{ width: number; height: number; close(): void }>();
  f.setDecode(() => fresh.promise);
  f.sockets[1]!.message(frame(641)); await flush();
  let closed = 0;
  old.resolve({ width: 640, height: 360, close() { closed++; } });
  late({ data: JSON.stringify({ type: "control", data: { type: "granted", generation: "stale" } }) });
  await flush();
  assert.equal(closed, 1); assert.equal(f.drawn.length, 0); assert.equal(f.decoded.length, 2);
  assert.equal(f.session.state.connected, false); assert.equal(f.session.state.controlling, false);
  assert.deepEqual(f.sockets[1]!.sent, []);
  f.sockets[1]!.message(frame(642)); await flush();
  assert.equal(f.decoded.length, 2, "old completion cannot clear the new decoder's busy flag");
  f.setDecode(async () => ({ width: 642, height: 360, close() {} }));
  fresh.resolve({ width: 641, height: 360, close() {} }); await flush();
  assert.deepEqual(f.drawn.map(bitmap => bitmap.width), [642]);
  assert.equal(f.session.state.connected, true);
  assert.deepEqual(f.sockets[1]!.sent, [{ type: "frame_request", count: 2 }]);
});

async function microphoneFixture(t: TestContext, direction = "sendrecv", capability: unknown = true) {
  const f = fixture(t);
  await f.session.connect();
  const peer = f.peers[0]!;
  const attached: unknown[] = [];
  let replace = async (_track: unknown) => {};
  const transceiver = {
    receiver: { track: { kind: "audio" } }, stopped: false,
    direction: "recvonly", currentDirection: direction,
    sender: { async replaceTrack(track: unknown) { attached.push(track); await replace(track); } },
  };
  peer.transceivers.push(transceiver);
  f.sockets[0]!.message({ type: "signal", signal: { type: "offer", sdp: "audio-offer" } }); await flush();
  peer.open(); f.session.takeControl();
  peer.reliable.message({ type: "granted", generation: "microphone-lease", ...(capability === "legacy" ? {} : { microphone: capability }) });
  const track = {
    kind: "audio", enabled: true, readyState: "live", onended: null as (() => void) | null, stops: 0,
    stop() { this.readyState = "ended"; this.stops++; },
  };
  const stream = new MediaStream(); stream.addTrack(track as unknown as MediaStreamTrack);
  f.setCapture(async () => stream);
  return {
    ...f, peer, transceiver, attached, track, stream,
    setReplace(value: typeof replace) { replace = value; },
    request() { f.session.setMicrophoneEnabled(true); return peer.reliable.sent.at(-1); },
    ack(request = peer.reliable.sent.at(-1), enabled = true) { peer.reliable.message({ ...request, enabled }); },
  };
}

test("microphone reserves a return sender without capture, then requires explicit opt-in and matching ACK", async t => {
  const f = await microphoneFixture(t);
  assert.equal(f.transceiver.direction, "sendrecv");
  assert.equal(f.session.state.microphoneAvailable, true);
  assert.equal(f.captures.length, 0);
  assert.deepEqual(f.attached, []);
  const request = f.request();
  assert.equal(request.type, "microphone"); assert.equal(request.generation, "microphone-lease");
  assert.equal(request.enabled, true); assert.match(request.requestID, /^[a-f0-9-]{36}$/);
  assert.equal(f.session.state.microphonePending, true); assert.equal(f.captures.length, 0);
  f.ack({ ...request, generation: "old" }); f.ack({ ...request, requestID: "old" });
  assert.equal(f.captures.length, 0);
  f.ack(request); f.ack(request); await flush();
  assert.deepEqual(f.captures, [{ audio: true, video: false }]);
  assert.deepEqual(f.attached, [f.track]); assert.equal(f.track.enabled, true);
  assert.equal(f.session.state.microphoneEnabled, true); assert.equal(f.session.state.microphonePending, false);
  f.session.setMicrophoneEnabled(true); f.ack(request); await flush();
  assert.equal(f.captures.length, 1, "repeated enable and duplicate ACK cannot reopen capture");
});

test("microphone requires host capability, browser capture support and a negotiated return direction", async t => {
  for (const direction of ["recvonly", "inactive"]) {
    await t.test(direction, async t => {
      const f = await microphoneFixture(t, direction);
      assert.equal(f.session.state.microphoneAvailable, false);
      f.request(); assert.equal(f.captures.length, 0);
      assert.equal(f.peer.reliable.sent.filter(value => value.type === "microphone").length, 0);
    });
  }
  for (const capability of [false, "legacy"]) {
    await t.test(`capability ${capability}`, async t => {
      const f = await microphoneFixture(t, "sendrecv", capability);
      assert.equal(f.session.state.microphoneAvailable, false); f.request(); assert.equal(f.captures.length, 0);
    });
  }
  await t.test("browser capture unavailable", async t => {
    const f = await microphoneFixture(t);
    Object.defineProperty(navigator, "mediaDevices", { value: undefined });
    f.session.setMicrophoneEnabled(true);
    assert.equal(f.peer.reliable.sent.filter(value => value.type === "microphone").length, 0);
  });
});

test("microphone ACK timeout disables the host request and ignores its late ACK", async t => {
  const f = await microphoneFixture(t), request = f.request();
  await f.tick(5000);
  assert.equal(f.session.state.microphonePending, false);
  assert.match(f.session.state.microphoneError!, /did not respond/);
  assert.equal(f.peer.reliable.sent.at(-1).enabled, false);
  f.ack(request); await flush(); assert.equal(f.captures.length, 0);
  const retry = f.request(); assert.notEqual(retry.requestID, request.requestID);
  f.ack(request); assert.equal(f.captures.length, 0);
  f.ack(retry); await flush(); assert.equal(f.session.state.microphoneEnabled, true);
});

for (const phase of ["ACK", "permission", "sender", "active"] as const) {
  for (const action of ["mute", "release", "revoke", "disconnect", "background", "close"] as const) {
    test(`microphone ${phase}: ${action} stops capture and ignores stale completion`, async t => {
      const f = await microphoneFixture(t);
      const permission = deferred<MediaStream>(), replacement = deferred<void>();
      if (phase === "permission") f.setCapture(() => permission.promise);
      if (phase === "sender") f.setReplace(track => track ? replacement.promise : Promise.resolve());
      const request = f.request();
      if (phase !== "ACK") { f.ack(request); await flush(); }
      if (action === "mute") f.session.setMicrophoneEnabled(false);
      else if (action === "release") f.session.releaseControl();
      else if (action === "revoke") f.peer.reliable.message({ type: "revoked", generation: "microphone-lease" });
      else if (action === "disconnect") { f.peer.connectionState = "disconnected"; f.peer.onconnectionstatechange?.(); }
      else if (action === "background") f.session.suspend(60_000);
      else f.session.close();
      assert.equal(f.session.state.microphoneEnabled, false); assert.equal(f.session.state.microphonePending, false);
      if (phase === "sender" || phase === "active") {
        assert.equal(f.track.readyState, "ended", "capture must stop synchronously, before sender cleanup finishes");
        assert.equal(f.track.enabled, false);
      }
      permission.resolve(f.stream); replacement.resolve(); f.ack(request); await flush();
      assert.equal(f.session.state.microphoneEnabled, false);
      if (phase === "ACK") assert.equal(f.captures.length, 0);
      else assert.equal(f.track.readyState, "ended");
      if (phase === "permission") assert.equal(f.attached.includes(f.track), false);
      if (phase === "sender" || phase === "active") assert.equal(f.attached.at(-1), null);
    });
  }
}

test("microphone permission and sender errors stop tracks, disable host input and permit explicit retry", async t => {
  for (const failure of ["permission", "sender", "no track"]) {
    await t.test(failure, async t => {
      const f = await microphoneFixture(t);
      if (failure === "permission") f.setCapture(async () => { throw new Error("permission denied"); });
      if (failure === "sender") f.setReplace(async () => { throw new Error("cannot send"); });
      if (failure === "no track") f.setCapture(async () => new MediaStream());
      f.ack(f.request()); await flush();
      assert.equal(f.session.state.microphoneEnabled, false); assert.equal(f.session.state.microphonePending, false);
      assert.match(f.session.state.microphoneError!, /access is unavailable/);
      assert.equal(f.peer.reliable.sent.at(-1).enabled, false);
      if (failure === "sender") assert.equal(f.track.readyState, "ended");
      const nextTrack = { ...f.track, readyState: "live", stops: 0 };
      const nextStream = new MediaStream(); nextStream.addTrack(nextTrack as unknown as MediaStreamTrack);
      f.setCapture(async () => nextStream); f.setReplace(async () => {});
      f.ack(f.request()); await flush();
      assert.equal(f.session.state.microphoneEnabled, true); assert.equal(f.session.state.microphoneError, undefined);
    });
  }
});

test("microphone pending permission times out and its eventual stream is stopped", async t => {
  const f = await microphoneFixture(t), permission = deferred<MediaStream>();
  f.playVideo();
  f.setCapture(() => permission.promise); f.ack(f.request());
  await f.tick(20_000); f.sockets[0]!.message({ type: "renewed" }); await flush();
  await f.tick(10_000);
  assert.match(f.session.state.microphoneError!, /timed out/);
  permission.resolve(f.stream); await flush();
  assert.equal(f.track.readyState, "ended"); assert.equal(f.attached.includes(f.track), false);
});

test("host microphone rejection and later receiver failure preserve speaker playback", async t => {
  const f = await microphoneFixture(t);
  const speaker = { kind: "audio", stop() { throw new Error("speaker must remain independent"); } };
  f.peer.ontrack?.({ track: speaker }); await f.session.setAudioEnabled(true);
  const playback = f.video.srcObject;
  const denied = f.request(); f.ack(denied, false); await flush();
  assert.equal(f.captures.length, 0); assert.match(f.session.state.microphoneError!, /unavailable/);
  const accepted = f.request(); f.ack(accepted); await flush();
  f.peer.reliable.message({ type: "revoked", generation: "stale-lease" });
  assert.equal(f.session.state.microphoneEnabled, true);
  f.ack(accepted, false); await flush();
  assert.equal(f.track.readyState, "ended"); assert.equal(f.session.state.microphoneEnabled, false);
  assert.equal(f.session.state.audioEnabled, true); assert.equal(f.video.muted, false);
  assert.equal(f.video.srcObject, playback); assert.equal(f.session.state.audioAvailable, true);
});

test("microphone device ending disables the host while keeping the control lease", async t => {
  const f = await microphoneFixture(t); f.ack(f.request()); await flush();
  f.track.readyState = "ended"; f.track.onended?.(); await flush();
  assert.equal(f.session.state.microphoneEnabled, false); assert.equal(f.session.state.controlling, true);
  assert.match(f.session.state.microphoneError!, /device changed/);
  assert.equal(f.peer.reliable.sent.at(-1).enabled, false);
});

test("matching malformed microphone ACK tears down while stale malformed ACK is ignored", async t => {
  const f = await microphoneFixture(t), request = f.request();
  f.peer.reliable.message({ ...request, requestID: "old", enabled: "yes" });
  assert.equal(f.session.state.connected, true);
  f.peer.reliable.message({ ...request, enabled: "yes" });
  assert.equal(f.session.state.connected, false); assert.equal(f.captures.length, 0);
});

test("microphone sender replacement is serialized across mute and explicit re-enable", async t => {
  const f = await microphoneFixture(t), replacement = deferred<void>();
  f.setReplace(track => track === f.track ? replacement.promise : Promise.resolve());
  const first = f.request(); f.ack(first); await flush();
  f.session.setMicrophoneEnabled(false);
  const newTrack = { ...f.track, readyState: "live", stops: 0 };
  const newStream = new MediaStream(); newStream.addTrack(newTrack as unknown as MediaStreamTrack);
  f.setCapture(async () => newStream);
  const next = f.request(); f.ack(first); f.ack(next); await flush();
  assert.equal(f.session.state.microphoneEnabled, false);
  replacement.resolve(); await flush();
  assert.deepEqual(f.attached, [f.track, null, newTrack]);
  assert.equal(f.track.readyState, "ended"); assert.equal(newTrack.enabled, true);
  assert.equal(f.session.state.microphoneEnabled, true);
});

test("microphone never resumes on reconnect, and a new peer is independent of old sender work", async t => {
  const f = await microphoneFixture(t), oldReplacement = deferred<void>();
  f.setReplace(track => track ? oldReplacement.promise : Promise.resolve());
  const oldRequest = f.request(); f.ack(oldRequest); await flush();
  f.session.reconnect(); await flush();
  const nextPeer = f.peers[1]!;
  const attached: unknown[] = [];
  nextPeer.transceivers.push({ ...f.transceiver, sender: { async replaceTrack(track: unknown) { attached.push(track); } } });
  f.sockets[1]!.message({ type: "signal", signal: { type: "offer", sdp: "reconnected" } }); await flush();
  nextPeer.open(); f.session.takeControl();
  nextPeer.reliable.message({ type: "granted", generation: "new-lease", microphone: true });
  nextPeer.reliable.message(oldRequest); await flush();
  assert.equal(f.session.state.microphoneAvailable, true);
  assert.equal(f.session.state.microphoneEnabled, false); assert.equal(f.captures.length, 1);
  const nextTrack = { ...f.track, readyState: "live", stops: 0 };
  const nextStream = new MediaStream(); nextStream.addTrack(nextTrack as unknown as MediaStreamTrack);
  f.setCapture(async () => nextStream);
  f.session.setMicrophoneEnabled(true); nextPeer.reliable.message(nextPeer.reliable.sent.at(-1)); await flush();
  assert.equal(f.session.state.microphoneEnabled, true); assert.deepEqual(attached, [nextTrack]);
  oldReplacement.resolve(); await flush();
  assert.deepEqual(attached, [nextTrack]); assert.equal(nextTrack.enabled, true); assert.equal(f.track.readyState, "ended");
});

test("frames-v1 never advertises or captures a microphone even with a microphone grant", async t => {
  const f = fixture(t, frameHand);
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "frames-viewer" }); await flush();
  f.sockets[0]!.message(frame()); await flush(); f.session.takeControl();
  f.sockets[0]!.message({ type: "control", data: { type: "granted", generation: "frames-mic", microphone: true } }); await flush();
  assert.equal(f.session.state.microphoneAvailable, false);
  f.session.setMicrophoneEnabled(true);
  assert.equal(f.captures.length, 0);
});

test("malformed microphone capability is rejected before capture", async t => {
  const f = await microphoneFixture(t, "sendrecv", "yes");
  assert.equal(f.session.state.connected, false); assert.equal(f.captures.length, 0);
});

test("interactive receiver hints preserve playback across supported, legacy and rejecting browsers", async t => {
  const f = fixture(t);
  await f.session.connect();
  const peer = f.peers[0]!;
  for (const kind of ["video", "audio"]) {
    for (const receiver of [{ jitterBufferTarget: null }, { playoutDelayHint: 0.4 }, {}]) {
      peer.ontrack!({ track: { kind, stop() {} }, receiver });
      if ("jitterBufferTarget" in receiver) assert.equal(receiver.jitterBufferTarget, 0);
      if ("playoutDelayHint" in receiver) assert.equal(receiver.playoutDelayHint, 0);
    }
    for (const receiver of [
      { set jitterBufferTarget(_value: number) { throw new Error("unsupported"); } },
      { set playoutDelayHint(_value: number) { throw new Error("unsupported"); } },
    ]) {
      const track = { kind, stop() {} };
      peer.ontrack!({ track, receiver });
      assert.ok((f.video.srcObject as MediaStream).getTracks().includes(track as MediaStreamTrack));
    }
    const receiver = { jitterBufferTarget: null, playoutDelayHint: 0.4 };
    peer.ontrack!({ track: { kind, stop() {} }, receiver });
    assert.equal(receiver.jitterBufferTarget, 0);
    assert.equal(receiver.playoutDelayHint, 0.4);
  }
});

test("congested absolute motion retains only the final position and sends when the channel drains", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const { reliable, motion } = f.peers[0]!;
  f.session.takeControl(); reliable.message({ type: "granted", generation: "pointer" });
  motion.bufferedAmount = 5000;
  for (let i = 0; i < 1000; i++) f.session.input({ kind: "move", x: i / 1000, y: .5 });
  await f.tick(1000);
  assert.equal(motion.sent.length, 0);
  assert.equal(motion.bufferedAmountLowThreshold, 1024);
  motion.bufferedAmount = 0; motion.onbufferedamountlow?.(); motion.onbufferedamountlow?.();
  assert.deepEqual(motion.sent, [{ kind: "move", x: .999, y: .5, sequence: 1, generation: "pointer" }]);
  assert.equal(f.session.state.controlling, true);
});

for (const kind of ["button", "key", "scroll", "relativeMove", "releaseAll"] as const) {
  test(`congested pointer sample cannot replay across a ${kind} boundary`, async t => {
    const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
    const { reliable, motion } = f.peers[0]!;
    f.session.takeControl(); reliable.message({ type: "granted", generation: "pointer", relativePointer: true });
    motion.bufferedAmount = 5000;
    f.session.input({ kind: "move", x: .1, y: .2 });
    const event = kind === "button" ? { kind, down: false, button: 0, x: .8, y: .9 }
      : kind === "key" ? { kind, down: false, key: 4 }
        : kind === "scroll" || kind === "relativeMove" ? { kind, deltaX: 2, deltaY: 3 } : { kind };
    f.session.input(event);
    motion.bufferedAmount = 0; motion.onbufferedamountlow?.();
    assert.equal(motion.sent.length, 0);
    assert.deepEqual(reliable.sent.at(-1), { ...event, sequence: 1, generation: "pointer" });
    f.session.input({ kind: "move", x: .9, y: .9 });
    assert.deepEqual(motion.sent.at(-1), { kind: "move", x: .9, y: .9, sequence: 2, generation: "pointer" });
  });
}

test("release and reconnect discard congestion callbacks and motion from the retired lease", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const { reliable, motion } = f.peers[0]!;
  f.session.takeControl(); reliable.message({ type: "granted", generation: "first" });
  motion.bufferedAmount = 5000; f.session.input({ kind: "move", x: .2, y: .2 });
  f.session.releaseControl(); reliable.message({ type: "revoked", generation: "first" });
  f.session.takeControl(); reliable.message({ type: "granted", generation: "second" });
  motion.bufferedAmount = 0; motion.onbufferedamountlow?.();
  assert.equal(motion.sent.length, 0);
  motion.bufferedAmount = 5000; f.session.input({ kind: "move", x: .3, y: .3 });
  const staleDrain = motion.onbufferedamountlow!;
  f.session.reconnect(); await flush(); f.peers[1]!.open();
  const next = f.peers[1]!; f.session.takeControl(); next.reliable.message({ type: "granted", generation: "third" });
  staleDrain(); assert.equal(next.motion.sent.length, 0); assert.equal(motion.sent.length, 0);
});

test("frames-v1 retries one final absolute position and clears its retry on release", async t => {
  const f = fixture(t, frameHand);
  await f.session.connect(); f.sockets[0]!.message({ type: "ready", connection_id: "viewer" }); await flush();
  f.sockets[0]!.message(frame()); await flush();
  f.session.takeControl(); f.sockets[0]!.message({ type: "control", data: { type: "granted", generation: "frame-pointer" } }); await flush();
  const socket = f.sockets[0]!;
  socket.bufferedAmount = 5000;
  f.session.input({ kind: "move", x: .1, y: .2 }); f.session.input({ kind: "move", x: .8, y: .9 });
  socket.bufferedAmount = 0; await f.tick(16);
  assert.deepEqual(socket.sent.filter(value => value.type === "input").map(value => value.data), [
    { kind: "move", x: .8, y: .9, generation: "frame-pointer", sequence: 1 },
  ]);
  socket.bufferedAmount = 5000; f.session.input({ kind: "move", x: .3, y: .3 });
  f.session.releaseControl(); socket.bufferedAmount = 0; await f.tick(16);
  assert.equal(socket.sent.filter(value => value.type === "input").length, 1);
});

test("stats polling is opt-in, never overlaps, and disabling ignores an in-flight sample", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const peer = f.peers[0]!, pending = deferred<RTCStatsReport>();
  await f.tick(1000); assert.equal(peer.statsCalls, 0);
  peer.stats = () => pending.promise;
  f.session.setStatsEnabled(true); assert.equal(peer.statsCalls, 1);
  await f.tick(3000); assert.equal(peer.statsCalls, 1, "a slow browser must not accumulate getStats work");
  f.session.setStatsEnabled(false);
  pending.resolve(new Map([['video', { id: 'video', type: 'inbound-rtp', kind: 'video', timestamp: 1000, framesDecoded: 60 }]]) as RTCStatsReport);
  await flush(); await f.tick(1000);
  assert.equal(peer.statsCalls, 1); assert.equal(f.session.state.stats, undefined);
});

test("stats update from interval reports, recover from rejection and stop after close", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const peer = f.peers[0]!;
  let framesDecoded = 100, timestamp = 1000;
  peer.stats = async () => new Map([['video', { id: 'video', type: 'inbound-rtp', kind: 'video', timestamp, framesDecoded }]]) as RTCStatsReport;
  f.session.setStatsEnabled(true); await flush(); assert.equal(f.session.state.stats?.decodeFps, undefined);
  framesDecoded = 140; timestamp = 3000; await f.tick(1000);
  assert.equal(f.session.state.stats?.decodeFps, 20);
  peer.stats = async () => { throw new Error("unavailable"); }; await f.tick(1000);
  assert.equal(f.session.state.stats?.decodeFps, undefined); assert.equal(f.session.state.connected, true);
  f.session.close(); const calls = peer.statsCalls; await f.tick(1000);
  assert.equal(peer.statsCalls, calls); assert.equal(f.session.state.stats, undefined);
});

test("startup diagnostics retain concurrent milestones with Stats closed and never sample signaling RTT", async t => {
  const f = fixture(t), ice = deferred<Response>(); f.setIceResponse(() => ice.promise);
  const connecting = f.session.connect(), socket = f.sockets[0]!;
  await f.tick(10); socket.onopen?.();
  await f.tick(10); socket.message({ type: "signal", signal: { type: "offer", sdp: "private-offer" } });
  await f.tick(60); ice.resolve(Response.json({ iceServers: [] })); await connecting; await flush();
  const peer = f.peers[0]!;
  await f.tick(20); peer.open();
  peer.ontrack?.({ track: { id: "picture", kind: "video", stop() {} } });
  await f.tick(60); f.frames.values().next().value!();
  assert.equal(peer.statsCalls, 0, "recording startup does not start diagnostics polling");
  assert.equal(Boolean(f.session.state.stats), false);
  f.session.setStatsEnabled(true); await flush();
  assert.deepEqual(f.session.state.stats?.startup, { socketOpenMs: 10, offerReceivedMs: 20,
    iceReadyMs: 80, answerSentMs: 80, controlsReadyMs: 100, peerConnectedMs: 100 });
  assert.equal(f.session.state.stats?.attempt, 1);
  assert.equal(f.session.state.stats?.icePolicy, "all");
  assert.equal(f.session.state.stats?.firstFrameMs, 160);
  assert.equal(f.session.state.stats?.totalFirstFrameMs, 160);
  assert.equal(f.session.state.stats?.roundTripMs, undefined, "socket establishment time is never media RTT");
  assert.equal(JSON.stringify(f.session.state).includes("private-offer"), false);
  await f.tick(50); socket.onopen?.(); peer.onconnectionstatechange?.();
  assert.equal(f.session.state.stats?.startup?.socketOpenMs, 10, "milestones preserve first occurrence");
  assert.equal(f.session.state.stats?.startup?.peerConnectedMs, 100);
});

test("reconnecting from a startup notification cannot mark the replacement ready", async t => {
  const f = fixture(t); await f.session.connect(); f.session.setStatsEnabled(true); await flush();
  let restarted = false;
  f.setChanged(state => {
    if (!restarted && state.stats?.startup?.controlsReadyMs !== undefined) {
      restarted = true; f.session.reconnect();
    }
  });
  f.peers[0]!.open(); await flush();
  assert.equal(restarted, true); assert.equal(f.peers.length, 2);
  assert.equal(f.session.state.connected, false);
  assert.equal(f.session.state.stats?.startup?.controlsReadyMs, undefined);
});

test("startup diagnostics separate failed-attempt time from time to frame including recovery", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const retiredSocketOpen = f.sockets[0]!.onopen!;
  await f.tick(100); f.peers[0]!.fail(); await f.tick(1000);
  const next = f.peers[1]!; next.open();
  next.ontrack?.({ track: { id: "replacement", kind: "video", stop() {} } });
  await f.tick(100); f.frames.values().next().value!();
  f.session.setStatsEnabled(true); await flush();
  assert.equal(f.session.state.stats?.attempt, 2);
  assert.equal(f.session.state.stats?.firstFrameMs, 100);
  assert.equal(f.session.state.stats?.totalFirstFrameMs, 1200);
  assert.equal(f.session.state.stats?.startup?.catalogReadyMs, 0);
  retiredSocketOpen();
  assert.equal(f.session.state.stats?.startup?.socketOpenMs, undefined, "retired callbacks cannot stamp the replacement");
  f.session.reconnect(); await flush();
  assert.equal(f.session.state.stats?.attempt, 1, "explicit reconnect starts a new user wait");
  assert.equal(f.session.state.stats?.firstFrameMs, undefined);
  assert.equal(f.session.state.stats?.totalFirstFrameMs, undefined);
});

test("first frame is measured on presentation, retained until stats open, and reset on reconnect", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const first = f.peers[0]!;
  first.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  const presented = [...f.frames.values()][0]!;
  assert.equal(Boolean(f.session.state.stats), false);
  await f.tick(175); presented(); assert.equal(f.frames.size, 1, "keep watching after the first frame");
  f.session.setStatsEnabled(true); await flush();
  assert.equal(f.session.state.stats?.firstFrameMs, 175);
  const pending = deferred<RTCStatsReport>(); first.stats = () => pending.promise; await f.tick(1000);
  f.session.reconnect(); await flush(); const second = f.peers[1]!; second.open();
  second.ontrack?.({ track: { kind: "video", id: "next-picture", stop() {} } });
  pending.resolve(new Map() as RTCStatsReport); await flush(); presented();
  assert.equal(f.session.state.stats?.firstFrameMs, undefined, "retired callbacks cannot mark the new peer ready");
  await f.tick(75); [...f.frames.values()][0]!();
  assert.equal(f.session.state.stats?.firstFrameMs, 75);
});

test("older browsers measure first decoded readiness and clean up their event listener", async t => {
  const f = fixture(t);
  Reflect.deleteProperty(f.video, "requestVideoFrameCallback"); Reflect.deleteProperty(f.video, "cancelVideoFrameCallback");
  await f.session.connect(); f.peers[0]!.open();
  f.session.setStatsEnabled(true); await flush();
  f.peers[0]!.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  await f.tick(120);
  f.video.readyState = 2; f.video.videoWidth = 640; f.video.videoHeight = 360;
  f.video.dispatchEvent(new Event("loadeddata"));
  assert.equal(f.session.state.stats?.firstFrameMs, 120);
  await f.tick(10); f.video.dispatchEvent(new Event("loadeddata"));
  assert.equal(f.session.state.stats?.firstFrameMs, 120);
  f.session.close(); f.video.dispatchEvent(new Event("loadeddata")); assert.equal(f.session.state.stats, undefined);
});

test("older browsers detect video added to an already loaded audio stream without reattaching", async t => {
  const f = fixture(t);
  Reflect.deleteProperty(f.video, "requestVideoFrameCallback"); Reflect.deleteProperty(f.video, "cancelVideoFrameCallback");
  await f.session.connect(); f.session.setStatsEnabled(true);
  const peer = f.peers[0]!;
  peer.ontrack!({ track: { kind: "audio", stop() {} } });
  f.video.readyState = 2; f.video.dispatchEvent(new Event("loadeddata"));
  const stream = f.video.srcObject;
  peer.ontrack!({ track: { kind: "video", stop() {} } });
  assert.equal(f.video.srcObject, stream); assert.equal(f.session.state.mediaReady, false);
  await f.tick(120); f.video.videoWidth = 640; f.video.videoHeight = 360;
  f.video.dispatchEvent(new Event("resize"));
  assert.equal(f.session.state.mediaReady, true); assert.equal(f.session.state.stats?.firstFrameMs, 120);
  await f.tick(30); f.video.dispatchEvent(new Event("resize"));
  assert.equal(f.session.state.stats?.firstFrameMs, 120);
  f.session.close(); f.video.dispatchEvent(new Event("resize"));
  assert.equal(f.session.state.mediaReady, false); assert.equal(f.session.state.stats, undefined);
});

test("a matching revocation discards pending motion but a stale revocation preserves the current lease", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const { reliable, motion } = f.peers[0]!;
  f.session.takeControl(); reliable.message({ type: "granted", generation: "current" });
  motion.bufferedAmount = 5000; f.session.input({ kind: "move", x: .2, y: .3 });
  reliable.message({ type: "revoked", generation: "old" });
  motion.bufferedAmount = 0; motion.onbufferedamountlow?.();
  assert.equal(motion.sent.length, 1); assert.equal(f.session.state.controlling, true);
  motion.bufferedAmount = 5000; f.session.input({ kind: "move", x: .7, y: .8 });
  reliable.message({ type: "revoked", generation: "current" });
  motion.bufferedAmount = 0; motion.onbufferedamountlow?.();
  assert.equal(motion.sent.length, 1); assert.equal(f.session.state.controlling, false);
});

test("reliable backpressure still tears down control instead of silently dropping a release", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const { reliable, motion } = f.peers[0]!;
  f.session.takeControl(); reliable.message({ type: "granted", generation: "slow" });
  motion.bufferedAmount = 5000; f.session.input({ kind: "move", x: .2, y: .3 });
  reliable.bufferedAmount = 32769;
  f.session.input({ kind: "button", button: 0, down: false, x: .4, y: .5 });
  assert.equal(f.session.state.controlling, false); assert.equal(f.session.state.connected, false);
  motion.bufferedAmount = 0; motion.onbufferedamountlow?.(); assert.equal(motion.sent.length, 0);
  assert.equal(reliable.readyState, "closed");
});

test("rapid stats toggles share one in-flight request and leave one polling loop", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.open();
  const peer = f.peers[0]!, pending = deferred<RTCStatsReport>(); peer.stats = () => pending.promise;
  f.session.setStatsEnabled(true); f.session.setStatsEnabled(true);
  f.session.setStatsEnabled(false); f.session.setStatsEnabled(true);
  f.session.setStatsEnabled(false); f.session.setStatsEnabled(true);
  assert.equal(peer.statsCalls, 1);
  pending.resolve(new Map() as RTCStatsReport); await flush();
  await f.tick(1000); assert.equal(peer.statsCalls, 2);
  await f.tick(1000); assert.equal(peer.statsCalls, 3);
  f.session.close(); f.session.setStatsEnabled(true); assert.equal(f.session.state.stats, undefined);
});

test("video becomes ready before input channels without enabling premature control", async t => {
  const f = fixture(t); await f.session.connect(); const peer = f.peers[0]!;
  peer.connectionState = "connected";
  peer.ontrack?.({ track: { kind: "video", id: "early-picture", stop() {} } });
  await f.tick(120); [...f.frames.values()][0]!();
  assert.equal(f.session.state.mediaReady, true);
  assert.equal(f.session.state.connected, false); assert.equal(f.session.state.connecting, true);
  assert.equal(f.session.state.status, "Watching · connecting controls…");
  f.session.takeControl(); assert.equal(peer.reliable.sent.length, 0);
  peer.open(); assert.equal(f.session.state.connected, true); assert.equal(f.session.state.status, "Watching");
  f.session.takeControl(); assert.deepEqual(peer.reliable.sent.at(-1), { type: "acquire" });
  f.session.close(); assert.equal(f.session.state.mediaReady, false);
});


const turnServers = [{ urls: ["stun:ice.example", "turns:ice.example:443?transport=tcp"], username: "viewer", credential: "ephemeral" }];
function videoReport(framesDecoded: number, trackIdentifier = "picture"): RTCStatsReport {
  return new Map([["video", { id: "video", type: "inbound-rtp", kind: "video", trackIdentifier, framesDecoded }]]) as RTCStatsReport;
}

test("a connected direct path with no video retries via TURN with Stats off and releases control", async t => {
  const f = fixture(t);
  f.setIceResponse(async () => Response.json({ iceServers: turnServers }));
  await f.session.connect(); const first = f.peers[0]!; first.open();
  assert.equal(first.config.iceTransportPolicy, "all");
  first.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  const staleFrame = [...f.frames.values()][0]!;
  // Metadata, an active audio clock and received bytes do not prove decoding.
  f.video.readyState = 4; f.video.videoWidth = 3840; f.video.videoHeight = 2160;
  first.stats = async () => new Map([
    ["video", { id: "video", type: "inbound-rtp", kind: "video", framesDecoded: 0, bytesReceived: Date.now() }],
    ["audio", { id: "audio", type: "inbound-rtp", kind: "audio", framesDecoded: Date.now() }],
  ]) as RTCStatsReport;
  f.session.takeControl(); first.reliable.message({ type: "granted", generation: "direct-lease" });
  for (let i = 0; i < 14; i++) { Object.assign(f.video, { currentTime: i }); await f.tick(1000); }
  assert.equal(f.session.state.connected, true); assert.equal(f.session.state.mediaReady, false);
  await f.tick(1000);
  assert.equal(first.connectionState, "closed"); assert.equal(f.session.state.controlling, false);
  assert.deepEqual(first.reliable.sent.at(-1), { type: "release", generation: "direct-lease" });
  await f.tick(1000); const second = f.peers[1]!; second.open();
  assert.equal(second.config.iceTransportPolicy, "relay"); assert.deepEqual(second.reliable.sent, []);
  staleFrame(); assert.equal(f.session.state.mediaReady, false);
  assert.equal(f.session.state.stats, undefined); assert.ok(first.statsCalls > 0);
});

for (const iceServers of [[], [{ urls: "stun:ice.example" }], [{ urls: "turn:ice.example" }], [{ urls: "turn:ice.example", username: "viewer", credential: "" }]]) {
  test(`missing TURN credentials leave retry candidates available: ${JSON.stringify(iceServers)}`, async t => {
    const f = fixture(t); f.setIceResponse(async () => Response.json({ iceServers }));
    await f.session.connect(); f.peers[0]!.fail(); await f.tick(1000);
    const retry = f.peers[1]!; assert.equal(retry.config.iceTransportPolicy, "all");
    retry.open(); assert.equal(f.session.state.connected, true);
    f.setIceResponse(async () => Response.json({ iceServers: turnServers }));
    retry.fail(); await f.tick(2000);
    assert.equal(f.peers[2]!.config.iceTransportPolicy, "relay", "fresh TURN availability uses the retained preference");
  });
}

test("relay preference survives healthy media, reconnect, resume and refreshed ICE credentials", async t => {
  const f = fixture(t); f.setIceResponse(async () => Response.json({ iceServers: turnServers }));
  await f.session.connect(); f.peers[0]!.fail(); await f.tick(1000);
  const relay = f.peers[1]!; relay.open(); f.playVideo();
  for (let i = 0; i < 12; i++) await f.tick(1000);
  f.session.reconnect(); await flush(); const next = f.peers[2]!; next.open();
  assert.equal(next.config.iceTransportPolicy, "relay");
  f.sockets[2]!.message({ type: "signal", signal: { type: "offer", sdp: "initial" } }); await flush();
  f.setIceResponse(async () => Response.json({ iceServers: [{ urls: "stun:ice.example" }] }));
  f.sockets[2]!.message({ type: "signal", signal: { type: "offer", sdp: "restart-no-turn" } }); await flush();
  assert.equal(next.config.iceTransportPolicy, "all", "expired/missing TURN must not strand a restart");
  f.setIceResponse(async () => Response.json({ iceServers: turnServers }));
  f.sockets[2]!.message({ type: "signal", signal: { type: "offer", sdp: "restart-with-turn" } }); await flush();
  assert.equal(next.config.iceTransportPolicy, "relay");
  f.session.suspend(); f.session.resume(); await flush();
  assert.equal(f.peers[3]!.config.iceTransportPolicy, "relay");
});

for (const singleFrame of [false, true]) {
  test(`transport readiness${singleFrame ? " and a single frame" : " without media"} cannot extend the 90-second recovery deadline`, async t => {
    const f = fixture(t); f.setIceResponse(async () => Response.json({ iceServers: turnServers }));
    await f.session.connect(); f.peers[0]!.open(); f.peers[0]!.fail();
    for (let i = 0; i < 90; i++) {
      await f.tick(1000);
      const peer = f.peers.at(-1)!;
      if (peer.connectionState === "new") {
        peer.open();
        if (singleFrame) {
          peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
          [...f.frames.values()][0]!();
        }
      }
      f.sockets.at(-1)!.message({ type: "renewed" });
    }
    assert.ok(f.peers.length > 3);
    assert.equal(f.session.state.connected, false); assert.equal(f.session.state.connecting, false);
    const attempts = f.peers.length; await f.tick(30_000); assert.equal(f.peers.length, attempts);
  });
}

test("sustained real video earns a new recovery budget and resets backoff without resetting relay", async t => {
  const f = fixture(t); f.setIceResponse(async () => Response.json({ iceServers: turnServers }));
  await f.session.connect(); f.peers[0]!.fail(); f.setCatalog([]);
  for (let i = 0; i < 50; i++) await f.tick(1000);
  f.setCatalog([screen]);
  while (f.peers.length === 1) await f.tick(1000);
  const relay = f.peers[1]!; relay.open(); f.playVideo();
  for (let i = 0; i < 12; i++) await f.tick(1000);
  relay.fail(); await f.tick(999); assert.equal(f.peers.length, 2);
  await f.tick(1); assert.equal(f.peers.length, 3, "healthy media resets exponential backoff");
  f.peers[2]!.open(); f.setCatalog([]);
  // The old budget expired at 90s; this later outage still has its own budget.
  for (let i = 0; i < 35; i++) await f.tick(1000);
  assert.equal(f.session.state.connecting, true);
  assert.equal(f.peers[2]!.config.iceTransportPolicy, "relay");
  for (let i = 0; i < 55; i++) await f.tick(1000);
  assert.equal(f.session.state.connecting, false);
});

test("stale presented video recovers even while the control and authorization leases stay healthy", async t => {
  const f = fixture(t); await f.session.connect(); const peer = f.peers[0]!; peer.open();
  peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  for (let i = 0; i < 12; i++) { [...f.frames.values()][0]!(); await f.tick(1000); }
  [...f.frames.values()][0]!(); f.sockets[0]!.message({ type: "renewed" });
  await f.tick(9000); assert.equal(f.session.state.connected, true);
  await f.tick(1000); assert.equal(f.session.state.connected, false);
  assert.equal(f.session.state.mediaReady, false); assert.equal(f.session.state.connecting, true);
});

test("decoded frames keep background video healthy without presentation callbacks or the Stats UI", async t => {
  const f = fixture(t); await f.session.connect(); const peer = f.peers[0]!; peer.open();
  peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  let decoded = 0; peer.stats = async () => videoReport(decoded);
  for (let i = 0; i < 35; i++) {
    decoded += 60; await f.tick(1000); f.sockets[0]!.message({ type: "renewed" });
  }
  assert.equal(f.peers.length, 1); assert.equal(f.session.state.connected, true);
  assert.equal(f.session.state.mediaReady, true); assert.equal(f.session.state.stats, undefined);
  for (let i = 0; i < 11; i++) { await f.tick(1000); f.sockets.at(-1)!.message({ type: "renewed" }); }
  assert.equal(peer.connectionState, "closed", "an unchanged decoded count must eventually expire");
});

for (const retire of ["suspend", "close", "reconnect"] as const) {
  test(`a pending health sample cannot escape ${retire}, and slow stats cannot disable the media deadline`, async t => {
    const f = fixture(t); await f.session.connect(); const peer = f.peers[0]!; peer.open();
    peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
    const pending = deferred<RTCStatsReport>(); peer.stats = () => pending.promise;
    await f.tick(3000); assert.equal(peer.statsCalls, 1);
    f.session.setStatsEnabled(true); f.session.setStatsEnabled(false);
    await f.tick(10_000); assert.equal(peer.statsCalls, 1, "health and diagnostics share the slow request");
    if (retire === "reconnect") {
      await f.tick(2000); assert.equal(peer.connectionState, "closed", "the unresolved request cannot hold the watchdog");
      await f.tick(1000); f.peers[1]!.open();
    } else f.session[retire]();
    pending.resolve(videoReport(600)); await flush();
    assert.equal(f.session.state.mediaReady, false); assert.equal(f.session.state.stats, undefined);
    if (retire !== "reconnect") {
      await f.tick(30_000); assert.equal(f.peers.length, 1);
      assert.equal(f.session.state.status, retire === "close" ? "Disconnected" : "Paused");
    }
  });
}

test("a replaced video track ignores late callbacks and stats for its predecessor", async t => {
  const f = fixture(t); await f.session.connect(); const peer = f.peers[0]!; peer.open();
  peer.ontrack?.({ track: { kind: "video", id: "old", stop() {} } });
  const callback = [...f.frames.values()][0]!, pending = deferred<RTCStatsReport>();
  peer.stats = () => pending.promise; await f.tick(3000);
  peer.ontrack?.({ track: { kind: "video", id: "new", stop() {} } });
  callback(); pending.resolve(videoReport(90, "old")); await flush();
  assert.equal(f.session.state.mediaReady, false);
  peer.stats = async () => videoReport(180, "old"); await f.tick(3000);
  assert.equal(f.session.state.mediaReady, false);
  peer.stats = async () => videoReport(60, "new"); await f.tick(1000);
  assert.equal(f.session.state.mediaReady, true);
});

for (const source of ["presentation", "counter", "decoder"] as const) {
  test(`closing synchronously from ${source} readiness retires the media watchdog`, async t => {
    const f = fixture(t); await f.session.connect(); const peer = f.peers[0]!; peer.open();
    peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
    f.setChanged(state => { if (state.mediaReady) f.session.close(); });
    if (source === "presentation") [...f.frames.values()][0]!();
    else if (source === "counter") {
      Object.assign(f.video, { getVideoPlaybackQuality: () => ({ totalVideoFrames: 60, droppedVideoFrames: 0 }) });
      await f.tick(1000);
    } else { peer.stats = async () => videoReport(60); await f.tick(3000); }
    assert.equal(f.session.state.status, "Disconnected"); assert.equal(f.session.state.mediaReady, false);
    assert.equal(f.frames.size, 0); const calls = peer.statsCalls;
    await f.tick(100_000); assert.equal(f.peers.length, 1); assert.equal(peer.statsCalls, calls);
  });
}

test("historical decoder totals cannot turn a short burst of pictures into stable recovery", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.fail();
  const deadline = Date.now() + 90_000;
  await f.tick(1000); const peer = f.peers[1]!; peer.open();
  peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  peer.stats = async () => videoReport(9);
  for (let i = 0; i < 9; i++) { [...f.frames.values()][0]!(); await f.tick(1000); }
  await f.tick(2000); assert.equal(peer.statsCalls, 1);
  peer.fail(); f.setCatalog([]);
  while (Date.now() < deadline) await f.tick(Math.min(1000, deadline - Date.now()));
  assert.equal(f.session.state.connecting, false, "the cumulative baseline must not erase the original budget");
});

for (const [quietPolls, delay] of [[0, 11_000], [2, 8000]] as const) {
  test(`a background timer delayed ${delay}ms checks decoder health with bounded grace for stuck stats`, async t => {
    const f = fixture(t); await f.session.connect(); const peer = f.peers[0]!; peer.open();
    peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
    let decoded = 0; peer.stats = async () => videoReport(decoded);
    for (let i = 0; i < 6; i++) { decoded += 60; await f.tick(1000); }
    for (let i = 0; i < quietPolls; i++) await f.tick(1000);
    f.sockets[0]!.message({ type: "renewed" });
    decoded += 660; await f.tick(delay);
    assert.equal(f.session.state.connected, true); assert.equal(f.peers.length, 1);
    f.sockets[0]!.message({ type: "renewed" });
    peer.stats = () => new Promise(() => {});
    await f.tick(11_000); assert.equal(f.session.state.connected, true, "allow a delayed decoder check to settle");
    await f.tick(1000); assert.equal(f.session.state.connected, true);
    await f.tick(1000); assert.equal(peer.connectionState, "closed", "a stuck sample gets only two seconds of grace");
  });
}

test("a late video heartbeat cannot clear a recovery deadline that already expired", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.fail(); f.setCatalog([]);
  for (let i = 0; i < 76; i++) await f.tick(1000);
  f.setCatalog([screen]);
  while (f.peers.length === 1) await f.tick(1000);
  f.peers[1]!.open(); f.playVideo();
  for (let i = 0; i < 9; i++) await f.tick(1000);
  assert.equal(f.session.state.connected, true);
  await f.tick(3000);
  assert.equal(f.session.state.connected, false); assert.equal(f.session.state.connecting, false);
  assert.equal(f.peers[1]!.connectionState, "closed");
});

test("returning to decoder sampling after local frames requires a fresh baseline", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.fail();
  const deadline = Date.now() + 90_000;
  await f.tick(1000); const peer = f.peers[1]!; peer.open();
  peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  let decoded = 1; peer.stats = async () => videoReport(decoded);
  await f.tick(3000);
  for (let i = 0; i < 8; i++) { decoded++; await f.tick(1000); [...f.frames.values()][0]!(); }
  for (let i = 0; i < 3; i++) await f.tick(1000);
  assert.equal(peer.statsCalls, 2);
  peer.fail(); f.setCatalog([]);
  while (Date.now() < deadline) await f.tick(Math.min(1000, deadline - Date.now()));
  assert.equal(f.session.state.connecting, false, "a stale pre-presentation baseline cannot earn stability");
});


test("a late switch from presentation to decoder health can baseline and then measure a new frame", async t => {
  const f = fixture(t); await f.session.connect(); const peer = f.peers[0]!; peer.open();
  peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  [...f.frames.values()][0]!();
  let decoded = 660; peer.stats = async () => videoReport(decoded);
  await f.tick(11_000); assert.equal(f.session.state.connected, true);
  decoded += 60; await f.tick(1000);
  assert.equal(peer.connectionState, "connected", "baseline plus fresh delta fits within the bounded grace");
  for (let i = 0; i < 3; i++) { decoded += 60; await f.tick(1000); }
  assert.equal(f.peers.length, 1);
});

test("a decoder report captured before local frames cannot seed their later fallback baseline", async t => {
  const f = fixture(t); await f.session.connect(); f.peers[0]!.fail();
  const deadline = Date.now() + 90_000;
  await f.tick(1000); const peer = f.peers[1]!; peer.open();
  peer.ontrack?.({ track: { kind: "video", id: "picture", stop() {} } });
  const pending = deferred<RTCStatsReport>(); peer.stats = () => pending.promise;
  await f.tick(3000);
  for (let i = 0; i < 9; i++) { await f.tick(1000); [...f.frames.values()][0]!(); }
  await f.tick(2000); pending.resolve(videoReport(0)); await flush();
  peer.stats = async () => videoReport(9); await f.tick(1000);
  peer.fail(); f.setCatalog([]);
  while (Date.now() < deadline) await f.tick(Math.min(1000, deadline - Date.now()));
  assert.equal(f.session.state.connecting, false, "the old pending sample cannot credit already-presented frames");
});
