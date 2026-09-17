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
