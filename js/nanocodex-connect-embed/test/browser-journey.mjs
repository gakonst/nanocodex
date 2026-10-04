import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import { build } from "esbuild";
import { chromium, expect } from "@playwright/test";
import { startBrowserFixture } from "./browser-server.mjs";
import { nanocodexTools } from "../../nanocodex-vite/tools.mjs";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const output = resolve(process.env.EMBED_BROWSER_OUTPUT ?? resolve(packageRoot, "../../output/connect-embed-browser"));
await mkdir(output, { recursive: true });
const bundle = await build({
  absWorkingDir: packageRoot, entryPoints: ["test/browser-app.jsx"], bundle: true,
  write: false, platform: "browser", format: "esm", jsx: "automatic", sourcemap: "inline",
  // Workspace SDK devDependencies use another React version. A consuming app
  // supplies one peer instance (equivalent to Vite resolve.dedupe).
  alias: { react: resolve(packageRoot, "node_modules/react") },
  // Rich conversation shares the SDK browser setup used by account/playground.
  // Preserve the production browser replacements, not test-only module mocks.
  plugins: [{ name: "nanocodex-browser-resolution", setup(builder) {
    const compatibility = nanocodexTools();
    builder.onResolve({ filter: /.*/ }, args => {
      const path = compatibility.resolveId(args.path, args.importer);
      return path ? { path } : undefined;
    });
  } }],
  external: ["crypto", "fs", "stream", "net", "os", "path"],
});
const html = `<!doctype html><html><head><meta charset="utf-8"><title>Embed browser journey</title></head><body><div id="root"></div><script type="module" src="/app.js"></script></body></html>`;
const assets = new Map([
  ["/", { type: "text/html", body: html }],
  ["/app.js", { type: "text/javascript", body: bundle.outputFiles[0].text }],
  ["/styles.css", { type: "text/css", body: await readFile(resolve(packageRoot, "styles.css")) }],
  ["/themes.css", { type: "text/css", body: await readFile(resolve(packageRoot, "themes.css")) }],
]);
const fixture = await startBrowserFixture(assets);
const browser = await chromium.launch({
  headless: true,
  ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE } : {}),
  args: ["--no-sandbox"],
});
const context = await browser.newContext({ viewport: { width: 1100, height: 900 } });
await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
const page = await context.newPage();
const failures = [];
const evidence = { command: "pnpm --filter nanocodex-connect-embed run test:browser", boundary: "Local synthetic agent HTTP/SSE service; real SDK transport, managed source, React controller and public primitives", assertions: [], styles: {} };
page.on("pageerror", error => failures.push(String(error)));
const record = (name, detail) => { evidence.assertions.push({ name, observed: detail }); console.log(`PASS ${name}: ${detail}`); };
const log = page.getByRole("log", { name: "Conversation" });
const status = page.locator('[data-agent-part="status"]');
const message = page.getByRole("textbox", { name: "Message", exact: true });
async function prompt(text) {
  await message.fill(text);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(log).toContainText(text);
}
async function styles() {
  return page.evaluate(() => {
    const properties = ["display", "color", "backgroundColor", "fontFamily", "fontSize", "padding", "borderRadius", "borderWidth", "width", "minHeight"];
    const read = selector => {
      const style = getComputedStyle(document.querySelector(selector));
      return Object.fromEntries(properties.map(name => [name, style[name]]));
    };
    return { embed: read("[data-agent-embed]"), hostButton: read("#host button"), hostTextarea: read("#host textarea"), hostParagraph: read("#host p") };
  });
}
try {
  await page.goto(fixture.origin);
  await expect(log).toContainText("alpha recent answer");
  await expect(log).not.toContainText("alpha older answer");
  await page.getByRole("button", { name: "Load earlier messages" }).click();
  await expect(log).toContainText("alpha older answer");
  await expect.poll(() => fixture.requests.some(request => request.query.includes("before=3"))).toBe(true);
  record("history pagination", "Recent history loaded over HTTP; Load earlier messages retrieved older alpha answer using before=3");

  assert.equal(await page.locator('link[rel="stylesheet"], style').count(), 0);
  evidence.styles.unstyled = await styles();
  assert.equal(evidence.styles.unstyled.embed.display, "block");
  await page.screenshot({ path: resolve(output, "unstyled.png"), fullPage: true });
  record("no CSS", "Public primitive imports inserted no style or stylesheet elements; native controls and transcript rendered");

  await prompt("show streamed result");
  await expect(log).toContainText("Checking the local fixture");
  await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeEnabled();
  await expect(log).not.toContainText("Completed: show streamed result");
  record("prompt and live stream", "Typed prompt submitted over POST /turns; partial SSE message visible before completion");
  fixture.advance("alpha", "tool");
  await expect(page.locator('[data-agent-part="activity"]')).toContainText("fixture_lookup — running");
  await page.screenshot({ path: resolve(output, "streaming-tool.png"), fullPage: true });
  fixture.advance("alpha", "final");
  await expect(log).toContainText("Completed: show streamed result");
  await expect(status).toContainText("Ready");
  await expect(page.locator('[data-agent-part="activity"]')).toContainText("fixture_lookup — completed");
  await page.getByText("fixture_lookup — completed", { exact: true }).click();
  await expect(page.locator('[data-agent-part="activity"] pre').filter({ hasText: "Two synthetic records found" })).toBeVisible();
  assert.equal(await log.getByText("Completed: show streamed result", { exact: true }).count(), 1);
  record("tool and final", "Tool running/completed states and output visible; exactly one final answer; controller returns Ready");

  await prompt("cancel this turn");
  await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect(status).toContainText("Cancelled");
  assert(fixture.requests.some(request => request.method === "POST" && request.path.endsWith("/cancel")));
  record("cancel", "Stop sent real cancel request and authoritative SSE terminal produced Cancelled");

  await prompt("fail this turn");
  fixture.advance("alpha", "error");
  await expect(page.getByRole("alert")).toContainText("Synthetic agent failure");
  await expect(status).toContainText("Turn failed");
  await prompt("recover from error");
  fixture.advance("alpha", "tool");
  fixture.advance("alpha", "final");
  await expect(log).toContainText("Completed: recover from error");
  await expect(status).toContainText("Ready");
  record("error recovery", "Remote turn failure is an accessible alert; next submitted turn completes normally");

  await prompt("detached alpha turn");
  await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeEnabled();
  await message.fill("alpha unsent draft");
  await page.getByRole("combobox", { name: "Agent", exact: true }).selectOption("beta");
  await expect(log).toContainText("beta recent answer");
  await expect(log).not.toContainText("alpha");
  await expect(log).not.toContainText("recover from error");
  await expect(message).toHaveValue("");
  fixture.advance("alpha", "tool");
  fixture.advance("alpha", "final");
  record("agent switch", "Switching an active source clears alpha transcript and unsent draft, loads beta history, and detaches from alpha completion");

  const beforeHistoryOff = fixture.requests.length;
  await page.getByRole("checkbox", { name: "Include history" }).uncheck();
  await expect(log).toContainText("No messages yet.");
  await expect(page.getByRole("button", { name: "Load earlier messages" })).toHaveCount(0);
  await prompt("history disabled prompt");
  fixture.advance("beta", "tool");
  fixture.advance("beta", "final");
  await expect(log).toContainText("Completed: history disabled prompt");
  await expect(log).not.toContainText("beta recent answer");
  await expect(log).not.toContainText("detached alpha turn");
  assert(!fixture.requests.slice(beforeHistoryOff).some(request => request.path.endsWith("/events/history")));
  record("history disabled", "No HTTP history read or Load earlier control; source exposes its submitted prompt and answer only");

  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await expect(status).toContainText("Not connected");
  await expect(message).toBeDisabled();
  await expect(log).not.toContainText("history disabled prompt");
  record("disconnect", "Composer disabled and previous source transcript released");

  await page.getByRole("button", { name: "Connect", exact: true }).click();
  await page.addStyleTag({ url: fixture.origin + "/styles.css" });
  await page.addStyleTag({ url: fixture.origin + "/themes.css" });
  await page.locator("[data-agent-embed]").evaluate(element => element.dataset.theme = "light");
  evidence.styles.light = await styles();
  assert.equal(evidence.styles.light.embed.display, "flex");
  assert.equal(evidence.styles.light.embed.backgroundColor, "rgb(255, 255, 255)");
  for (const key of ["hostButton", "hostTextarea", "hostParagraph"]) assert.deepEqual(evidence.styles.light[key], evidence.styles.unstyled[key], `light CSS leaked to ${key}`);
  await page.screenshot({ path: resolve(output, "light-theme.png"), fullPage: true });
  await page.locator("[data-agent-embed]").evaluate(element => element.dataset.theme = "dark");
  evidence.styles.dark = await styles();
  assert.equal(evidence.styles.dark.embed.backgroundColor, "rgb(23, 23, 23)");
  assert.equal(evidence.styles.dark.embed.color, "rgb(250, 250, 250)");
  for (const key of ["hostButton", "hostTextarea", "hostParagraph"]) assert.deepEqual(evidence.styles.dark[key], evidence.styles.unstyled[key], `dark CSS leaked to ${key}`);
  await prompt("dark theme prompt");
  fixture.advance("beta", "tool");
  fixture.advance("beta", "final");
  await expect(log).toContainText("Completed: dark theme prompt");
  await page.screenshot({ path: resolve(output, "dark-theme.png"), fullPage: true });
  record("scoped themes", "Explicit light/dark CSS changes embed palette and layout; host button/textarea/paragraph computed styles identical; dark-mode prompt completes");

  await page.goto(fixture.origin + "/?assembled=true&theme=light");
  await page.addStyleTag({ url: fixture.origin + "/styles.css" });
  await page.addStyleTag({ url: fixture.origin + "/themes.css" });
  await expect(log).toContainText("Completed: detached alpha turn");
  await prompt("assembled embed prompt");
  fixture.advance("alpha", "tool");
  fixture.advance("alpha", "final");
  await expect(log).toContainText("Completed: assembled embed prompt");
  await page.screenshot({ path: resolve(output, "assembled-embed.png"), fullPage: true });
  record("assembled embed", "Public AgentEmbed convenience component loads durable history and submits/completes a prompt");
  await prompt("hold assembled turn");
  await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeEnabled();
  const rootsBeforeSteer = fixture.requests.filter(request => request.method === "POST" && request.path.endsWith("/turns")).length;
  await message.fill("unavailable steering");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Synthetic steering service unavailable");
  await expect(message).toHaveValue("unavailable steering");
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeEnabled();
  assert(fixture.requests.some(request => request.path.endsWith("/steer") && request.status === 503));
  assert.equal(fixture.requests.filter(request => request.method === "POST" && request.path.endsWith("/turns")).length, rootsBeforeSteer);
  record("HTTP failure retains draft", "Real SDK steering HTTP503 produced an accessible error and retained editable draft; no new root submission or automatic retry");
  await page.screenshot({ path: resolve(output, "retained-failed-steer.png"), fullPage: true });
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect(status).toContainText("Cancelled");
  await message.fill("assembled unsent draft");
  await page.getByRole("combobox", { name: "Agent", exact: true }).selectOption("beta");
  await expect(log).toContainText("Completed: dark theme prompt");
  await expect(log).not.toContainText("hold assembled turn");
  await expect(log).not.toContainText("assembled embed prompt");
  await expect(message).toHaveValue("");
  record("assembled source switch", "AgentEmbed switched alpha to beta; old transcript and unsent draft disappeared");
  for (const authorization of ["revoked", "mismatch"]) {
    const before = fixture.requests.length;
    await page.goto(fixture.origin + "/?rich=true&auth=" + authorization);
    await expect(page.getByLabel("Connect state")).toHaveText("error");
    await expect(page.getByRole("region", { name: "Connect conversation" })).toContainText(
      authorization === "revoked" ? "no longer active" : "does not belong");
    assert.equal(fixture.requests.length, before, `${authorization} connection sent an agent request`);
    record(`Connect ${authorization}`, "Rendered authorization error with zero agent HTTP requests");
  }
  await page.goto(fixture.origin + "/?rich=true");
  await expect(page.getByLabel("Connect state")).toHaveText("ready");
  const rich = page.getByRole("region", { name: "Connect conversation" });
  await expect(rich).toContainText("Completed: assembled embed prompt");
  await page.getByRole("checkbox", { name: "Grant history", exact: true }).uncheck();
  await expect(rich).not.toContainText("Completed: assembled embed prompt");
  record("Connect visibility remount", "Turning off grant history immediately discarded the previous transcript");
  await page.getByRole("checkbox", { name: "Show tool calls", exact: true }).uncheck();
  await rich.getByRole("textbox").fill("hidden tool details");
  await rich.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(rich).toContainText("Checking the local fixture");
  fixture.advance("alpha", "tool");
  fixture.advance("alpha", "final");
  await expect(rich).toContainText("Completed: hidden tool details");
  await expect(rich).not.toContainText("fixture_lookup");
  record("Connect showToolCalls false", "Prompt and final rendered while tool invocation name/details remained hidden");
  await page.getByRole("checkbox", { name: "Show tool calls", exact: true }).check();
  await expect(rich).toContainText("fixture_lookup");
  await page.getByRole("checkbox", { name: "Grant raw traces", exact: true }).uncheck();
  await rich.getByRole("textbox").fill("grant hides tool details");
  await rich.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(rich).toContainText("Checking the local fixture");
  fixture.advance("alpha", "tool");
  fixture.advance("alpha", "final");
  await expect(rich).toContainText("Completed: grant hides tool details");
  await expect(rich).not.toContainText("fixture_lookup");
  await page.screenshot({ path: resolve(output, "connect-grant-visibility.png"), fullPage: true });
  record("Connect rawTraces false", "Grant visibility hides tool invocation even when host showToolCalls is true");
  assert.deepEqual(failures, [], "Browser runtime errors");
  evidence.outcome = "passed";
} catch (error) {
  evidence.outcome = "failed";
  evidence.error = error.stack;
  await page.screenshot({ path: resolve(output, "failure.png"), fullPage: true }).catch(() => {});
  throw error;
} finally {
  evidence.browserErrors = failures;
  evidence.requests = fixture.requests;
  evidence.transcript = await page.locator("body").innerText().catch(() => "page unavailable");
  await writeFile(resolve(output, "evidence.json"), JSON.stringify(evidence, null, 2));
  await context.tracing.stop({ path: resolve(output, "trace.zip") });
  await browser.close();
  await fixture.close();
  console.log(`Browser evidence: ${output}`);
}
