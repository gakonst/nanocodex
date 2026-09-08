export type RemoteHand = Readonly<{
  id: string; name: string; kind: "desktop" | "window" | "phone" | "vm";
  width: number; height: number; controllable: boolean;
  machine_id: string; machine_name: string; generation: string;
  transport?: "webrtc" | "frames-v1";
}>;
export type RemoteState = Readonly<{ status: string; connected: boolean; controlling: boolean; connecting: boolean }>;
export type RemoteInput = {
  kind: "move" | "button" | "scroll" | "key" | "text" | "releaseAll";
  x?: number; y?: number; button?: number; down?: boolean; key?: number; text?: string; deltaX?: number; deltaY?: number;
};

const encoder = new TextEncoder();
class RemoteError extends Error {
  readonly terminal: boolean;
  constructor(message: string, terminal = false) { super(message); this.terminal = terminal; }
}
async function request(path: string, method = "GET", body?: unknown, signal?: AbortSignal): Promise<any> {
  const response = await fetch("/v1/account/hands" + path, {
    method, credentials: "same-origin", cache: "no-store", redirect: "error",
    signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(10_000)]) : AbortSignal.timeout(10_000),
    ...(body === undefined ? {} : { body: JSON.stringify(body), headers: { "content-type": "application/json" } }),
  });
  if (!response.ok) {
    const unauthorized = [401, 403].includes(response.status);
    throw new RemoteError(unauthorized ? "This remote session is no longer authorized." : "This screen is unavailable.", unauthorized);
  }
  return response.json();
}
export async function listRemoteHands(signal?: AbortSignal): Promise<readonly RemoteHand[]> {
  const value = await request("/screens", "GET", undefined, signal);
  const string = (value: unknown) => typeof value === "string" && value.length > 0 && value.length <= 512;
  if (!value || !Array.isArray(value.surfaces) || value.surfaces.length > 512 || !value.surfaces.every((hand: RemoteHand) => hand
    && [hand.id, hand.name, hand.machine_id, hand.machine_name, hand.generation].every(string)
    && ["desktop", "window", "phone", "vm"].includes(hand.kind) && typeof hand.controllable === "boolean"
    && (hand.transport === undefined || ["webrtc", "frames-v1"].includes(hand.transport))
    && Number.isInteger(hand.width) && hand.width > 0 && Number.isInteger(hand.height) && hand.height > 0)) {
    throw new RemoteError("Invalid screen catalog.", true);
  }
  return value.surfaces;
}

function frameBytes(value: { jpeg?: unknown; width?: unknown; height?: unknown }) {
  const { jpeg, width, height } = value;
  const invalid = () => new RemoteError("Invalid remote frame.", true);
  if (typeof jpeg !== "string" || jpeg.length > 700_000 || jpeg.length % 4 !== 0 || !/^\/9j\/[A-Za-z0-9+/]*={0,2}$/.test(jpeg)
    || !Number.isInteger(width) || !Number.isInteger(height) || Number(width) < 1 || Number(height) < 1 || Number(width) > 1280 || Number(height) > 1280) throw invalid();
  const binary = atob(jpeg), bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  // Check the JPEG's own SOF dimensions before asking the browser to allocate
  // decoded pixels; a small advertised size cannot conceal a huge image.
  let offset = 2;
  while (offset < bytes.length) {
    if (bytes[offset++] !== 0xff) throw invalid();
    while (bytes[offset] === 0xff) offset++;
    const marker = bytes[offset++];
    if (marker === undefined || marker === 0xda || marker === 0xd9) break;
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd8)) continue;
    if (offset + 2 > bytes.length) throw invalid();
    const length = bytes[offset]! * 256 + bytes[offset + 1]!;
    if (length < 2 || offset + length > bytes.length) throw invalid();
    if ([0xc0, 0xc1, 0xc2].includes(marker)) {
      if (length < 8 || bytes[offset + 3]! * 256 + bytes[offset + 4]! !== height
        || bytes[offset + 5]! * 256 + bytes[offset + 6]! !== width) throw invalid();
      return bytes;
    }
    offset += length;
  }
  throw invalid();
}

/** WebRTC by default; explicitly advertised frames-v1 hosts use the leased socket. */
export class RemoteBrowserSession {
  state: RemoteState = { status: "Connecting…", connected: false, controlling: false, connecting: true };
  hand: RemoteHand;
  private readonly video: HTMLVideoElement;
  private readonly canvas?: HTMLCanvasElement;
  private readonly changed: (state: RemoteState) => void;
  private peer?: RTCPeerConnection;
  private socket?: WebSocket;
  private reliable?: RTCDataChannel;
  private motion?: RTCDataChannel;
  private sequence = 0;
  // Serialize acquire/release exchanges: legacy hosts acknowledge release with
  // an unversioned revoked message, which must not cancel a later explicit take.
  private control: "idle" | "acquiring" | "cancelled-acquire" | { kind: "held" | "releasing"; generation: string } = "idle";
  private get generation(): string | undefined { return typeof this.control === "object" && this.control.kind === "held" ? this.control.generation : undefined; }
  private controlRequested = false;
  private abort = new AbortController();
  private watchdog?: ReturnType<typeof setTimeout>;
  private connectingTimer?: ReturnType<typeof setTimeout>;
  private retryTimer?: ReturnType<typeof setTimeout>;
  private renewTimer?: ReturnType<typeof setInterval>;
  private controlTimer?: ReturnType<typeof setInterval>;
  private frameTimer?: ReturnType<typeof setTimeout>;
  private frameDeadline?: ReturnType<typeof setTimeout>;
  private framePending = false;
  private frameDecoding = false;
  private frameRequestedAt = 0;
  private epoch = 0;
  private retries = 0;
  private recoveryDeadline?: number;
  private suspended = false;
  private closed = false;
  constructor(hand: RemoteHand, video: HTMLVideoElement, changed: (state: RemoteState) => void, canvas?: HTMLCanvasElement) {
    this.hand = hand; this.video = video; this.changed = changed; this.canvas = canvas;
  }

  async connect(): Promise<void> { await this.start(false); }
  reconnect(): void {
    if (this.closed) return;
    this.suspended = false; this.retries = 0; this.recoveryDeadline = undefined;
    void this.start(true);
  }
  suspend(): void {
    if (this.closed || this.suspended) return;
    this.suspended = true; this.detach();
    this.update({ status: "Paused", connected: false, controlling: false, connecting: false });
  }
  resume(): void { if (this.suspended && !this.closed) this.reconnect(); }

  private current(epoch: number): boolean { return epoch === this.epoch && !this.closed && !this.suspended; }
  private async start(refresh: boolean): Promise<void> {
    if (this.closed || this.suspended) return;
    this.detach();
    const epoch = this.epoch;
    this.abort = new AbortController();
    const signal = this.abort.signal;
    this.update({ status: refresh ? "Reconnecting…" : "Connecting…", connected: false, controlling: false, connecting: true });
    const remaining = this.recoveryDeadline === undefined ? Infinity : this.recoveryDeadline - performance.now();
    this.connectingTimer = setTimeout(() => {
      if (this.current(epoch)) this.fail(new RemoteError("Could not establish a screen connection."));
    }, Math.max(0, Math.min(refresh ? 10_000 : 25_000, remaining)));
    try {
      if (refresh) {
        const hands = await listRemoteHands(signal);
        if (!this.current(epoch)) return;
        const hand = hands.find(hand => hand.machine_id === this.hand.machine_id && hand.id === this.hand.id);
        if (!hand) throw new RemoteError("This screen is unavailable.");
        this.hand = hand;
      }
      const frames = this.hand.transport === "frames-v1";
      let peer: RTCPeerConnection | undefined;
      if (frames) {
        if (!this.canvas?.getContext("2d")) throw new RemoteError("This browser cannot display this screen.", true);
      }
      // Fetch TURN credentials while the authenticated viewer socket connects.
      // Offers stay queued until this attempt's credentials and peer are ready.
      const peerReady = frames ? Promise.resolve() : request("/ice", "POST", undefined, signal).then(ice => {
        if (!this.current(epoch)) return;
        peer = new RTCPeerConnection({ iceServers: ice.iceServers, bundlePolicy: "max-bundle" });
        this.peer = peer;
        const connectedPeer = peer;
        peer.onicecandidate = ({ candidate }) => {
          if (this.current(epoch) && candidate) this.signal({ type: "candidate", candidate: candidate.candidate, sdpMid: candidate.sdpMid, sdpMLineIndex: candidate.sdpMLineIndex });
        };
        peer.ontrack = ({ track }) => {
          if (!this.current(epoch)) return;
          this.video.srcObject = new MediaStream([track]);
          void this.video.play().catch(() => { if (this.current(epoch)) this.update({ status: "Tap the picture to start video." }); });
        };
        peer.onconnectionstatechange = () => {
          if (!this.current(epoch)) return;
          if (["failed", "disconnected", "closed"].includes(connectedPeer.connectionState)) this.fail(new RemoteError("Screen disconnected."));
          else this.ready();
        };
        peer.ondatachannel = ({ channel }) => { if (this.current(epoch)) this.channel(channel, epoch); else channel.close(); };
      }).catch(error => { if (this.current(epoch)) this.fail(error); });
      const url = new URL("/v1/account/hands/view", location.origin);
      url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
      url.search = new URLSearchParams({ machine_id: this.hand.machine_id, surface_id: this.hand.id, generation: this.hand.generation }).toString();
      const socket = new WebSocket(url); this.socket = socket;
      socket.onclose = () => { if (this.current(epoch)) this.fail(new RemoteError("Screen disconnected.")); };
      socket.onerror = () => { if (this.current(epoch)) this.fail(new RemoteError("Could not connect to this screen.")); };
      const candidates: RTCIceCandidateInit[] = [];
      let signalQueue = Promise.resolve();
      let queuedMessages = 0;
      socket.onmessage = ({ data }) => {
        if (!this.current(epoch)) return;
        let message: any;
        try {
          if (typeof data !== "string" || encoder.encode(data).length > (frames ? 710_000 : 70_000)) throw new RemoteError("Invalid remote signal.", true);
          if (++queuedMessages > 128) throw new RemoteError("Too many remote signals.", true);
          message = JSON.parse(data);
          if (!message || typeof message !== "object") throw new RemoteError("Invalid remote signal.", true);
          if (frames && message.type === "frame") {
            if (!this.framePending || this.frameDecoding) throw new RemoteError("Unexpected remote frame.", true);
            this.frameDecoding = true;
          } else if (frames && encoder.encode(data).length > 8192) throw new RemoteError("Invalid remote signal.", true);
        } catch { this.fail(new RemoteError("Invalid remote signal.", true)); return; }
        signalQueue = signalQueue.then(async () => {
          if (!this.current(epoch)) return;
          if (message.type === "ready") {
            if (this.renewTimer || typeof message.connection_id !== "string" || message.connection_id.length > 128) throw new RemoteError("Invalid remote lease.", true);
            const id = message.connection_id;
            this.authorized(epoch);
            this.renewTimer = setInterval(() => {
              void request("/renew", "POST", { connection_id: id }, signal).then(() => {
                if (this.current(epoch) && socket.readyState === WebSocket.OPEN) socket.send('{"type":"ping"}');
              }).catch(error => { if (this.current(epoch)) this.fail(error); });
            }, 10_000);
            if (frames) this.requestFrame(epoch);
          } else if (message.type === "renewed") this.authorized(epoch);
          else if (message.type === "pong") return; // Liveness is not lease authorization.
          else if (frames && message.type === "frame") await this.renderFrame(message, epoch);
          else if (frames && message.type === "control") this.receiveControl(message.data, epoch);
          else if (!frames && message.type === "signal") {
            await peerReady;
            if (!this.current(epoch)) return;
            if (!peer) throw new RemoteError("Could not initialize this screen.");
            const offer = message.signal;
            if (!offer || typeof offer !== "object") throw new RemoteError("Invalid remote offer.", true);
            if (offer.type === "candidate") {
              if (candidates.length >= 128) throw new RemoteError("Too many remote candidates.", true);
              if (peer.remoteDescription) await peer.addIceCandidate(offer);
              else candidates.push(offer);
            } else if (offer.type === "offer" && typeof offer.sdp === "string" && encoder.encode(offer.sdp).length <= 65_536) {
              // Initial credentials are already fresh; only subsequent offers
              // (host ICE restarts) need another authenticated TURN request.
              if (peer.remoteDescription) {
                const ice = await request("/ice", "POST", undefined, signal);
                if (!this.current(epoch)) return;
                peer.setConfiguration({ ...peer.getConfiguration(), iceServers: ice.iceServers });
              }
              await peer.setRemoteDescription({ type: "offer", sdp: offer.sdp });
              if (!this.current(epoch)) return;
              for (const candidate of candidates.splice(0)) await peer.addIceCandidate(candidate);
              if (!this.current(epoch)) return;
              const answer = await peer.createAnswer();
              if (!this.current(epoch)) return;
              await peer.setLocalDescription(answer);
              if (this.current(epoch)) this.signal({ type: "answer", sdp: peer.localDescription!.sdp });
            } else throw new RemoteError("Invalid remote offer.", true);
          } else throw new RemoteError("Invalid remote signal.", true);
        }).catch(error => { if (this.current(epoch)) this.fail(error instanceof SyntaxError ? new RemoteError("Invalid remote signal.", true) : error); }).finally(() => { queuedMessages--; });
      };
      this.authorized(epoch);
      await peerReady;
    } catch (error) { if (this.current(epoch)) this.fail(error); }
  }

  takeControl(): void {
    if (this.state.connected && this.hand.controllable && !this.state.controlling && !this.controlRequested) {
      this.controlRequested = true; this.acquireControl();
    }
  }
  private acquireControl(): void {
    if (this.control !== "idle" || !this.controlRequested) return;
    this.control = "acquiring"; this.send({ type: "acquire" });
  }
  releaseControl(): void {
    this.controlRequested = false;
    const generation = this.generation;
    if (this.control === "acquiring") this.control = "cancelled-acquire";
    else if (generation) this.control = { kind: "releasing", generation };
    clearInterval(this.controlTimer); this.controlTimer = undefined;
    if (generation && !this.closed) this.send({ type: "release", generation });
    if (!this.closed) this.update({ controlling: false, ...(this.state.connected ? { status: "Watching" } : {}) });
  }
  input(event: RemoteInput): void {
    if (!this.state.controlling || !this.generation || this.closed || this.suspended) return;
    this.send({ ...event, sequence: ++this.sequence, generation: this.generation }, event.kind === "move");
  }
  close(status = "Disconnected"): void {
    if (this.closed) return;
    this.closed = true; this.detach();
    this.update({ status, connected: false, controlling: false, connecting: false });
  }
  private detach(): void {
    ++this.epoch;
    // Teardown is best effort: never let a failed release reenter recovery.
    if (this.generation) {
      const release = { type: "release", generation: this.generation };
      try {
        if (this.hand.transport === "frames-v1" && this.socket?.readyState === WebSocket.OPEN) this.socket.send(JSON.stringify({ type: "control", data: release }));
        else if (this.reliable?.readyState === "open") this.reliable.send(JSON.stringify(release));
      } catch { /* Closing the connection also expires control. */ }
    }
    this.control = "idle"; this.controlRequested = false; this.sequence = 0;
    this.abort.abort();
    clearTimeout(this.watchdog); clearTimeout(this.connectingTimer); clearTimeout(this.retryTimer);
    clearTimeout(this.frameTimer); clearTimeout(this.frameDeadline); this.framePending = false; this.frameDecoding = false;
    clearInterval(this.renewTimer); clearInterval(this.controlTimer);
    this.watchdog = this.connectingTimer = this.retryTimer = this.renewTimer = this.controlTimer = undefined;
    this.frameTimer = this.frameDeadline = undefined;
    if (this.socket) { this.socket.onclose = this.socket.onerror = this.socket.onmessage = null; this.socket.close(); }
    if (this.peer) { this.peer.onconnectionstatechange = this.peer.ontrack = this.peer.onicecandidate = this.peer.ondatachannel = null; this.peer.close(); }
    this.socket = undefined; this.peer = undefined; this.reliable = undefined; this.motion = undefined;
    this.video.srcObject = null;
    if (this.canvas) { this.canvas.width = 0; this.canvas.height = 0; }
  }
  private fail(error: unknown): void {
    if (this.closed || this.suspended) return;
    const status = error instanceof Error ? error.message : "Could not connect to this screen.";
    this.detach();
    const now = performance.now();
    this.recoveryDeadline ??= now + 90_000;
    const retry = !(error instanceof RemoteError && error.terminal) && now < this.recoveryDeadline;
    this.update({ status: retry ? "Reconnecting…" : status, connected: false, controlling: false, connecting: retry });
    if (!retry) return;
    const epoch = this.epoch;
    const delay = Math.min(1000 * 2 ** Math.min(this.retries++, 3), this.recoveryDeadline - now);
    this.retryTimer = setTimeout(() => {
      if (!this.current(epoch)) return;
      if (performance.now() >= this.recoveryDeadline!) this.update({ status, connecting: false });
      else void this.start(true);
    }, delay);
  }
  private authorized(epoch: number): void {
    clearTimeout(this.watchdog);
    this.watchdog = setTimeout(() => { if (this.current(epoch)) this.fail(new RemoteError("This remote session is no longer authorized.", true)); }, 25_000);
  }
  private signal(signal: unknown): void {
    if (this.closed || this.suspended) return;
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN || this.socket.bufferedAmount > 128_000) { this.fail(new RemoteError("Signaling connection unavailable.")); return; }
    try { this.socket.send(JSON.stringify({ type: "signal", signal })); }
    catch { this.fail(new RemoteError("Signaling connection unavailable.")); }
  }
  private send(value: unknown, motion = false): void {
    const frames = this.hand.transport === "frames-v1";
    const channel = frames ? this.socket : motion ? this.motion : this.reliable;
    if (this.closed || this.suspended || !channel || channel.readyState !== (frames ? WebSocket.OPEN : "open")) return;
    if (channel.bufferedAmount > (motion ? 4096 : 32_768)) { if (!motion) this.fail(new RemoteError("Connection too slow for control.")); return; }
    const wire = JSON.stringify(frames ? { type: value && typeof value === "object" && "kind" in value ? "input" : "control", data: value } : value);
    if (encoder.encode(wire).length > 8192) { this.fail(new RemoteError("Input is too large.", true)); return; }
    try { channel.send(wire); } catch { this.fail(new RemoteError("Input connection closed.")); }
  }
  private requestFrame(epoch: number): void {
    if (!this.current(epoch) || this.framePending) return;
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN || this.socket.bufferedAmount > 32_768) {
      this.fail(new RemoteError("Screen connection unavailable.")); return;
    }
    this.framePending = true; this.frameRequestedAt = performance.now();
    this.frameDeadline = setTimeout(() => { if (this.current(epoch)) this.fail(new RemoteError("This screen stopped sending frames.")); }, 10_000);
    try { this.socket.send('{"type":"frame_request"}'); }
    catch { this.fail(new RemoteError("Screen connection unavailable.")); }
  }
  private async renderFrame(value: { jpeg?: unknown; width?: unknown; height?: unknown }, epoch: number): Promise<void> {
    if (!this.framePending || !this.canvas) throw new RemoteError("Unexpected remote frame.", true);
    const bytes = frameBytes(value);
    let bitmap: ImageBitmap | undefined;
    try {
      bitmap = await createImageBitmap(new Blob([bytes], { type: "image/jpeg" }));
      if (!this.current(epoch)) return;
      if (bitmap.width !== value.width || bitmap.height !== value.height) throw new RemoteError("Invalid remote frame.", true);
      const context = this.canvas.getContext("2d");
      if (!context) throw new RemoteError("This browser cannot display this screen.", true);
      if (this.canvas.width !== bitmap.width) this.canvas.width = bitmap.width;
      if (this.canvas.height !== bitmap.height) this.canvas.height = bitmap.height;
      context.drawImage(bitmap, 0, 0);
      clearTimeout(this.frameDeadline); this.frameDeadline = undefined; this.framePending = false;
      this.ready(true);
      this.frameTimer = setTimeout(() => this.requestFrame(epoch), Math.ceil(Math.max(0, 100 - (performance.now() - this.frameRequestedAt))));
    } catch (error) {
      throw error instanceof RemoteError ? error : new RemoteError("Invalid remote frame.", true);
    } finally { bitmap?.close(); if (this.current(epoch)) this.frameDecoding = false; }
  }
  private channel(channel: RTCDataChannel, epoch: number): void {
    if (channel.label === "remote-control-v1" && !this.reliable && channel.ordered && channel.maxRetransmits === null && channel.maxPacketLifeTime === null) this.reliable = channel;
    else if (channel.label === "remote-motion-v1" && !this.motion && !channel.ordered && channel.maxRetransmits === 0 && channel.maxPacketLifeTime === null) this.motion = channel;
    else { channel.close(); this.fail(new RemoteError("Invalid remote input channel.", true)); return; }
    channel.onopen = () => { if (this.current(epoch)) this.ready(); };
    channel.onclose = () => { if (this.current(epoch)) this.fail(new RemoteError("Input connection closed.")); };
    channel.onmessage = ({ data }) => {
      if (!this.current(epoch)) return;
      try {
        if (channel !== this.reliable || typeof data !== "string" || encoder.encode(data).length > 8192) throw new Error();
        this.receiveControl(JSON.parse(data), epoch);
      } catch { this.fail(new RemoteError("Invalid remote control response.", true)); }
    };
    this.ready();
  }
  private receiveControl(value: any, epoch: number): void {
    if (!this.current(epoch)) return;
    if (!value || typeof value !== "object" || (value.generation !== undefined &&
      (typeof value.generation !== "string" || !value.generation.length || value.generation.length > 128))) throw new RemoteError("Invalid remote control response.", true);
    if (value.type === "granted" && typeof value.generation === "string" && value.generation.length > 0 && value.generation.length <= 128) {
      if (this.control === "cancelled-acquire") {
        this.control = { kind: "releasing", generation: value.generation };
        this.send({ type: "release", generation: value.generation }); return;
      }
      if (this.control !== "acquiring") throw new RemoteError("Invalid remote control response.", true);
      this.control = { kind: "held", generation: value.generation }; this.sequence = 0;
      this.update({ controlling: true, status: "You’re controlling" });
      clearInterval(this.controlTimer);
      this.controlTimer = setInterval(() => { if (this.current(epoch)) this.send({ type: "renew", generation: this.generation }); }, 3000);
    } else if (value.type === "revoked") {
      if (typeof this.control === "object") {
        if (value.generation !== undefined && value.generation !== this.control.generation) return;
        const released = this.control.kind === "releasing";
        this.control = "idle";
        clearInterval(this.controlTimer); this.controlTimer = undefined;
        this.update({ controlling: false, status: "Watching" });
        if (released) { this.acquireControl(); return; }
      } else if (this.control === "acquiring") this.control = "cancelled-acquire";
      // An unsolicited revocation cancels intent, including an in-flight grant.
      this.controlRequested = false;
    } else if (value.type === "denied") {
      const cancelled = this.control === "cancelled-acquire";
      if (!cancelled && this.control !== "acquiring") throw new RemoteError("Invalid remote control response.", true);
      this.control = "idle";
      if (cancelled) this.acquireControl();
      else { this.controlRequested = false; this.update({ status: "Another viewer is controlling this screen." }); }
    }
    else throw new RemoteError("Invalid remote control response.", true);
  }
  private ready(frame = false): void {
    if (!this.closed && !this.suspended && !this.state.connected && (frame || (this.peer?.connectionState === "connected" && this.reliable?.readyState === "open" && this.motion?.readyState === "open"))) {
      clearTimeout(this.connectingTimer); this.retries = 0; this.recoveryDeadline = undefined;
      this.update({ connected: true, connecting: false, status: "Watching" });
    }
  }
  private update(patch: Partial<RemoteState>): void { this.state = { ...this.state, ...patch }; this.changed(this.state); }
}
/** Physical keys use the same USB HID page as the native clients. */
export const remoteKeys: Readonly<Record<string, number>> = Object.freeze({
  ...Object.fromEntries(Array.from({ length: 26 }, (_, index) => ["Key" + String.fromCharCode(65 + index), 4 + index])),
  ...Object.fromEntries(Array.from({ length: 9 }, (_, index) => ["Digit" + (index + 1), 30 + index])),
  ...Object.fromEntries(Array.from({ length: 12 }, (_, index) => ["F" + (index + 1), 58 + index])),
  Digit0: 39, Enter: 40, Escape: 41, Backspace: 42, Tab: 43, Space: 44, Minus: 45, Equal: 46,
  BracketLeft: 47, BracketRight: 48, Backslash: 49, Semicolon: 51, Quote: 52, Backquote: 53,
  Comma: 54, Period: 55, Slash: 56, CapsLock: 57, Insert: 73, Home: 74, PageUp: 75, Delete: 76,
  End: 77, PageDown: 78, ArrowRight: 79, ArrowLeft: 80, ArrowDown: 81, ArrowUp: 82,
  ControlLeft: 224, ShiftLeft: 225, AltLeft: 226, MetaLeft: 227, ControlRight: 228, ShiftRight: 229, AltRight: 230, MetaRight: 231,
});
