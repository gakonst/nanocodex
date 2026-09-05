import { afterEach, describe, expect, test } from "vitest";

import { startWebClient } from "../src/web-server.js";

let close: (() => Promise<void>) | undefined;
afterEach(async () => {
  await close?.();
  close = undefined;
});

describe("browser client", () => {
  test("serves a credential-free resumable actor UI with security headers", async () => {
    const web = await startWebClient({ port: 0 });
    close = web.close;
    const page = await fetch(web.url);
    expect(page.status).toBe(200);
    expect(page.headers.get("content-security-policy")).toContain("frame-ancestors 'none'");
    expect(await page.text()).toContain("Inference outlives the tab.");

    const script = await fetch(`${web.url}/dist/app.js`);
    const source = await script.text();
    expect(script.headers.get("content-type")).toContain("text/javascript");
    expect(source).toContain("localStorage");
    expect(source).toContain("URLSearchParams");
    expect(source).toContain("turnAccepted");
    expect(source).toContain("assistant.delta");
    expect(source).toContain("storage");
    expect(source).not.toContain("OPENAI_API_KEY");
    expect(source).not.toContain("CHATGPT_ACCESS_TOKEN");
  });
});
