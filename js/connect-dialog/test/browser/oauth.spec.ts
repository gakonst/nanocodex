import { expect, test, type BrowserContext, type Page, type TestInfo } from "@playwright/test";

const requestId = "a".repeat(43);
const requestPath = `/oauth/requests/${requestId}`;
async function setSession(context: BrowserContext, state: string) {
  expect((await context.request.post("/v1/fixture/session", { data: { state } })).ok()).toBe(true);
}
function observe(page: Page, info: TestInfo) {
  const requests: { path: string; method: string; body: unknown; origin?: string }[] = [];
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("request", async request => {
    const path = new URL(request.url()).pathname;
    if (path.startsWith("/v1/") || path.startsWith("/oauth/requests/")) requests.push({
      path, method: request.method(), body: request.postDataJSON(), origin: (await request.allHeaders()).origin,
    });
  });
  return {
    requests,
    async evidence() {
      expect(errors).toEqual([]);
      await info.attach("oauth-consent-journey", {
        contentType: "application/json", body: JSON.stringify({ requests, errors, url: page.url() }, null, 2),
      });
      await info.attach("oauth-consent-screen", { contentType: "image/png", body: await page.screenshot() });
    },
  };
}
async function signIn(page: Page) {
  await page.getByRole("textbox", { name: "Mobile number" }).fill("+12025550100");
  await page.getByRole("button", { name: "Text me a code" }).click();
  await page.getByRole("textbox", { name: "6-digit code" }).fill("123456");
  await page.getByRole("button", { name: "Continue", exact: true }).click();
}

test("MCP client consent displays identity and access before genuine hosted approval", async ({ context, page }, info) => {
  await setSession(context, "persistent");
  const observed = observe(page, info);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/?oauth_request=${requestId}`);
  await expect(page.getByRole("heading", { name: "Connect Synthetic MCP Client" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Requested access" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Allow access" })).toBeEnabled();
  await expect.poll(() => observed.requests.map(request => request.path)).toEqual([requestPath, "/v1/me", "/v1/connectors"]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await observed.evidence();
  await page.getByRole("button", { name: "Allow access" }).click();
  await expect(page).toHaveURL(/127\.0\.0\.1:4198\/oauth-callback\?registered=kept&code=synthetic-code&state=original-state/);
  const authorization = observed.requests.find(request => request.path === "/v1/connect/hosted-authorization/authorize")!;
  const approval = observed.requests.find(request => request.path === `${requestPath}/approve`)!;
  expect(authorization.origin).toBe("http://modal.nanocodex.localhost:4198");
  expect(approval.body).toEqual({
    account_address: "0x1111111111111111111111111111111111111111",
    code: "s".repeat(43), scope: "agent:run", resources: (authorization.body as any).resources,
  });
  expect((authorization.body as any).resources).not.toContain("urn:nanocodex:connector:gmail");
  expect((authorization.body as any).resources).not.toContain("urn:nanocodex:history:read");
  expect((authorization.body as any).app_origin).toBe("http://127.0.0.1:4198");
  await observed.evidence();
});

test("SMS restores an expired session but leaves OAuth consent to an explicit action", async ({ context, page }, info) => {
  await setSession(context, "expired");
  const observed = observe(page, info);
  await page.goto(`/?oauth_request=${requestId}`);
  await expect(page.getByRole("textbox", { name: "Mobile number" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Allow access" })).toHaveCount(0);
  await expect(page.getByRole("checkbox")).toHaveCount(0);
  await signIn(page);
  await expect(page.getByRole("button", { name: "Allow access" })).toBeEnabled();
  expect(observed.requests.some(request => request.path.endsWith("/approve"))).toBe(false);
  expect(observed.requests.some(request => request.path.endsWith("/authorize"))).toBe(false);
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await expect(page).toHaveURL(/error=access_denied&state=original-state/);
  expect(observed.requests.some(request => request.path.endsWith("/authorize"))).toBe(false);
  await observed.evidence();
});

test("account changes before approval require fresh sign-in and never exchange a code", async ({ context, page }, info) => {
  await setSession(context, "persistent");
  const observed = observe(page, info);
  await page.goto(`/?oauth_request=${requestId}`);
  await expect(page.getByRole("button", { name: "Allow access" })).toBeEnabled();
  await setSession(context, "persistent-other");
  await page.getByRole("button", { name: "Allow access" }).click();
  await expect(page.getByRole("alert")).toContainText("session changed or expired");
  await expect(page.getByRole("textbox", { name: "Mobile number" })).toBeVisible();
  expect(observed.requests.some(request => request.path.endsWith("/approve"))).toBe(false);
  await observed.evidence();
});

test("expired request has no approval controls and an unexpected callback is never followed", async ({ context, page }, info) => {
  const observed = observe(page, info);
  await page.goto(`/?oauth_request=${"z".repeat(43)}`);
  await expect(page.getByRole("alert")).toContainText("expired");
  await expect(page.getByRole("button", { name: "Allow access" })).toHaveCount(0);
  await setSession(context, "persistent");
  await page.goto(`/?oauth_request=${"t".repeat(43)}`);
  await page.getByRole("button", { name: "Allow access" }).click();
  await expect(page.getByRole("alert")).toContainText("unexpected callback");
  await expect(page).toHaveURL(/oauth_request=/);
  await expect(page.getByRole("button", { name: "Allow access" })).toHaveCount(0);
  await observed.evidence();
});

test("unknown issuer and duplicate request IDs cannot send account data or open consent", async ({ page }, info) => {
  const observed = observe(page, info);
  await page.goto(`/?oauth_request=${requestId}&oauth_issuer=https%3A%2F%2Funtrusted.example`);
  await expect(page.getByRole("alert")).toContainText("invalid");
  expect(observed.requests).toEqual([]);
  await page.goto(`/?oauth_request=${requestId}&oauth_request=${"b".repeat(43)}`);
  await expect(page.getByRole("alert")).toContainText("invalid");
  expect(observed.requests).toEqual([]);
  await observed.evidence();
});

test("current local issuer and account switching preserve a separate consent decision", async ({ context, page }, info) => {
  await setSession(context, "persistent");
  const observed = observe(page, info);
  await page.goto(`/?oauth_request=${requestId}&oauth_issuer=${encodeURIComponent("http://modal.nanocodex.localhost:4198")}`);
  await expect(page.getByRole("button", { name: "Allow access" })).toBeEnabled();
  await page.getByRole("button", { name: "Switch account" }).click();
  await expect(page.getByRole("textbox", { name: "Mobile number" })).toBeVisible();
  await signIn(page);
  await expect(page.getByRole("button", { name: "Allow access" })).toBeEnabled();
  expect(observed.requests.some(request => request.path.endsWith("/authorize"))).toBe(false);
  await observed.evidence();
});

test("scope narrowing signs only chosen direct connector access without ChatGPT authority", async ({ context, page }, info) => {
  await setSession(context, "persistent");
  const observed = observe(page, info);
  await page.goto(`/?oauth_request=${requestId}`);
  const agentScope = page.getByRole("checkbox", { name: /Run agents using/ });
  await expect(agentScope).toBeEnabled();
  await expect(agentScope).toBeChecked();
  await page.getByText("Not connected", { exact: false }).click();
  await expect(page.getByRole("checkbox", { name: "Use Slack" })).toBeDisabled();
  await expect(page.getByRole("checkbox", { name: "Use Gmail" })).not.toBeChecked();
  await agentScope.uncheck();
  await expect(page.getByRole("button", { name: "Allow access" })).toBeDisabled();
  await page.getByRole("checkbox", { name: "Use Gmail" }).check();
  await page.getByRole("button", { name: "Allow access" }).click();
  await expect(page).toHaveURL(/code=synthetic-code/);
  const authorization = observed.requests.find(request => request.path === "/v1/connect/hosted-authorization/authorize")!.body as any;
  const approval = observed.requests.find(request => request.path.endsWith("/approve"))!.body as any;
  expect(approval.scope).toBe("connector:gmail");
  expect(approval.resources).toEqual(authorization.resources);
  expect(approval.resources).toContain("urn:nanocodex:connector:gmail");
  expect(approval.resources).not.toContain("urn:nanocodex:connector:chatgpt");
  expect(approval.resources).not.toContain("urn:nanocodex:agent:output:actions");
  expect(approval.resources).not.toContain("urn:nanocodex:history:read");
  await observed.evidence();
});


test("hosted rejection explains the failure and permits a revised selection before exchange", async ({ context, page }, info) => {
  await setSession(context, "persistent");
  const observed = observe(page, info);
  await page.goto(`/?oauth_request=${requestId}`);
  await expect(page.getByRole("button", { name: "Allow access" })).toBeEnabled();
  await page.getByRole("checkbox", { name: "Read conversation history" }).check();
  await page.getByRole("button", { name: "Allow access" }).click();
  await expect(page.getByRole("alert")).toContainText("This permission is unavailable for this account.");
  expect(observed.requests.some(request => request.path.endsWith("/approve"))).toBe(false);
  await page.getByRole("checkbox", { name: "Read conversation history" }).uncheck();
  await page.getByRole("button", { name: "Allow access" }).click();
  await expect(page).toHaveURL(/code=synthetic-code/);
  await observed.evidence();
});

for (const viewport of [{ width: 320, height: 640 }, { width: 390, height: 844 }, { width: 1280, height: 900 }]) {
  test(`full permission selection remains usable at ${viewport.width}px`, async ({ context, page }, info) => {
    await setSession(context, "persistent");
    await page.setViewportSize(viewport);
    if (viewport.width === 320) await page.emulateMedia({ colorScheme: "dark" });
    const observed = observe(page, info);
    await page.goto(`/?oauth_request=${"f".repeat(43)}`);
    const approve = page.getByRole("button", { name: "Allow access", exact: true });
    await expect(approve).toBeEnabled();
    await expect.poll(() => observed.requests.some(r => r.path === "/v1/connectors")).toBe(true);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const bounds = await approve.boundingBox();
    expect(bounds!.y).toBeGreaterThanOrEqual(0);
    expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(viewport.height);
    await page.screenshot({ path: `../../output/connect-mcp-ux/consent-${viewport.width}.png` });
    await observed.evidence();
    await page.locator(".dialog-content").evaluate(el => { el.scrollTop = el.scrollHeight; });
    const afterScroll = await approve.boundingBox();
    expect(afterScroll!.y + afterScroll!.height).toBeLessThanOrEqual(viewport.height);
    expect(afterScroll!.height).toBeGreaterThanOrEqual(44);
    expect(observed.requests.some(r => r.path.endsWith("/approve"))).toBe(false);
  });
}

test("bulk service selection never adds memory writes or unavailable services", async ({ context, page }, info) => {
  await setSession(context, "persistent");
  const observed = observe(page, info);
  await page.goto(`/?oauth_request=${"f".repeat(43)}`);
  await page.getByRole("button", { name: "Select all services", exact: true }).click();
  await expect(page.getByRole("checkbox", { name: "Use Gmail", exact: true })).toBeChecked();
  await expect(page.getByRole("checkbox", { name: "Write saved memory", exact: true })).not.toBeChecked();
  await page.getByRole("button", { name: "Clear services", exact: true }).click();
  await expect(page.getByRole("checkbox", { name: "Use Gmail", exact: true })).not.toBeChecked();
  const memory = page.getByRole("checkbox", { name: "Read saved memory", exact: true });
  await memory.focus();
  await page.keyboard.press("Space");
  await expect(memory).toBeChecked();
  await page.getByRole("checkbox", { name: /Run agents using/ }).uncheck();
  await page.getByRole("button", { name: "Allow access", exact: true }).click();
  await expect(page).toHaveURL(/code=synthetic-code/);
  const approval = observed.requests.find(r => r.path.endsWith("/approve"))!.body as any;
  expect(approval.scope).toBe("memory:read");
  expect(approval.resources).toContain("urn:nanocodex:memory:read");
  expect(approval.resources).not.toContain("urn:nanocodex:memory:write");
  expect(approval.resources).not.toContain("urn:nanocodex:connector:slack");
  await observed.evidence();
});
