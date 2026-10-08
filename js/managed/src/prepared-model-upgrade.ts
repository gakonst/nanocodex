import { consumeRpcData } from "nanocodex/cloudflare/rpc";

const PREPARED_HEADER = "x-nanocodex-prepared-model-upgrade";
const TTL_MS = 10_000;
const ACK_TIMEOUT_MS = 1_000;
type UpgradeBinding = Pick<Fetcher, "fetch"> & {
  prepareModelUpgrade?: (input: { headers: [string, string][] }) => Promise<unknown>;
  cancelModelUpgrade?: (input: { headers: [string, string][]; id: string }) => Promise<unknown>;
};

/** One auth-only upgrade. Remote preparation is acknowledged before writes;
 * only fetch transfers its WebSocket, after durable admission. */
export class PreparedModelUpgrade {
  readonly #request: Request;
  readonly #binding: UpgradeBinding;
  readonly #abort = new AbortController();
  readonly #ready: Promise<void>;
  readonly #timer: ReturnType<typeof setTimeout>;
  readonly #started = Date.now();
  #promise?: Promise<Response | undefined>;
  #remoteId?: string;
  #response?: Response;
  #end: () => void = () => {};
  readonly #ended = new Promise<undefined>(resolve => { this.#end = () => resolve(undefined); });
  #disposed = false;
  #claimed = false;
  #transferred = false;
  constructor(request: Request, binding: UpgradeBinding) {
    this.#request = request;
    this.#binding = binding;
    this.#timer = setTimeout(() => this.dispose("expired"), TTL_MS);
    this.#observe("started");
    if (typeof binding.prepareModelUpgrade === "function") {
      this.#ready = this.#prepareRemote();
    } else {
      // Older bindings and local SDK fixtures retain the existing fetch path.
      this.#promise = this.#fetch(request);
      this.#ready = Promise.resolve();
    }
  }
  /** Bounded remote-handler acknowledgment, never handshake completion. */
  acknowledged(): Promise<void> { return this.#ready; }
  #headers(): [string, string][] { return [...this.#request.headers.entries()]; }
  #prepareRemote(): Promise<void> {
    return new Promise(resolve => {
      let abandoned = false;
      const timer = setTimeout(() => {
        abandoned = true;
        this.#observe("ack_timeout");
        resolve();
      }, ACK_TIMEOUT_MS);
      let attempt: Promise<unknown>;
      try { attempt = this.#binding.prepareModelUpgrade!({ headers: this.#headers() }); }
      catch { attempt = Promise.reject(new Error("Preparation unavailable")); }
      void Promise.resolve(attempt).then(result => {
        clearTimeout(timer);
        let value: { status?: unknown; id?: unknown } | null;
        try { value = consumeRpcData(result) as typeof value; }
        catch { this.#observe("ack_failed"); resolve(); return; }
        if (value?.status === "prepared" && typeof value.id === "string"
          && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value.id)) {
          if (abandoned || this.#disposed || this.#expired()) this.#cancelRemote(value.id);
          else { this.#remoteId = value.id; this.#observe("acknowledged"); }
        } else if (!abandoned && !this.#disposed) this.#observe("ack_unavailable");
        resolve();
      }, () => {
        clearTimeout(timer);
        if (!abandoned && !this.#disposed) this.#observe("ack_failed");
        resolve();
      });
    });
  }
  #fetch(request: Request): Promise<Response | undefined> {
    let pending: Promise<Response>;
    try { pending = this.#binding.fetch(new Request(request, { signal: this.#abort.signal })); }
    catch { pending = Promise.reject(new Error("Preparation unavailable")); }
    return pending.then(response => {
      this.#response = response;
      if (this.#disposed || this.#expired() || response.status !== 101 || !response.webSocket) {
        this.dispose("unavailable");
        return undefined;
      }
      this.#observe("connected");
      return response;
    }).catch(() => { this.dispose("failed"); return undefined; });
  }
  async take(request: Request, durable: () => Promise<void>, valid: () => boolean): Promise<Response | undefined> {
    if (!valid()) { this.dispose("stale"); throw new Error("Managed model preparation is no longer authorized"); }
    if (this.#expired()) this.dispose("expired");
    if (this.#claimed || this.#disposed) return undefined;
    if (request.method !== "GET" || request.url !== this.#request.url) return undefined;
    const headers = (value: Request) => JSON.stringify([...value.headers.entries()].sort(([a], [b]) => a.localeCompare(b)));
    if (headers(request) !== headers(this.#request) || !valid()) {
      this.dispose("mismatch");
      return undefined;
    }
    this.#claimed = true;
    try {
      await durable();
      await this.#ready;
      if (!valid()) { this.dispose("stale"); throw new Error("Managed model preparation is no longer authorized"); }
      if (this.#disposed || this.#expired()) { this.dispose("expired"); return undefined; }
      if (this.#remoteId) {
        const consume = new Request(request);
        consume.headers.set(PREPARED_HEADER, this.#remoteId);
        this.#promise = this.#fetch(consume);
      }
      const response = await Promise.race([this.#promise ?? Promise.resolve(undefined), this.#ended]);
      if (!valid()) { this.dispose("stale"); throw new Error("Managed model preparation is no longer authorized"); }
      if (!response || this.#disposed || this.#expired()) { this.dispose("stale"); return undefined; }
      this.#transferred = true;
      this.#remoteId = undefined;
      clearTimeout(this.#timer);
      this.#observe("consumed");
      return response;
    } catch (error) { this.dispose("durability_failed"); throw error; }
  }
  #expired(): boolean { return Date.now() >= this.#started + TTL_MS; }
  #cancelRemote(id: string): void {
    try { void this.#binding.cancelModelUpgrade?.({ headers: this.#headers(), id }).then(consumeRpcData).catch(() => {}); }
    catch { /* Remote holder also enforces bounded expiry. */ }
  }
  dispose(outcome: string): void {
    if (this.#transferred) return;
    if (!this.#disposed) {
      this.#disposed = true;
      this.#end();
      clearTimeout(this.#timer);
      this.#abort.abort();
      if (this.#remoteId) { this.#cancelRemote(this.#remoteId); this.#remoteId = undefined; }
      this.#observe(outcome);
    }
    const response = this.#response;
    this.#response = undefined;
    if (response?.webSocket) {
      try { response.webSocket.accept(); } catch { /* already accepted/closed */ }
      try { response.webSocket.close(1000, "Preparation ended"); } catch { /* disconnected */ }
    } else { void response?.body?.cancel().catch(() => {}); }
  }
  #observe(outcome: string): void {
    console.info({ type: "managed.model_upgrade_preparation", outcome, at_ms: Date.now(), elapsed_ms: Date.now() - this.#started });
  }
}
