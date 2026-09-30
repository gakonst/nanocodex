import { createObscuraBrowserBinding } from "../../src/obscura-browser.ts";
import { createBundledObscuraOptions } from "../../src/obscura-assets.ts";
import { loadCdpSpec } from "#obscura-sdk/spec";
import {
  createBrowserSession,
  connectBrowserSession,
  listBrowserTargets,
  deleteBrowserSession,
} from "#obscura-sdk/browser-run";
import { journey } from "./journey.mjs";
export default {
  async fetch(request, env) {
    const checks = [],
      requests = [],
      events = [];
    const assert = (name, actual, expected) => {
      const pass = JSON.stringify(actual) === JSON.stringify(expected);
      checks.push({ name, actual, expected, pass });
      if (!pass)
        throw new Error(
          `${name}: ${JSON.stringify(actual)} != ${JSON.stringify(expected)}`,
        );
    };
    const binding = createObscuraBrowserBinding(
      env.LOADER,
      createBundledObscuraOptions(env.NETWORK),
    );
    // Observe the real transport without modifying responses or CDP messages.
    const browser = {
      async fetch(input, init) {
        const req = new Request(input, init);
        const res = await binding.fetch(req);
        const entry = {
          method: req.method,
          path: new URL(req.url).pathname,
          query: new URL(req.url).search,
          status: res.status,
          sessionHeader: res.headers.get("cf-browser-session-id"),
        };
        requests.push(entry);
        if (req.method === "POST" && res.ok)
          entry.createdSession = (await res.clone().json()).sessionId;
        if (res.webSocket)
          res.webSocket.addEventListener("message", ({ data }) => {
            const m = JSON.parse(data);
            if (m.id === void 0) events.push(m);
          });
        return res;
      },
    };
    let session, info, other;
    try {
      const spec = await loadCdpSpec({ browser });
      assert(
        "SDK loadCdpSpec discovers Runtime.evaluate",
        spec.domains
          .flatMap((d) => d.commands)
          .some((c) => c.method === "Runtime.evaluate"),
        true,
      );
      assert(
        "SDK loadCdpSpec deletes its temporary session",
        requests.slice(0, 3).map((r) => r.method),
        ["POST", "GET", "DELETE"],
      );
      const specSession = requests[0].createdSession;
      assert(
        "temporary spec session is gone",
        (
          await browser.fetch(
            `https://localhost/v1/devtools/browser/${specSession}/json/list`,
          )
        ).status,
        404,
      );
      const n = requests.length;
      assert(
        "SDK spec cache returns same object",
        (await loadCdpSpec({ browser })) === spec,
        true,
      );
      assert("SDK spec cache avoids transport", requests.length, n);
      info = await createBrowserSession(browser, {
        includeTargets: true,
        keepAliveMs: 6e4,
      });
      assert("SDK creates session", typeof info.sessionId, "string");
      assert(
        "new session starts empty",
        await listBrowserTargets(browser, info.sessionId),
        [],
      );
      session = await connectBrowserSession(browser, info.sessionId, 15e3);
      assert("SDK session keeps ID", session.sessionId, info.sessionId);
      assert(
        "upgrade returns session header",
        requests.at(-1).sessionHeader,
        info.sessionId,
      );
      const call = async (method, params = {}, sessionId) => {
        const result = await session.send(method, params, { sessionId });
        if (method === "Page.navigate" && params.url.endsWith("/parent")) {
          for (let i = 0; i < 100; i++) {
            const ready = await session.send(
              "Runtime.evaluate",
              {
                expression:
                  'typeof scriptResult!=="undefined" && scriptResult.executed===1 && scriptResult.loaded===1',
                returnByValue: true,
              },
              { sessionId },
            );
            if (ready.result.value) break;
            await new Promise((r) => setTimeout(r, 10));
          }
        }
        return result;
      };
      await journey(call, events, checks);
      const { targetId: storageTarget } = await session.send(
        "Target.createTarget",
        { url: "https://fixture.example.com/storage-a" },
      );
      const storageSid = await session.attachToTarget(storageTarget);
      const storageEval = async (sid, expression) =>
        (
          await session.send(
            "Runtime.evaluate",
            { expression, returnByValue: true },
            { sessionId: sid },
          )
        ).result.value;
      await storageEval(
        storageSid,
        'document.cookie="sdkCookie=kept; Path=/; Secure; SameSite=Strict";localStorage.setItem("sdkLocal","kept-local");sessionStorage.setItem("sdkSession","kept-session")',
      );
      await session.send(
        "Page.navigate",
        { url: "https://fixture.example.com/storage-b" },
        { sessionId: storageSid },
      );
      assert(
        "document.cookie survives same-origin navigation",
        await storageEval(
          storageSid,
          'document.cookie.includes("sdkCookie=kept")',
        ),
        true,
      );
      assert(
        "localStorage survives same-origin navigation",
        await storageEval(storageSid, 'localStorage.getItem("sdkLocal")'),
        "kept-local",
      );
      assert(
        "sessionStorage survives same-origin navigation",
        await storageEval(storageSid, 'sessionStorage.getItem("sdkSession")'),
        "kept-session",
      );
      const { targetId: secondTarget } = await session.send(
        "Target.createTarget",
        { url: "https://fixture.example.com/storage-a" },
      );
      const secondSid = await session.attachToTarget(secondTarget);
      assert(
        "same-origin second tab shares cookie",
        await storageEval(
          secondSid,
          'document.cookie.includes("sdkCookie=kept")',
        ),
        true,
      );
      assert(
        "same-origin second tab shares localStorage",
        await storageEval(secondSid, 'localStorage.getItem("sdkLocal")'),
        "kept-local",
      );
      assert(
        "second tab sessionStorage starts isolated",
        await storageEval(secondSid, 'sessionStorage.getItem("sdkSession")'),
        null,
      );
      await storageEval(
        secondSid,
        'sessionStorage.setItem("sdkSession","second-tab")',
      );
      assert(
        "second tab sessionStorage does not change first",
        await storageEval(storageSid, 'sessionStorage.getItem("sdkSession")'),
        "kept-session",
      );
      await session.send("Target.closeTarget", { targetId: secondTarget });
      const { targetId: replacementTarget } = await session.send(
        "Target.createTarget",
        { url: "https://fixture.example.com/storage-a" },
      );
      const replacementSid = await session.attachToTarget(replacementTarget);
      assert(
        "replacement tab does not inherit closed tab storage",
        await storageEval(
          replacementSid,
          'sessionStorage.getItem("sdkSession")',
        ),
        null,
      );
      await session.send(
        "Page.navigate",
        { url: "https://child.example.com/storage" },
        { sessionId: storageSid },
      );
      assert(
        "cross-origin storage is isolated",
        await storageEval(
          storageSid,
          '[localStorage.getItem("sdkLocal"),sessionStorage.getItem("sdkSession"),document.cookie]',
        ),
        [null, null, ""],
      );
      await session.send("Target.closeTarget", { targetId: replacementTarget });
      await session.send("Target.closeTarget", { targetId: storageTarget });
      const { targetId } = await session.send("Target.createTarget", {
        url: "about:blank",
      });
      const attached = await session.attachToTarget(targetId);
      assert(
        "SDK attachToTarget gets flattened session",
        typeof attached,
        "string",
      );
      assert(
        "HTTP list uses SDK BrowserTargetInfo.id for live CDP target",
        (await listBrowserTargets(browser, info.sessionId)).map((t) => t.id),
        [targetId],
      );
      other = await createBrowserSession(browser);
      assert(
        "browser sessions are isolated",
        await listBrowserTargets(browser, other.sessionId),
        [],
      );
      await deleteBrowserSession(browser, other.sessionId);
      other = void 0;
      assert(
        "active session DELETE explicitly requires closing socket",
        (
          await browser.fetch(
            `https://localhost/v1/devtools/browser/${info.sessionId}`,
            { method: "DELETE" },
          )
        ).status,
        409,
      );
      session.close();
      session = void 0;
      for (let i = 0; i < 50; i++) {
        if ((await listBrowserTargets(browser, info.sessionId)).length === 0)
          break;
        await new Promise((r) => setTimeout(r, 10));
      }
      assert(
        "SDK close destroys remaining live targets",
        await listBrowserTargets(browser, info.sessionId),
        [],
      );
      await deleteBrowserSession(browser, info.sessionId);
      assert(
        "SDK delete removes browser session",
        (
          await browser.fetch(
            `https://localhost/v1/devtools/browser/${info.sessionId}/json/list`,
          )
        ).status,
        404,
      );
      await deleteBrowserSession(browser, info.sessionId);
      info = void 0;
      assert("SDK deletion is idempotent", true, true);
      return Response.json({
        ok: checks.every((c) => c.pass),
        sdk: "agents@0.22.0",
        integration:
          "upstream unmodified browser helpers -> actual managed createBundledObscuraOptions packaged asset factory -> actual managed createObscuraBrowserBinding -> WorkerLoader -> Obscura QuickJS + Rust DOM Wasm -> fixture Fetcher",
        checks,
        requests,
        events: events.map((e) => ({
          method: e.method,
          sessionId: e.sessionId,
        })),
        limitations: [
          "Helper API integration; BrowserConnector and full managed worker are not exercised.",
          "Fixture network, no rendering/screenshot claim.",
        ],
      });
    } catch (error) {
      return Response.json(
        {
          ok: false,
          error: String(error.stack || error),
          checks,
          requests,
          events,
        },
        { status: 500 },
      );
    } finally {
      session?.close();
      if (info)
        await deleteBrowserSession(browser, info.sessionId).catch(() => {});
      if (other)
        await deleteBrowserSession(browser, other.sessionId).catch(() => {});
    }
  },
};
