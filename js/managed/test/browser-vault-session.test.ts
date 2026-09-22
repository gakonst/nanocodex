import { afterEach, describe, expect, it, vi } from "vitest";
import { PrivateBrowserCdp, PrivateBrowserContinuationSession } from "../src/browser-vault";

const identity = { vault_id: "a".repeat(22), target_id: "target", expected_origin: "https://example.test" };
const signal = () => new AbortController().signal;
function fixture() {
  const sockets: { cdp: PrivateBrowserCdp; close: ReturnType<typeof vi.fn> }[] = [];
  const connect = vi.spyOn(PrivateBrowserCdp, "connect").mockImplementation(async () => {
    const cdp = { closed: false, close: vi.fn(() => { cdp.closed = true; }) };
    sockets.push({ cdp: cdp as unknown as PrivateBrowserCdp, close: cdp.close });
    return cdp as unknown as PrivateBrowserCdp;
  });
  const retained = new PrivateBrowserContinuationSession({} as never, 1000);
  const run = (abortSignal = signal(), request = identity, session = "browser") => retained.run(session, request, abortSignal, async cdp => cdp);
  return { retained, run, sockets, connect };
}
afterEach(() => { vi.restoreAllMocks(); vi.useRealTimers(); });
describe("bounded private continuation transport", () => {
  it("reuses only the same provider session and Vault identity and expires idle connections", async () => {
    vi.useFakeTimers();
    const f = fixture();
    const first = await f.run();
    expect(await f.run()).toBe(first);
    expect(f.connect).toHaveBeenCalledTimes(1);
    for (const [request, session] of [
      [{ ...identity, vault_id: "b".repeat(22) }, "browser"],
      [{ ...identity, target_id: "other-target" }, "browser"],
      [{ ...identity, expected_origin: "https://other.test" }, "browser"],
      [identity, "replacement-browser"],
    ] as const) {
      const previous = f.sockets.at(-1)!;
      await f.run(signal(), request, session);
      expect(previous.close).toHaveBeenCalledOnce();
    }
    await vi.advanceTimersByTimeAsync(1000);
    expect(f.sockets.at(-1)!.close).toHaveBeenCalledOnce();
    await f.run();
    expect(f.connect).toHaveBeenCalledTimes(6);
    f.retained.close();
  });
  it("removes completed call abort listeners but closes active calls without retry", async () => {
    const f = fixture(), old = new AbortController(), active = new AbortController();
    const first = await f.run(old.signal);
    old.abort();
    expect(await f.run()).toBe(first);
    const running = f.retained.run("browser", identity, active.signal, async cdp => {
      active.abort();
      expect(cdp.closed).toBe(true);
      throw new Error("cancelled");
    });
    await expect(running).rejects.toThrow("cancelled");
    expect(f.connect).toHaveBeenCalledTimes(1);
    expect(await f.run()).not.toBe(first);
    f.retained.close();
  });
  it("does not connect for an aborted call and closes a connect completed after abort", async () => {
    const f = fixture(), aborted = new AbortController();
    aborted.abort();
    await expect(f.run(aborted.signal)).rejects.toThrow();
    expect(f.connect).not.toHaveBeenCalled();
    const during = new AbortController(), close = vi.fn();
    f.connect.mockImplementationOnce(async () => { during.abort(); return { close } as unknown as PrivateBrowserCdp; });
    await expect(f.run(during.signal)).rejects.toThrow();
    expect(close).toHaveBeenCalledOnce();
  });
  it("discards disconnected or failed transports and does not replay operations", async () => {
    const f = fixture();
    const first = await f.run();
    first.close();
    expect(await f.run()).not.toBe(first);
    const operation = vi.fn(async () => { throw new Error("ambiguous"); });
    await expect(f.retained.run("browser", identity, signal(), operation)).rejects.toThrow("ambiguous");
    expect(operation).toHaveBeenCalledOnce();
    expect(f.sockets.at(-1)!.close).toHaveBeenCalledOnce();
    await f.run();
    expect(f.connect).toHaveBeenCalledTimes(3);
    f.retained.close();
  });
});

describe("private target attachment lifecycle", () => {
  it("retains one target attachment and invalidates detach and disconnect events", async () => {
    const listeners = new Map<string, (event: any) => void>();
    let serial = 0;
    const send = vi.fn((raw: string) => {
      const message = JSON.parse(raw);
      listeners.get("message")!({ data: JSON.stringify({ id: message.id, result: message.method === "Target.attachToTarget" ? { sessionId: `attachment-${++serial}` } : {} }) });
    });
    const socket = { accept() {}, addEventListener(type: string, listener: (event: any) => void) { listeners.set(type, listener); }, send, close: vi.fn() };
    const cdp = new PrivateBrowserCdp(socket as unknown as WebSocket);
    expect(await cdp.attachTarget("one")).toEqual({ sessionId: "attachment-1" });
    expect(await cdp.attachTarget("one")).toEqual({ sessionId: "attachment-1" });
    expect(send).toHaveBeenCalledTimes(1);
    expect(await cdp.attachTarget("two")).toEqual({ sessionId: "attachment-2" });
    expect(JSON.parse(send.mock.calls[1]![0]).method).toBe("Target.detachFromTarget");
    listeners.get("message")!({ data: JSON.stringify({ method: "Target.detachedFromTarget", params: { sessionId: "attachment-2" } }) });
    expect(await cdp.attachTarget("two")).toEqual({ sessionId: "attachment-3" });
    cdp.close();
    await expect(cdp.attachTarget("two")).rejects.toThrow("disconnected");
  });
});

describe("private browser upgrade cancellation", () => {
  it("keeps a retained socket alive past the handshake deadline and completed call cancellation", async () => {
    vi.useFakeTimers();
    const call = new AbortController();
    let upgradeSignal: AbortSignal | undefined;
    const socket = { accept() {}, addEventListener() {}, close: vi.fn() };
    const browser = { fetch: vi.fn(async (_url: string, options: RequestInit) => {
      upgradeSignal = options.signal as AbortSignal;
      upgradeSignal.addEventListener("abort", () => socket.close());
      return { webSocket: socket };
    }) };
    const cdp = await PrivateBrowserCdp.connect(browser as never, "browser", call.signal);
    call.abort();
    await vi.advanceTimersByTimeAsync(10_001);
    expect(upgradeSignal!.aborted).toBe(false);
    expect(socket.close).not.toHaveBeenCalled();
    expect(cdp.closed).toBe(false);
    cdp.close();
  });
  it("still cancels outstanding upgrades on timeout and caller abort", async () => {
    vi.useFakeTimers();
    for (const cancel of ["timeout", "caller"] as const) {
      const call = new AbortController();
      const browser = { fetch: vi.fn((_url: string, options: RequestInit) => new Promise((_resolve, reject) => {
        options.signal!.addEventListener("abort", () => reject(new Error("cancelled")), { once: true });
      })) };
      const pending = expect(PrivateBrowserCdp.connect(browser as never, "browser", call.signal)).rejects.toThrow("cancelled");
      if (cancel === "caller") call.abort();
      else await vi.advanceTimersByTimeAsync(10_000);
      await pending;
      expect(vi.getTimerCount()).toBe(0);
    }
  });
});

// Exercise actual transport sanitization; a raw-error mock misses this boundary.
describe("private touch cancellation error classification", () => {
  it("allows recovery through the sanitizing CDP transport", async () => {
    const listeners = new Map<string, (event: any) => void>();
    const socket = {
      accept() {}, close() {},
      addEventListener(type: string, listener: (event: any) => void) { listeners.set(type, listener); },
      send(raw: string) {
        const { id, method } = JSON.parse(raw);
        let result: unknown = {};
        if (method === "Target.getTargetInfo") result = { targetInfo: { type: "page", targetId: "target", url: identity.expected_origin } };
        if (method === "Target.attachToTarget") result = { sessionId: "private" };
        if (method === "Page.getFrameTree") result = { frameTree: { frame: { id: "top", url: identity.expected_origin } } };
        if (method === "Page.getLayoutMetrics") result = { cssLayoutViewport: { clientWidth: 390, clientHeight: 700 } };
        if (method === "Page.captureScreenshot") result = { data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a1X8AAAAASUVORK5CYII=" };
        listeners.get("message")!({ data: JSON.stringify(method === "Input.dispatchTouchEvent"
          ? { id, error: { code: -32602, message: "Must send a TouchStart first to start a new touch." } } : { id, result }) });
      },
    };
    const cdp = new PrivateBrowserCdp(socket as unknown as WebSocket);
    const { privateVaultTakeover } = await import("../src/browser-vault-takeover");
    const touch = { active: false, uncertain: true };
    expect((await privateVaultTakeover(cdp, identity, { action: "observe" }, touch)).status).toBe("active");
    expect(touch).toEqual({ active: false, uncertain: false });
    cdp.close();
  });
  it.each([
    ["Input.dispatchTouchEvent", "touchStart", -32602, "Must send a TouchStart first to start a new touch."],
    ["Input.insertText", "touchCancel", -32602, "Must send a TouchStart first to start a new touch."],
    ["Input.dispatchTouchEvent", "touchCancel", -32601, "Must send a TouchStart first to start a new touch."],
    ["Input.dispatchTouchEvent", "touchCancel", -32602, "private text https://provider.invalid"],
  ])("does not classify unrelated provider failures (%s %s)", async (method, type, code, message) => {
    let receive: (event: any) => void = () => {};
    const socket = { accept() {}, close() {}, addEventListener(event: string, listener: typeof receive) { if (event === "message") receive = listener; },
      send(raw: string) { receive({ data: JSON.stringify({ id: JSON.parse(raw).id, error: { code, message } }) }); } };
    const cdp = new PrivateBrowserCdp(socket as unknown as WebSocket);
    await expect(cdp.send(method, { type })).rejects.toThrow(/^Private browser operation failed$/);
    cdp.close();
  });
});
