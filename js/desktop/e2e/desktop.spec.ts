import {
  test,
  expect,
  _electron as electron,
  type Page,
} from "@playwright/test";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { once } from "node:events";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { parseEnv } from "node:util";
import { Agent } from "nanocodex/managed";
import { timeline } from "../src/renderer/timeline";

async function credentials() {
  let dotenv: Record<string, string> = {};
  try {
    dotenv = parseEnv(await readFile(resolve("../../.env"), "utf8"));
  } catch {}
  return {
    apiKey:
      process.env.NANOCODEX_API_KEY ||
      process.env.NC_API_KEY ||
      dotenv.NANOCODEX_API_KEY ||
      dotenv.NC_API_KEY,
    baseUrl:
      process.env.NANOCODEX_MANAGED_URL ||
      dotenv.NANOCODEX_MANAGED_URL ||
      "https://nanocodex.gakonst.workers.dev",
  };
}

async function removeTestAgent(
  id: string,
  options: Awaited<ReturnType<typeof credentials>>
) {
  try {
    await Agent.remove(id, options);
  } catch (error) {
    // Deletion can finish even when the Worker loses its final response.
    const response = await fetch(new URL(`/v1/agents/${id}`, options.baseUrl), {
      headers: { authorization: `Bearer ${options.apiKey}` },
    });
    if (response.status !== 404) throw error;
  }
}

async function launch(
  directory: string,
  apiKey?: string,
  options: { env?: Record<string, string>; onboarding?: boolean } = {}
) {
  const started = performance.now();
  const application = await electron.launch({
    ...(process.env.NANOCODEX_TEST_EXECUTABLE
      ? { executablePath: process.env.NANOCODEX_TEST_EXECUTABLE, args: [] }
      : { args: [resolve("dist/main/index.js")] }),
    env: {
      ...process.env,
      ...(apiKey ? { NC_API_KEY: apiKey } : {}),
      NANOCODEX_DESKTOP_DATA: join(directory, "app-data"),
      NANOCODEX_DESKTOP_TEST: "1",
      ...options.env,
    },
  });
  const page = await application.firstWindow();
  await page.setViewportSize({ width: 1360, height: 850 });
  expect(
    await application.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows().every((window) => !window.isVisible())
    )
  ).toBe(true);
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("requestfailed", (request) =>
    errors.push(`${request.url()}: ${request.failure()?.errorText}`)
  );
  await expect(
    page.getByRole("textbox", {
      name: options.onboarding ? "Phone number" : "Message Nanocodex",
    })
  ).toBeVisible();
  return {
    application,
    page,
    errors,
    startupMs: Math.round(performance.now() - started),
  };
}

async function renameTab(page: Page, name: string) {
  await page.getByRole("tab", { selected: true }).dblclick();
  await page.getByRole("textbox", { name: "Tab name" }).fill(name);
  await page.getByRole("textbox", { name: "Tab name" }).press("Enter");
  await expect(page.getByRole("tab", { selected: true })).toContainText(name);
}

test("tabs preserve drafts, placement and theme with native shortcuts", async ({}, testInfo) => {
  const account = await credentials();
  test.skip(!account.apiKey, "A managed test account is required for the signed-in workspace.");
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-desktop-tabs-"));
  const { application, page, errors, startupMs } = await launch(directory, account.apiKey, { env: { NANOCODEX_MANAGED_URL: account.baseUrl } });
  try {
    await expect(page).toHaveTitle("Nanocodex");
    await expect(page.getByRole("tab")).toHaveCount(1);
    await renameTab(page, "Build");
    const composer = page.getByRole("textbox", { name: "Message Nanocodex" });
    const inputStarted = performance.now();
    await composer.fill("Keep this draft in the Build tab.");
    await expect(composer).toHaveValue("Keep this draft in the Build tab.");
    const inputMs = Math.round(performance.now() - inputStarted);
    await composer.press("Meta+t");
    await expect(page.getByRole("tab")).toHaveCount(2);
    await expect(composer).toHaveValue("");
    await renameTab(page, "Review");
    await composer.fill("A separate draft for Review.");
    const switchStarted = performance.now();
    await composer.press("Meta+1");
    await expect(composer).toHaveValue("Keep this draft in the Build tab.");
    const switchMs = Math.round(performance.now() - switchStarted);
    await page
      .getByRole("button", { name: "Move tabs to top", exact: true })
      .click();
    await expect(page.getByRole("tablist")).toHaveAttribute(
      "aria-orientation",
      "horizontal"
    );
    await page.reload();
    await expect(page.getByRole("tab")).toHaveCount(2);
    await expect(page.getByRole("tablist")).toHaveAttribute(
      "aria-orientation",
      "horizontal"
    );
    await expect(composer).toHaveValue("Keep this draft in the Build tab.");
    await composer.press("Meta+2");
    await expect(composer).toHaveValue("A separate draft for Review.");
    await composer.press("Meta+w");
    await expect(page.getByRole("tab")).toHaveCount(1);
    await expect(composer).toHaveValue("Keep this draft in the Build tab.");
    await composer.press("Meta+Shift+t");
    await expect(page.getByRole("tab")).toHaveCount(2);
    await expect(composer).toHaveValue("A separate draft for Review.");
    await page
      .getByRole("tab", { name: /Review/ })
      .dragTo(page.getByRole("tab", { name: /Build/ }));
    await expect(page.getByRole("tab").first()).toContainText("Review");
    await page.screenshot({ path: testInfo.outputPath("tabs-top.png") });
    await composer.press("Meta+,");
    await page.getByRole("button", { name: "Dark", exact: true }).click();
    await page.getByRole("button", { name: "Sidebar", exact: true }).click();
    await page.getByRole("tab", { name: /Review/ }).click();
    await expect(page.getByRole("tablist")).toHaveAttribute(
      "aria-orientation",
      "vertical"
    );
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await page.screenshot({
      path: testInfo.outputPath("tabs-sidebar-dark.png"),
    });
    await page.reload();
    await expect(page.getByRole("tab").first()).toContainText("Review");
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await expect(composer).toHaveValue("A separate draft for Review.");
    await composer.press("Meta+,");
    await page.screenshot({ path: testInfo.outputPath("settings-dark.png") });
    await page.getByRole("button", { name: "Hands", exact: true }).click();
    await page.screenshot({ path: testInfo.outputPath("hands-dark.png") });
    await page.keyboard.press("Meta+,");
    await page.getByRole("button", { name: "Light", exact: true }).click();
    await page.screenshot({ animations: "disabled", path: testInfo.outputPath("settings-light.png") });
    await page.getByRole("button", { name: "Hands", exact: true }).click();
    await page.screenshot({ path: testInfo.outputPath("hands-light.png") });
    await page.getByRole("tab").first().click();
    await page.locator(".model-trigger").click();
    await page.screenshot({ path: testInfo.outputPath("composer-models.png") });
    expect(errors).toEqual([]);
    expect(startupMs).toBeLessThan(8_000);
    expect(inputMs).toBeLessThan(500);
    expect(switchMs).toBeLessThan(500);
    console.info("Desktop responsiveness", { startupMs, inputMs, switchMs });
    await testInfo.attach("responsiveness.json", {
      body: JSON.stringify({ startupMs, inputMs, switchMs }),
      contentType: "application/json",
    });
  } finally {
    await application.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test("phone onboarding validates codes, resends, and restores its secure sign-in", async ({}, testInfo) => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-desktop-otp-"));
  const environmentFile = join(directory, "empty.env");
  await writeFile(environmentFile, "");
  const key = `ncx_live_${"e".repeat(12)}_${"k".repeat(43)}`;
  const session = `nanocodex_account=s_${"s".repeat(43)}`;
  const challenge = "c".repeat(43);
  const calls: {
    path: string;
    method: string;
    body: Record<string, unknown>;
    authorization?: string;
    cookie?: string;
  }[] = [];
  const server = createServer(async (request, response) => {
    let encoded = "";
    for await (const chunk of request) encoded += chunk;
    const body = encoded ? JSON.parse(encoded) : {};
    const path = request.url!.split("?")[0];
    calls.push({
      path,
      method: request.method!,
      body,
      authorization: request.headers.authorization,
      cookie: request.headers.cookie,
    });
    response.setHeader("content-type", "application/json");
    if (path === "/v1/auth/sms/start") {
      response.writeHead(202);
      response.end(
        JSON.stringify({
          challenge_id: challenge,
          expires_in: 600,
          resend_after: 2,
        })
      );
    } else if (path === "/v1/auth/sms/verify") {
      if (body.code !== "123456") {
        response.writeHead(400);
        response.end(JSON.stringify({ error: "invalid_or_expired_otp" }));
      } else {
        response.writeHead(200, {
          "set-cookie": `${session}; Path=/; HttpOnly; SameSite=Lax`,
        });
        response.end(JSON.stringify({ user: { persistent: true } }));
      }
    } else if (path === "/v1/api-keys" && request.method === "POST") {
      response.writeHead(201);
      response.end(
        JSON.stringify({ api_key: key, key: { id: "e".repeat(12) } })
      );
    } else if (path === "/v1/agents") {
      response.end(JSON.stringify({ data: [] }));
    } else if (path === "/v1/auth/logout") {
      response.writeHead(204);
      response.end();
    } else {
      response.writeHead(404);
      response.end(JSON.stringify({ error: "fixture_route_not_found" }));
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("Fixture did not listen");
  const baseUrl = `http://127.0.0.1:${address.port}`;
  const env = {
    NANOCODEX_ENV_FILE: environmentFile,
    NC_API_KEY: "",
    NANOCODEX_API_KEY: "",
    NANOCODEX_MANAGED_URL: baseUrl,
  };
  let application:
    | Awaited<ReturnType<typeof launch>>["application"]
    | undefined;
  try {
    const first = await launch(directory, undefined, { env, onboarding: true });
    application = first.application;
    const { page, errors } = first;
    const rendererRequests: string[] = [];
    page.on("request", (request) => rendererRequests.push(request.url()));
    await expect(
      page.getByRole("heading", { name: "Welcome to Nanocodex" })
    ).toBeVisible();
    await expect(
      page.getByRole("textbox", { name: "Nanocodex API key" })
    ).not.toBeVisible();
    await page.screenshot({
      path: testInfo.outputPath("onboarding-phone.png"),
    });
    const phone = page.getByRole("textbox", { name: "Phone number" });
    await phone.fill("555");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("country code");
    expect(calls).toHaveLength(0);
    await phone.fill("+1 (415) 555-0123");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Check your messages" })
    ).toBeVisible();
    const code = page.getByRole("textbox", { name: "Verification code" });
    await expect(code).toBeFocused();
    await expect(
      page.getByRole("button", { name: /^Resend code in/ })
    ).toBeDisabled();
    await page.screenshot({ path: testInfo.outputPath("onboarding-code.png") });
    await expect(
      page.getByRole("button", { name: "Resend code", exact: true })
    ).toBeEnabled({ timeout: 5_000 });
    await page
      .getByRole("button", { name: "Resend code", exact: true })
      .click();
    await expect
      .poll(() => calls.filter((call) => call.path.endsWith("/start")).length)
      .toBe(2);
    await page
      .getByRole("button", { name: "Change number", exact: true })
      .click();
    await expect(phone).toHaveValue("+14155550123");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await code.fill("000000");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText(
      "incorrect or has expired"
    );
    await expect(code).toHaveValue("000000");
    await code.fill("123456");
    const blockedStore = join(directory, "app-data", "desktop.json.tmp");
    await mkdir(blockedStore);
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText(
      "could not save your sign-in"
    );
    await expect(code).toHaveValue("123456");
    await rm(blockedStore, { recursive: true });
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(
      page.getByText("Managed agents", { exact: true })
    ).toBeVisible();
    await expect(
      page.getByRole("textbox", { name: "Message Nanocodex" })
    ).toBeVisible();
    await expect
      .poll(
        () => calls.filter((call) => call.path === "/v1/auth/logout").length
      )
      .toBe(1);
    expect(calls.filter((call) => call.path === "/v1/api-keys")).toHaveLength(
      1
    );
    expect(
      calls.filter(
        (call) => call.path.endsWith("/verify") && call.body.code === "123456"
      )
    ).toHaveLength(1);
    expect(calls.find((call) => call.path === "/v1/api-keys")?.cookie).toBe(
      session
    );
    expect(
      calls.find((call) => call.path === "/v1/agents")?.authorization
    ).toBe(`Bearer ${key}`);
    const exposed = await page.evaluate(async () =>
      JSON.stringify({
        state: await window.nanocodex.state(),
        localStorage: { ...localStorage },
        sessionStorage: { ...sessionStorage },
        cookie: document.cookie,
        text: document.body.innerText,
      })
    );
    for (const privateValue of [key, session, challenge])
      expect(exposed).not.toContain(privateValue);
    expect(rendererRequests.some((url) => url.startsWith(baseUrl))).toBe(false);
    const saved = await readFile(
      join(directory, "app-data", "desktop.json"),
      "utf8"
    );
    expect(JSON.parse(saved).connection).toEqual(expect.any(String));
    for (const privateValue of [key, session, challenge, "+14155550123"])
      expect(saved).not.toContain(privateValue);
    expect(errors).toEqual([]);
    await application.close();
    application = undefined;
    const restored = await launch(directory, undefined, { env });
    application = restored.application;
    await expect(
      restored.page.getByText("Managed agents", { exact: true })
    ).toBeVisible();
    await expect(
      restored.page.getByRole("heading", { name: "Welcome to Nanocodex" })
    ).not.toBeVisible();
    expect(calls.filter((call) => call.path.endsWith("/start"))).toHaveLength(
      3
    );
    await restored.page
      .getByRole("textbox", { name: "Message Nanocodex" })
      .fill("Keep this draft while I check account settings.");
    await restored.page
      .getByRole("textbox", { name: "Message Nanocodex" })
      .press("Meta+,");
    await restored.page
      .getByRole("button", { name: "Switch account", exact: true })
      .click();
    await expect(
      restored.page.getByRole("textbox", { name: "Phone number" })
    ).toBeVisible();
    await restored.page
      .getByRole("button", { name: "Cancel", exact: true })
      .click();
    await expect(
      restored.page.getByText("Connected to Nanocodex", { exact: true })
    ).toBeVisible();
    await restored.page.getByRole("tab").first().click();
    await expect(
      restored.page.getByRole("textbox", { name: "Message Nanocodex" })
    ).toHaveValue("Keep this draft while I check account settings.");
    expect(restored.errors).toEqual([]);
  } finally {
    await application?.close();
    await new Promise<void>((done) => {
      server.closeAllConnections();
      server.close(() => done());
    });
    await rm(directory, { recursive: true, force: true });
  }
});

test("Astra normalizes unsupported settings and its real response survives reload", async ({}, testInfo) => {
  test.setTimeout(180_000);
  const { apiKey, baseUrl } = await credentials();
  test.skip(!apiKey, "A real Nanocodex account key is required.");
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-desktop-astra-"));
  const { application, page, errors } = await launch(directory, apiKey);
  let createdId: string | undefined;
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame() && !createdId)
      createdId = new URL(frame.url()).hash.slice(1) || undefined;
  });
  try {
    await expect(
      page.getByText("Managed agents", { exact: true })
    ).toBeVisible();
    await page.locator(".model-trigger").click();
    const model = page.getByRole("combobox", { name: "Model", exact: true });
    const thinking = page.getByRole("combobox", { name: "Reasoning effort" });
    const pro = page.getByRole("checkbox", { name: "Pro reasoning" });
    await expect(model).toHaveValue("gpt-5.6-sol");
    await expect(thinking).toHaveValue("high");
    await thinking.selectOption("none");
    await pro.check();
    await model.selectOption("gpt-6-astra");
    await expect(thinking).toHaveValue("high");
    await expect(
      thinking.getByRole("option", { name: "None", exact: true })
    ).toHaveJSProperty("disabled", true);
    await expect(
      thinking.getByRole("option", { name: "Max", exact: true })
    ).toHaveJSProperty("disabled", false);
    await expect(pro).not.toBeChecked();
    await expect(pro).toBeDisabled();
    await page.getByRole("button", { name: "Done", exact: true }).click();
    await page
      .getByRole("textbox", { name: "Message Nanocodex" })
      .fill("Reply with exactly ASTRA_DESKTOP_OK. Do not call tools.");
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    const result = page
      .locator(".transcript .assistant-message, .transcript .inline-error")
      .last();
    await expect(result).toBeVisible({ timeout: 120_000 });
    await expect(page.locator(".working-line")).toHaveCount(0);
    const resultText = await result.innerText();
    expect(createdId).toBeTruthy();
    const response = await fetch(new URL(`/v1/agents/${createdId}`, baseUrl), {
      headers: { authorization: `Bearer ${apiKey}` },
    });
    expect(response.ok).toBe(true);
    expect((await response.json()).settings).toEqual({
      model: "gpt-6-astra",
      thinking: "high",
      reasoning_mode: "standard",
      fast_mode: false,
    });
    await page.reload();
    await expect(result).toHaveText(resultText, { timeout: 30_000 });
    await page.locator(".model-trigger").click();
    await expect(model).toHaveValue("gpt-6-astra");
    await expect(thinking).toHaveValue("high");
    await expect(pro).not.toBeChecked();
    await expect(pro).toBeDisabled();
    await page.screenshot({
      path: testInfo.outputPath("astra-durable-thread.png"),
    });
    await testInfo.attach("astra-result.json", {
      body: JSON.stringify({ agentId: createdId, result: resultText }),
      contentType: "application/json",
    });
    await page.getByRole("button", { name: "Done", exact: true }).click();
    await page
      .getByRole("textbox", { name: "Message Nanocodex" })
      .press("Meta+t");
    await page.locator(".model-trigger").click();
    await expect(model).toHaveValue("gpt-5.6-sol");
    await expect(thinking).toHaveValue("high");
    await expect(pro).toBeEnabled();
    await expect(
      thinking.getByRole("option", { name: "None", exact: true })
    ).toHaveJSProperty("disabled", false);
    expect(await page.locator("body").innerText()).not.toContain(apiKey!);
    expect(errors).toEqual([]);
    expect(resultText, "The linked provider must support Astra").toContain(
      "ASTRA_DESKTOP_OK"
    );
  } finally {
    await application.close();
    if (createdId) await removeTestAgent(createdId, { apiKey, baseUrl });
    await rm(directory, { recursive: true, force: true });
  }
});

test("real managed turns execute on a local Hand and survive reload", async ({}, testInfo) => {
  test.setTimeout(300_000);
  const { apiKey, baseUrl } = await credentials();
  test.skip(!apiKey, "A real Nanocodex account key is required.");
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-desktop-e2e-"));
  const { application, page, errors } = await launch(directory, apiKey);
  const created = new Set<string>();
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) {
      const id = new URL(frame.url()).hash.slice(1);
      if (id && !created.size) created.add(id);
    }
  });
  try {
    await expect(
      page.getByText("Managed agents", { exact: true })
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Let’s build" })
    ).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath("new-thread.png") });
    await page.getByRole("button", { name: "Hands", exact: true }).click();
    await page
      .getByRole("button", { name: "Use this computer", exact: true })
      .first()
      .click();
    await expect(
      page.getByRole("textbox", { name: "Workspace folder" })
    ).not.toHaveValue("");
    await page
      .getByRole("textbox", { name: "Workspace folder" })
      .fill(directory);
    await expect(
      page.getByRole("textbox", { name: "Machine ID" })
    ).not.toBeVisible();
    await page.getByRole("button", { name: "Start Hand", exact: true }).click();
    await expect(page.locator(".hand-card .status-badge")).toHaveText(
      "Connected",
      { timeout: 30_000 }
    );
    await page.screenshot({ path: testInfo.outputPath("connected-hand.png") });
    await page.getByRole("button", { name: "Use in thread" }).click();
    const composer = page.getByRole("textbox", { name: "Message Nanocodex" });
    await page.locator(".model-trigger").click();
    await page
      .getByRole("combobox", { name: "Reasoning effort" })
      .selectOption("low");
    await page.getByRole("button", { name: "Done", exact: true }).click();
    await composer.fill(
      'Desktop integration test. On the selected Hand, run a shell command that writes the exact text "hand-roundtrip-ok" into desktop-evidence.txt in the root of its workspace, then read that file. Reply with exactly HAND_ROUNDTRIP_OK when it succeeds. Do not provision a cloud sandbox.'
    );
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(page.locator(".user-message")).toContainText(
      "Desktop integration test."
    );
    await expect(page.locator(".assistant-message").last()).toContainText(
      "HAND_ROUNDTRIP_OK",
      { timeout: 120_000 }
    );
    await expect(page.locator(".working-line")).toHaveCount(0, {
      timeout: 15_000,
    });
    expect(
      await readFile(join(directory, "desktop-evidence.txt"), "utf8")
    ).toBe("hand-roundtrip-ok");
    await page.reload();
    await expect(page.locator(".assistant-message").last()).toContainText(
      "HAND_ROUNDTRIP_OK",
      { timeout: 30_000 }
    );
    await composer.fill(
      "Without running another command, what was the exact content of the file you just wrote? Reply with that text only."
    );
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(page.locator(".user-message")).toHaveCount(2, {
      timeout: 30_000,
    });
    await expect(page.locator(".assistant-message").last()).toContainText(
      "hand-roundtrip-ok",
      { timeout: 90_000 }
    );
    await expect(page.locator(".working-line")).toHaveCount(0, {
      timeout: 15_000,
    });
    await page.screenshot({ path: testInfo.outputPath("durable-thread.png") });
    expect(
      await page.evaluate(() => ({
        node: typeof (window as unknown as { require: unknown }).require,
        storage: Object.keys(localStorage),
      }))
    ).toEqual({ node: "undefined", storage: [] });
    expect(await page.locator("body").innerText()).not.toContain(apiKey!);
    await page.getByRole("button", { name: /^Hands/ }).click();
    await page.getByRole("button", { name: "Find connected Hands" }).click();
    await expect(page.locator(".user-message")).toHaveCount(3, {
      timeout: 30_000,
    });
    await expect(page.locator(".user-message.pending")).toHaveCount(0, {
      timeout: 30_000,
    });
    await expect(page.locator(".working-line")).toHaveCount(0, {
      timeout: 60_000,
    });
    await page.getByRole("button", { name: /^Hands/ }).click();
    await expect(page.locator(".remote-inventory")).toBeVisible();
    await page.locator(".remote-inventory").scrollIntoViewIfNeeded();
    await page.screenshot({
      path: testInfo.outputPath("discovered-hands.png"),
    });
    await page.getByRole("button", { name: "Stop Hand", exact: true }).click();
    await expect(page.locator(".hand-card .status-badge")).toHaveText(
      "Stopped"
    );
    expect(errors).toEqual([]);
  } finally {
    await application.close();
    await Promise.all(
      [...created].map((id) => removeTestAgent(id, { apiKey, baseUrl }))
    );
    await rm(directory, { recursive: true, force: true });
  }
});

test("choosing a folder starts its Hand on send with no setup", async ({}, testInfo) => {
  test.setTimeout(180_000);
  const { apiKey, baseUrl } = await credentials();
  test.skip(!apiKey, "A real Nanocodex account key is required.");
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-desktop-folder-"));
  const { application, page, errors } = await launch(directory, apiKey);
  let createdId: string | undefined;
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame() && !createdId)
      createdId = new URL(frame.url()).hash.slice(1) || undefined;
  });
  try {
    await expect(
      page.getByText("Managed agents", { exact: true })
    ).toBeVisible();
    // Supply the result of the OS picker; every app action still goes through the UI and IPC.
    await application.evaluate(({ dialog }, folder) => {
      dialog.showOpenDialog = async () => ({
        canceled: false,
        filePaths: [folder],
      });
    }, directory);
    await page
      .getByRole("button", { name: "Choose folder", exact: true })
      .click();
    await expect(
      page.getByText("This thread can use this folder when you send.")
    ).toBeVisible();
    expect(
      (await page.evaluate(() => window.nanocodex.state())).hands
    ).toHaveLength(0);
    await page.locator(".model-trigger").click();
    await page
      .getByRole("combobox", { name: "Reasoning effort" })
      .selectOption("low");
    await page.getByRole("button", { name: "Done", exact: true }).click();
    await page
      .getByRole("textbox", { name: "Message Nanocodex" })
      .fill(
        'Use the selected folder Hand to write the exact bytes "folder-default-ok" to auto-hand.txt in its workspace. Read the file and reply with exactly FOLDER_DEFAULT_OK.'
      );
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await page
      .getByRole("textbox", { name: "Message Nanocodex" })
      .fill("Keep this next draft while the first task starts.");
    await expect(page.locator(".assistant-message").last()).toContainText(
      "FOLDER_DEFAULT_OK",
      { timeout: 120_000 }
    );
    await expect(page.locator(".working-line")).toHaveCount(0);
    expect(await readFile(join(directory, "auto-hand.txt"), "utf8")).toBe(
      "folder-default-ok"
    );
    await expect(
      page.getByRole("textbox", { name: "Message Nanocodex" })
    ).toHaveValue("Keep this next draft while the first task starts.");
    const state = await page.evaluate(() => window.nanocodex.state());
    expect(state.hands).toHaveLength(1);
    expect(state.hands[0]).toMatchObject({
      status: "connected",
      agentId: createdId,
    });
    expect(state.hands[0].workspace).toMatch(/nanocodex-desktop-folder-/);
    await page.screenshot({
      path: testInfo.outputPath("automatic-folder-hand.png"),
    });
    await page.keyboard.press("Meta+t");
    await page
      .getByRole("textbox", { name: "Message Nanocodex" })
      .fill("Keep the unrelated tab draft.");
    await page.getByRole("button", { name: /^Hands/ }).click();
    await page
      .getByRole("button", { name: "Use in thread", exact: true })
      .click();
    await expect(page.locator(".assistant-message").last()).toContainText(
      "FOLDER_DEFAULT_OK"
    );
    await expect(
      page.getByRole("textbox", { name: "Message Nanocodex" })
    ).toHaveValue("Keep this next draft while the first task starts.");
    await expect
      .poll(async () => {
        const layout = (await page.evaluate(() => window.nanocodex.state()))
          .layout;
        return layout?.tabs.some(
          (item) => item.draft === "Keep the unrelated tab draft."
        );
      })
      .toBe(true);
    const scopedLayout = (await page.evaluate(() => window.nanocodex.state()))
      .layout;
    expect(
      scopedLayout?.tabs.find((item) => item.threadId === createdId)?.target
    ).toBe(state.hands[0].id);
    await page.getByRole("button", { name: /^Hands/ }).click();
    await expect(page.getByText("Selected thread only")).toBeVisible();
    await page.getByRole("button", { name: "Stop Hand", exact: true }).click();
    await expect(page.locator(".hand-card .status-badge")).toHaveText(
      "Stopped"
    );
    expect(errors).toEqual([]);
  } finally {
    await application.close();
    if (createdId) await removeTestAgent(createdId, { apiKey, baseUrl });
    await rm(directory, { recursive: true, force: true });
  }
});

test("cloud compute provisions and executes from its selected Hand", async ({}, testInfo) => {
  test.setTimeout(300_000);
  const { apiKey, baseUrl } = await credentials();
  test.skip(!apiKey, "A real Nanocodex account key is required.");
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-desktop-cloud-"));
  const { application, page, errors } = await launch(directory, apiKey);
  let createdId: string | undefined;
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame() && !createdId)
      createdId = new URL(frame.url()).hash.slice(1) || undefined;
  });
  try {
    await expect(
      page.getByText("Managed agents", { exact: true })
    ).toBeVisible();
    await page.locator(".model-trigger").click();
    await page
      .getByRole("combobox", { name: "Reasoning effort" })
      .selectOption("low");
    await page.getByRole("button", { name: "Done", exact: true }).click();
    await page.getByRole("button", { name: "Hands", exact: true }).click();
    await page.getByRole("button", { name: "Create cloud Hand" }).click();
    await expect(page.locator(".user-message")).toHaveCount(1, {
      timeout: 30_000,
    });
    await expect(page.locator(".user-message.pending")).toHaveCount(0, {
      timeout: 30_000,
    });
    await expect(page.locator(".working-line")).toHaveCount(0, {
      timeout: 150_000,
    });
    await page.getByRole("button", { name: /^Hands/ }).click();
    const cloud = page
      .locator(".remote-inventory button")
      .filter({ hasText: "desktop-workspace" });
    await expect(cloud).toHaveCount(1);
    await cloud.scrollIntoViewIfNeeded();
    await page.screenshot({ path: testInfo.outputPath("cloud-hand.png") });
    await cloud.click();
    await page
      .getByRole("textbox", { name: "Message Nanocodex" })
      .fill(
        "On the selected cloud Hand run printf to print CLOUD_HAND_OK, followed by uname -s. Reply with exactly CLOUD_HAND_OK Linux after both commands succeed."
      );
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(page.locator(".assistant-message").last()).toContainText(
      "CLOUD_HAND_OK Linux",
      { timeout: 90_000 }
    );
    await expect(page.locator(".working-line")).toHaveCount(0);
    const snapshot = await page.evaluate(
      (id) => window.nanocodex.openThread(id),
      createdId!
    );
    const commands = timeline(snapshot.events).filter(
      (entry) => entry.kind === "tool" && entry.name === "exec_command"
    );
    expect(
      commands.some(
        (entry) =>
          entry.output?.includes("CLOUD_HAND_OK") &&
          entry.output?.includes("Linux")
      )
    ).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("cloud-command.png") });
    expect(errors).toEqual([]);
  } finally {
    await application.close();
    // The owned agent deletion also destroys its owned Cloudflare compute.
    if (createdId) await removeTestAgent(createdId, { apiKey, baseUrl });
    await rm(directory, { recursive: true, force: true });
  }
});

test("steer changes an active task and stop cancels its running command", async ({}, testInfo) => {
  test.setTimeout(240_000);
  const { apiKey, baseUrl } = await credentials();
  test.skip(!apiKey, "A real Nanocodex account key is required.");
  const directory = await mkdtemp(
    join(tmpdir(), "nanocodex-desktop-controls-")
  );
  const { application, page, errors } = await launch(directory, apiKey);
  let createdId: string | undefined;
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame() && !createdId)
      createdId = new URL(frame.url()).hash.slice(1) || undefined;
  });
  try {
    await expect(
      page.getByText("Managed agents", { exact: true })
    ).toBeVisible();
    await application.evaluate(({ dialog }, folder) => {
      dialog.showOpenDialog = async () => ({
        canceled: false,
        filePaths: [folder],
      });
    }, directory);
    await page
      .getByRole("button", { name: "Choose folder", exact: true })
      .click();
    await page.locator(".model-trigger").click();
    await page
      .getByRole("combobox", { name: "Reasoning effort" })
      .selectOption("low");
    await page.getByRole("button", { name: "Done", exact: true }).click();
    const composer = page.getByRole("textbox", { name: "Message Nanocodex" });
    await composer.fill(
      "On the selected Hand run sleep 10 with exec_command yield_time_ms 30000, then reply exactly ORIGINAL_FINISH."
    );
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(
      page.locator(".tool-activity").filter({ hasText: "sleep 10" })
    ).toBeVisible({ timeout: 60_000 });
    await composer.fill(
      "New direction: after the running command completes, reply exactly STEER_OK instead of the earlier requested response."
    );
    await page.getByRole("button", { name: "Steer", exact: true }).click();
    await expect(page.locator(".assistant-message").last()).toContainText(
      "STEER_OK",
      { timeout: 60_000 }
    );
    await expect(page.locator(".working-line")).toHaveCount(0);
    await composer.fill(
      "On the selected Hand use exec_command with yield_time_ms 30000 to execute this shell command in its workspace: echo $$ > cancel-process.pid; sleep 60. Reply CANCEL_COMPLETED only after the command finishes."
    );
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect
      .poll(
        async () => {
          try {
            return Number(
              (
                await readFile(join(directory, "cancel-process.pid"), "utf8")
              ).trim()
            );
          } catch {
            return 0;
          }
        },
        { timeout: 60_000 }
      )
      .toBeGreaterThan(0);
    const pid = Number(
      (await readFile(join(directory, "cancel-process.pid"), "utf8")).trim()
    );
    await page
      .getByRole("button", { name: "Stop current turn", exact: true })
      .click();
    await expect(
      page.getByText("Stopped by you.", { exact: true })
    ).toBeVisible({ timeout: 20_000 });
    await expect(page.locator(".working-line")).toHaveCount(0);
    await expect
      .poll(
        () => {
          try {
            process.kill(pid, 0);
            return true;
          } catch {
            return false;
          }
        },
        { timeout: 10_000 }
      )
      .toBe(false);
    const snapshot = await page.evaluate(
      (id) => window.nanocodex.openThread(id),
      createdId!
    );
    expect(snapshot.activeTurns).toEqual([]);
    await testInfo.attach("steering-events.json", {
      body: JSON.stringify(
        snapshot.events.filter((event) =>
          JSON.stringify(event.data).includes("New direction")
        )
      ),
      contentType: "application/json",
    });
    await page.screenshot({ path: testInfo.outputPath("steer-and-stop.png") });
    await page.getByRole("button", { name: /^Hands/ }).click();
    await page.getByRole("button", { name: "Stop Hand", exact: true }).click();
    await expect(page.locator(".hand-card .status-badge")).toHaveText(
      "Stopped"
    );
    expect(errors).toEqual([]);
  } finally {
    await application.close();
    if (createdId) await removeTestAgent(createdId, { apiKey, baseUrl });
    await rm(directory, { recursive: true, force: true });
  }
});
