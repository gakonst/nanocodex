import { connectBrowserSession, type BrowserBinding, type BrowserSessionInfo, type CdpSession } from "agents/browser";
import type { ToolContext } from "nanocodex";

export type BrowserControlState = {
  mode: "agent" | "human";
  generation: string;
  reason: string;
  requestId?: string;
  completedRequestId?: string;
};

/** Owner-only control plane. No browser frames, input, or CDP results enter agent receipts. */
export class BrowserControl {
  private active = 0;
  private tail: Promise<unknown> = Promise.resolve();
  private readonly ready: Promise<void>;
  private state: BrowserControlState = { mode: "agent", generation: "", reason: "" };

  constructor(
    private readonly storage: Pick<DurableObjectStorage, "get" | "put">,
    private readonly browser: BrowserBinding,
    private readonly sessionInfo: () => Promise<BrowserSessionInfo | undefined>,
  ) {
    this.ready = storage.get<BrowserControlState>("browser:control").then((saved) => {
      if (saved) this.state = saved;
    });
  }

  async model<T>(context: ToolContext, run: () => Promise<T>): Promise<T> {
    await this.ready;
    context.signal?.throwIfAborted();
    while (!(await this.exclusive(async () => {
      if (this.state.mode === "human") return false;
      this.active++;
      return true;
    }))) await this.wait(context.signal);
    try { return await run(); } finally { this.active--; }
  }

  async handoff(reason: string, context: ToolContext): Promise<unknown> {
    await this.exclusive(async () => {
      context.signal?.throwIfAborted();
      if (this.state.completedRequestId === context.callId) return;
      if (this.active) throw new Error("Browser is busy; finish the current browser operation first.");
      if (this.state.mode === "agent") await this.save({
        mode: "human", generation: crypto.randomUUID(), reason, requestId: context.callId,
      });
    });
    await this.wait(context.signal);
    return { status: "returned_to_agent", message: "The user returned control. Verify the current page before continuing; this does not prove authentication succeeded." };
  }

  private async wait(signal?: AbortSignal): Promise<void> {
    while (this.state.mode === "human") {
      signal?.throwIfAborted();
      await new Promise<void>((resolve, reject) => {
        const done = () => { clearTimeout(timer); signal?.removeEventListener("abort", abort); resolve(); };
        const abort = () => { clearTimeout(timer); signal?.removeEventListener("abort", abort); reject(signal?.reason ?? new Error("Cancelled")); };
        const timer = setTimeout(done, 500);
        signal?.addEventListener("abort", abort, { once: true });
      });
    }
    signal?.throwIfAborted();
  }

  private async save(state: BrowserControlState): Promise<void> {
    // Close the gate immediately; reopen it only after the release is durable.
    if (state.mode === "human") this.state = state;
    await this.storage.put("browser:control", state);
    this.state = state;
  }

  private async exclusive<T>(run: () => Promise<T>): Promise<T> {
    const next = this.tail.then(async () => { await this.ready; return run(); });
    this.tail = next.catch(() => {});
    return next;
  }

  async request(request: Request): Promise<Response> {
    return this.exclusive(async () => {
      const reply = (body: unknown, status = 200) => Response.json(body, { status, headers: {
        "cache-control": "no-store", "referrer-policy": "no-referrer",
      } });
      const operation = new URL(request.url).pathname.split("/").at(-1);
      if (request.method === "GET" && operation === "browser") {
        let info: BrowserSessionInfo | undefined;
        try { info = await this.sessionInfo(); } catch { return reply({ ...this.state, available: false, tabs: [] }); }
        return reply({ ...this.state, available: !!info, tabs: (info?.targets ?? [])
          .filter((t) => t.type === "page")
          .map((t) => ({ id: t.id, title: t.title ?? "", url: t.url ?? "" })) });
      }
      if (request.method !== "POST") return reply({ error: "method_not_allowed" }, 405);
      const encoded = await request.text();
      if (encoded.length > 16_384) return reply({ error: "input_too_large" }, 413);
      let body: Record<string, unknown>;
      try { body = JSON.parse(encoded); } catch { return reply({ error: "invalid_request" }, 400); }
      if (!body || typeof body !== "object" || Array.isArray(body)) return reply({ error: "invalid_request" }, 400);
      if (operation === "takeover") {
        if (this.active) return reply({ error: "browser_busy", message: "The browser is finishing an action. Try taking control again." }, 409);
        if (this.state.mode === "agent") await this.save({ mode: "human", generation: crypto.randomUUID(), reason: "You have control" });
        return reply(this.state);
      }
      if ((operation !== "frame" && this.state.mode !== "human") || body.generation !== this.state.generation) {
        return reply({ error: "control_changed", message: "Browser control changed. Refresh before continuing." }, 409);
      }
      if (operation === "release") {
        await this.save({ mode: "agent", generation: crypto.randomUUID(), reason: "", completedRequestId: this.state.requestId });
        return reply(this.state);
      }
      const info = await this.sessionInfo().catch(() => undefined);
      if (!info) return reply({ error: "browser_expired", message: "This browser has expired. Return control so the agent can reopen the page." }, 409);
      let cdp: CdpSession | undefined;
      try {
        cdp = await connectBrowserSession(this.browser, info.sessionId, 15_000);
        const targets = await cdp.send("Target.getTargets") as { targetInfos: { targetId: string; type: string; url: string }[] };
        const target = targets.targetInfos.find((t) => t.type === "page" && t.targetId === body.target);
        if (!target) return reply({ error: "page_changed" }, 409);
        const sessionId = await cdp.attachToTarget(target.targetId);
        const send = (method: string, params?: unknown) => cdp!.send(method, params, { sessionId });
        if (operation === "frame") {
          const result = await send("Page.captureScreenshot", { format: "jpeg", quality: 65, captureBeyondViewport: false }) as { data: string };
          const metrics = await send("Page.getLayoutMetrics") as { cssLayoutViewport: { clientWidth: number; clientHeight: number } };
          return reply({ data: result.data, width: metrics.cssLayoutViewport.clientWidth, height: metrics.cssLayoutViewport.clientHeight });
        }
        if (operation === "navigate") {
          if (typeof body.url !== "string") return reply({ error: "invalid_url" }, 400);
          const url = new URL(body.url);
          if (!["https:", "http:"].includes(url.protocol) || url.username || url.password) return reply({ error: "invalid_url" }, 400);
          await send("Page.navigate", { url: url.href });
        } else if (operation === "click") {
          if (![body.x, body.y].every((v) => typeof v === "number" && Number.isFinite(v) && v >= 0 && v < 20_000)) return reply({ error: "invalid_coordinates" }, 400);
          for (const type of ["mousePressed", "mouseReleased"]) await send("Input.dispatchMouseEvent", { type, x: body.x, y: body.y, button: "left", clickCount: 1 });
        } else if (operation === "scroll") {
          if (typeof body.deltaY !== "number" || !Number.isFinite(body.deltaY) || Math.abs(body.deltaY) > 4000) return reply({ error: "invalid_scroll" }, 400);
          await send("Input.dispatchMouseEvent", { type: "mouseWheel", x: 100, y: 100, deltaX: 0, deltaY: body.deltaY });
        } else if (operation === "type") {
          if (typeof body.text !== "string" || body.text.length > 8192) return reply({ error: "invalid_text" }, 400);
          if (typeof body.pageUrl !== "string" || body.pageUrl !== target.url) return reply({ error: "page_changed" }, 409);
          // Check destination and fill atomically in the trusted executor. CDP debug
          // buffers are cleared below and this expression never reaches a model.
          const result = await send("Runtime.evaluate", { returnByValue: true, userGesture: true, expression: `(() => {
            if (location.href !== ${JSON.stringify(body.pageUrl)}) return false;
            const element = document.activeElement;
            if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement || element?.isContentEditable)) return false;
            return document.execCommand("insertText", false, ${JSON.stringify(body.text)});
          })()` }) as { result?: { value?: unknown } };
          if (result.result?.value !== true) return reply({ error: "field_changed", message: "Tap the field again before entering private input. Embedded sign-in frames are not supported by private input yet." }, 409);
        } else if (operation === "key") {
          const keys: Record<string, number> = { Enter: 13, Tab: 9, Backspace: 8, Escape: 27, ArrowDown: 40, ArrowUp: 38, ArrowLeft: 37, ArrowRight: 39 };
          if (typeof body.key !== "string" || !Object.hasOwn(keys, body.key)) return reply({ error: "invalid_key" }, 400);
          // Chromium needs Enter's character payload to perform the default action
          // (form submission/newline), not just a keydown notification.
          await send("Input.dispatchKeyEvent", {
            type: body.key === "Enter" ? "keyDown" : "rawKeyDown",
            key: body.key, code: body.key, windowsVirtualKeyCode: keys[body.key],
            ...(body.key === "Enter" ? { text: "\r", unmodifiedText: "\r" } : {}),
          });
          await send("Input.dispatchKeyEvent", { type: "keyUp", key: body.key, code: body.key, windowsVirtualKeyCode: keys[body.key] });
        } else return reply({ error: "not_found" }, 404);
        return reply({ ok: true });
      } catch {
        // CDP errors can include input or provider URLs. Never serialize them.
        return reply({ error: "browser_operation_failed", message: "The page changed or the browser disconnected. Refresh and try again." }, 409);
      } finally { cdp?.clearDebugLog(); cdp?.disconnect(); }
    });
  }
}
