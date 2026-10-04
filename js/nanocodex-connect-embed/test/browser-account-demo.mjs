import assert from "node:assert/strict";
import { mkdir, readFile, writeFile, rename } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import { build } from "esbuild";
import { chromium, expect } from "@playwright/test";
import { startBrowserFixture } from "./browser-server.mjs";
import { nanocodexTools } from "../../nanocodex-vite/tools.mjs";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const output = resolve(process.env.EMBED_BROWSER_OUTPUT ?? resolve(packageRoot, "../../output/connect-embed-account-demo"));
await mkdir(output, { recursive: true });
const bundle = await build({
  absWorkingDir: packageRoot, entryPoints: ["test/browser-account-demo.jsx"], bundle: true,
  write: false, outdir: "demo", platform: "browser", format: "esm", jsx: "automatic",
  alias: { react: resolve(packageRoot, "node_modules/react") },
  plugins: [{ name: "nanocodex-browser-resolution", setup(builder) {
    const compatibility = nanocodexTools();
    builder.onResolve({ filter: /.*/ }, args => {
      const path = compatibility.resolveId(args.path, args.importer);
      return path ? { path } : undefined;
    });
  } }],
  external: ["crypto", "fs", "stream", "net", "os", "path", "/paradigm-mark.svg"],
});
const html = `<!doctype html><html data-theme="dark"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Nanocodex SDK — app styling demo</title><link rel="stylesheet" href="/app.css"></head><body><div id="root"></div><script type="module" src="/app.js"></script></body></html>`;
const assets = new Map([
  ["/", { type: "text/html", body: html }],
  ["/paradigm-mark.svg", { type: "image/svg+xml", body: await readFile(resolve(packageRoot, "../account/public/paradigm-mark.svg")) }],
  ["/app.js", { type: "text/javascript", body: bundle.outputFiles.find(file => file.path.endsWith(".js")).text }],
  ["/app.css", { type: "text/css", body: bundle.outputFiles.find(file => file.path.endsWith(".css")).text }],
]);
const fixture = await startBrowserFixture(assets);
const browser = await chromium.launch({ headless: true, args: ["--no-sandbox"],
  ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE } : {}),
});
const context = await browser.newContext({ viewport: { width: 1200, height: 850 },
  recordVideo: { dir: resolve(output, "video"), size: { width: 1200, height: 850 } },
});
await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
const page = await context.newPage();
const errors = [];
page.on("pageerror", error => errors.push(String(error)));
const evidence = {
  command: "pnpm --filter nanocodex-connect-embed run demo:account",
  boundary: "Representative app-styled fixture, not a signed-in account session. Shipped AgentConversation + conversation.css, unchanged account index.css/AgentTerminal.css/Home.css, real managed SDK HTTP/SSE and controller. Synthetic remote agent replies/tools; no live model, account authorization, voice or host account controls exercised.",
  stylesheets: ["nanocodex-connect-embed/conversation.css", "js/account/src/index.css", "js/account/src/AgentTerminal.css", "js/account/src/Home.css"],
  assertions: [], styles: {},
};
const record = (name, observed) => { evidence.assertions.push({ name, observed }); console.log(`PASS ${name}: ${observed}`); };
const composer = page.getByRole("textbox");
async function appearance() {
  return page.evaluate(() => {
    const read = selector => {
      const s = getComputedStyle(document.querySelector(selector));
      return Object.fromEntries(["color", "backgroundColor", "fontFamily", "fontSize", "borderRadius", "padding", "maxWidth"].map(key => [key, s[key]]));
    };
    return { workspace: read(".chat-workspace"), user: read(".agent-terminal-user"), composer: read(".agent-touch-composer"), transcript: read(".agent-dom-transcript-inner") };
  });
}
try {
  await page.goto(fixture.origin);
  await expect(composer).toBeEnabled();
  await expect(page.getByText("What should we work on?", { exact: true })).toBeVisible();
  await page.screenshot({ path: resolve(output, "account-welcome-dark.png") });
  await page.waitForTimeout(900);
  await composer.pressSequentially("Compare the two synthetic records", { delay: 45 });
  await page.waitForTimeout(500);
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.locator(".agent-dom-transcript")).toContainText("Checking the local fixture");
  await page.waitForTimeout(1100);
  fixture.advance("alpha", "tool");
  await expect(page.locator(".agent-dom-transcript")).toContainText("fixture_lookup");
  await page.screenshot({ path: resolve(output, "account-streaming-dark.png") });
  await page.waitForTimeout(1400);
  fixture.advance("alpha", "final");
  await expect(page.locator(".agent-dom-transcript")).toContainText("Completed: Compare the two synthetic records");
  await expect(page.getByText("Completed: Compare the two synthetic records", { exact: true })).toHaveCount(1);
  const tool = page.locator(".agent-terminal-tool summary");
  await tool.click();
  await expect(page.locator(".agent-dom-transcript")).toContainText("Two synthetic records found");
  await page.waitForTimeout(1300);
  await page.screenshot({ path: resolve(output, "account-completed-dark.png") });
  evidence.styles.dark = await appearance();
  assert.equal(evidence.styles.dark.workspace.backgroundColor, "rgb(33, 33, 33)");
  assert.equal(evidence.styles.dark.user.borderRadius, "24px");
  assert.equal(evidence.styles.dark.transcript.fontSize, "16px");
  record("app styling and real streaming", "Account CSS supplies dark surface, 24px user bubble, 16px transcript and rounded composer. Typed submission, SSE commentary/tool, expanded result and single final answer visible.");
  await page.getByRole("button", { name: "Use light appearance" }).click();
  evidence.styles.light = await appearance();
  assert.equal(evidence.styles.light.workspace.backgroundColor, "rgb(255, 255, 255)");
  assert.equal(evidence.styles.light.user.borderRadius, "24px");
  await page.waitForTimeout(1400);
  await page.screenshot({ path: resolve(output, "account-completed-light.png") });
  record("existing app light appearance", "Account html[data-theme=light] rules supply white surface; conversation and composer remain usable.");
  await page.getByRole("button", { name: "Use dark appearance" }).click();
  await tool.click();
  await page.waitForTimeout(700);
  await composer.pressSequentially("Start another synthetic lookup", { delay: 40 });
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.getByRole("button", { name: /stop/i })).toBeVisible();
  await page.waitForTimeout(900);
  await page.getByRole("button", { name: /stop/i }).click();
  await expect.poll(() => fixture.requests.some(request => request.path.endsWith("/cancel") && request.status === 200)).toBe(true);
  await expect(composer).toBeEnabled();
  await page.waitForTimeout(900);
  record("cancel transport", "The rich composer Stop control sent POST /turns/:id/cancel and stayed usable after the synthetic terminal event.");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: resolve(output, "account-mobile-dark.png") });
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "Mobile horizontal overflow");
  await expect(composer).toBeVisible();
  record("mobile host styling", "390px viewport shows the app conversation/composer without horizontal overflow.");
  assert.deepEqual(errors, []);
  evidence.outcome = "passed";
} catch (error) {
  evidence.outcome = "failed";
  evidence.error = error.stack;
  await page.screenshot({ path: resolve(output, "failure.png") }).catch(() => {});
  throw error;
} finally {
  evidence.browserErrors = errors;
  evidence.requests = fixture.requests;
  evidence.transcript = await page.locator("body").innerText().catch(() => "page unavailable");
  await writeFile(resolve(output, "evidence.json"), JSON.stringify(evidence, null, 2));
  await context.tracing.stop({ path: resolve(output, "trace.zip") });
  const video = page.video();
  await context.close();
  if (video) await rename(await video.path(), resolve(output, "account-demo.webm"));
  await browser.close();
  await fixture.close();
  console.log(`App-styled demo evidence: ${output}`);
}
