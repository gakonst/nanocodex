import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { once } from "node:events";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { createComputerTools } from "../index.mjs";

test("owned Chromium navigation, form input and screenshot through public CUA", {
  skip: !process.env.NANOCODEX_TEST_BROWSER, timeout: 40_000,
}, async t => {
  const profile = await mkdtemp(join(tmpdir(), "nanocodex-cua-browser-"));
  const server = createServer((_request, response) => {
    response.setHeader("content-type", "text/html; charset=utf-8");
    response.end('<!doctype html><title>Nanocodex browser fixture</title><label>Message <input id="message"></label><button onclick="document.getElementById(\'result\').textContent=\'Saved: \'+document.getElementById(\'message\').value">Save</button><p id="result">Waiting</p>');
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  const browser = spawn(process.env.NANOCODEX_TEST_BROWSER, [...(process.env.NANOCODEX_TEST_BROWSER_NO_SANDBOX === "1" ? ["--no-sandbox"] : []), "--headless", "--disable-background-networking", "--disable-sync", "--no-first-run", "--no-default-browser-check", "--password-store=basic", "--use-mock-keychain", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"], { stdio: "ignore" });
  let computer;
  t.after(async () => {
    await computer?.close();
    const closed = once(browser, "exit").catch(() => {});
    if (browser.exitCode === null && browser.signalCode === null) {
      browser.kill();
      await Promise.race([closed, delay(2000)]);
    }
    if (browser.exitCode === null && browser.signalCode === null) {
      browser.kill("SIGKILL");
      await Promise.race([closed, delay(2000)]);
    }
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  });
  let endpoint;
  for (let attempt = 0; attempt < 100; attempt++) {
    const value = await readFile(join(profile, "DevToolsActivePort"), "utf8").catch(() => "");
    const [port, path] = value.trim().split("\n");
    if (port && path) { endpoint = `ws://127.0.0.1:${port}${path}`; break; }
    if (browser.exitCode !== null) throw new Error("Owned browser exited before CDP became ready");
    await delay(100);
  }
  assert(endpoint, "Owned browser CDP endpoint is required");
  const executable = process.env.NANOCODEX_TEST_COMPUTER ?? fileURLToPath(new URL("../../../crates/experimental/nanocodex-computer/runtime/target/debug/nanocodex-computer", import.meta.url));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const preferences = join(profile, "computer-preferences.json");
  await writeFile(preferences, JSON.stringify({ defaultBrowser: "chrome", origins: { [origin]: "work" } }));
  computer = createComputerTools({ executable, args: ["--cdp", `chrome=${endpoint}`, "--cdp", `work=${endpoint}`, "--browser-preferences", preferences] });
  const context = { sessionId: "browser-fixture", callId: "browser-fixture", model: "test", signal: new AbortController().signal };
  const invoke = async code => {
    const result = await computer.tools[0].handler({ code }, context);
    assert.equal(result.success, true, JSON.stringify(result.value));
    return result;
  };
  await invoke(`let preferred = await cua.getBrowser({url:${JSON.stringify(origin + "/")}}); if (preferred.browserId !== "work") throw new Error("URL preference was ignored");`);
  await invoke(`let defaultBrowser = await cua.getBrowser(); if (defaultBrowser.browserId !== "chrome") throw new Error("Default preference was ignored");`);
  await invoke(`let tab = await cua.createBrowserTab("chrome", ${JSON.stringify(origin + "/")});`);
  await invoke(`await tab.playwright.getByRole("textbox", {name:"Message"}).fill("Nanocodex browser Ω"); await tab.playwright.getByRole("button", {name:"Save",exact:true}).click(); var state = await tab.getAXState({emit:false}); if (!state.includes("Saved: Nanocodex browser Ω")) throw new Error(state);`);
  const screenshot = await invoke("await nodeRepl.emitImage(await tab.getScreenshot({emit:false}));");
  assert(screenshot.output.some(item => item.type === "input_image" && item.detail === "original"));
  await invoke("await tab.close();");
});
