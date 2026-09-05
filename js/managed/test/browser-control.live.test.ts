import { env, runInDurableObject } from "cloudflare:test";
import { connectBrowserSession, createBrowserSession, deleteBrowserSession, listBrowserTargets, type BrowserBinding } from "agents/browser";
import { expect, it } from "vitest";
import { BrowserControl } from "../src/browser-control";
import type { DurableAgentSession } from "../src/index";
import type { ToolContext } from "nanocodex";

// Uses actual Cloudflare Chromium and DO storage, but no model, ChatGPT account,
// production agent, or real website credentials. The form lives only in this tab.
it("real cloud browser: private entry, screenshot, handoff, reconnect, and stale input", async () => {
  const bindings = env as unknown as {
    BROWSER: BrowserBinding;
    NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
  };
  const info = await createBrowserSession(bindings.BROWSER, { keepAliveMs: 60_000 });
  try {
    const cdp = await connectBrowserSession(bindings.BROWSER, info.sessionId);
    try {
      const target = await cdp.send("Target.createTarget", { url: "about:blank" }) as { targetId: string };
      const sessionId = await cdp.attachToTarget(target.targetId);
      const send = (method: string, params?: unknown) => cdp.send(method, params, { sessionId });
      const tree = await send("Page.getFrameTree") as { frameTree: { frame: { id: string } } };
      await send("Page.setDocumentContent", { frameId: tree.frameTree.frame.id, html: `<!doctype html><title>Cloud browser test</title>
        <style>body{font:20px sans-serif;padding:24px}input,button{display:block;margin:16px;padding:12px}</style>
        <form onsubmit="event.preventDefault();document.getElementById('result').textContent=document.getElementById('secret').value==='synthetic-test-value'?'Signed in':'Try again'">
        <label>Test secret<input id="secret" type="password"></label><button id="submit">Continue</button></form><p id="result">Waiting</p>` });
      const stub = bindings.NANOCODEX_SESSIONS.getByName(crypto.randomUUID());
      await runInDurableObject(stub, async (_instance, state) => {
        const sessionInfo = async () => ({ sessionId: info.sessionId, targets: await listBrowserTargets(bindings.BROWSER, info.sessionId) });
        const control = new BrowserControl(state.storage, bindings.BROWSER, sessionInfo);
        const request = (operation: string, body: unknown = {}) => control.request(new Request(`https://session.internal/browser/${operation}`, { method: "POST", body: JSON.stringify(body) }));
        const view = () => control.request(new Request("https://session.internal/browser"));
        const signal = new AbortController().signal;
        const context = { callId: crypto.randomUUID(), signal } as ToolContext;
        const paused = control.handoff("Complete the synthetic form", context);
        const taken = await (await view()).json() as { mode: string; generation: string };
        expect(taken.mode).toBe("human");
        let modelRan = false;
        const waitingModel = control.model(context, async () => { modelRan = true; return "resumed"; });
        const frame = await request("frame", { generation: taken.generation, target: target.targetId });
        expect(frame.status).toBe(200);
        const screenshot = await frame.json() as { data: string; width: number; height: number };
        expect(atob(screenshot.data).slice(0, 2)).toBe("\xff\xd8");
        expect(screenshot.width).toBeGreaterThan(0);
        expect(screenshot.height).toBeGreaterThan(0);
        const field = await send("Runtime.evaluate", { returnByValue: true, expression: `(() => {const r=document.getElementById('secret').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()` }) as { result: { value: { x: number; y: number } } };
        expect((await request("click", { generation: taken.generation, target: target.targetId, ...field.result.value })).status).toBe(200);
        expect((await request("type", { generation: taken.generation, target: target.targetId, pageUrl: "https://wrong.example/", text: "do-not-fill" })).status).toBe(409);
        expect((await request("type", { generation: taken.generation, target: target.targetId, pageUrl: "about:blank", text: "synthetic-test-value" })).status).toBe(200);
        expect((await request("key", { generation: taken.generation, target: target.targetId, key: "Enter" })).status).toBe(200);
        const result = await send("Runtime.evaluate", { returnByValue: true, expression: "document.getElementById('result').textContent" }) as { result: { value: unknown } };
        expect(result.result.value).toBe("Signed in");
        expect(modelRan).toBe(false);
        const restored = new BrowserControl(state.storage, bindings.BROWSER, sessionInfo);
        const restoredState = await (await restored.request(new Request("https://session.internal/browser"))).json() as { mode: string; generation: string };
        expect(restoredState.generation).toBe(taken.generation);
        expect(restoredState.mode).toBe("human");
        expect(JSON.stringify([...await state.storage.list()])).not.toContain("synthetic-test-value");
        expect((await request("release", { generation: taken.generation })).status).toBe(200);
        expect(await paused).toMatchObject({ status: "returned_to_agent" });
        expect(await waitingModel).toBe("resumed");
        expect((await request("type", { generation: taken.generation, target: target.targetId, pageUrl: "about:blank", text: "stale" })).status).toBe(409);
      });
    } finally { cdp.clearDebugLog(); cdp.disconnect(); }
  } finally { await deleteBrowserSession(bindings.BROWSER, info.sessionId); }
}, 120_000);
