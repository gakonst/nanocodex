import { afterEach, describe, expect, test } from "vitest";

import { startWebClient } from "../src/web-server.js";

let close: (() => Promise<void>) | undefined;
afterEach(async () => {
  await close?.();
  close = undefined;
});

describe("browser client", () => {
  test("serves the browser client with security headers", async () => {
    const web = await startWebClient({ port: 0 });
    close = web.close;
    const page = await fetch(web.url);
    expect(page.status).toBe(200);
    expect(page.headers.get("content-security-policy")).toContain("frame-ancestors 'none'");

    const script = await fetch(`${web.url}/dist/app.js`);
    expect(script.headers.get("content-type")).toContain("text/javascript");
  });
});
