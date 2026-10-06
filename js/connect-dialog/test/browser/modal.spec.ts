import { expect, test, type Page, type TestInfo } from "@playwright/test";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const evidence = resolve(process.cwd(), "../../output/connect-full-page");
const sizes = [
  { name: "desktop", width: 1280, height: 900 },
  { name: "mobile", width: 390, height: 844 },
  { name: "short", width: 390, height: 440 },
  { name: "narrow-keyboard", width: 320, height: 360 },
];

async function contained(page: Page) {
  const geometry = await page.evaluate(() => {
    const shell = document.querySelector(".dialog-shell")!;
    const box = shell.getBoundingClientRect();
    return {
      width: innerWidth, height: innerHeight,
      documentWidth: document.documentElement.scrollWidth,
      documentHeight: document.documentElement.scrollHeight,
      left: box.left, top: box.top, right: box.right, bottom: box.bottom,
      horizontalOverflow: [...shell.querySelectorAll("*")].filter(el => {
        const style = getComputedStyle(el);
        return style.overflowX === "visible" && el.scrollWidth > el.clientWidth + 1 && el.clientWidth > 0;
      }).map(el => `${el.tagName}.${el.className}`),
    };
  });
  expect(geometry.documentWidth).toBeLessThanOrEqual(geometry.width);
  expect(geometry.documentHeight).toBeLessThanOrEqual(geometry.height);
  expect(geometry.left).toBe(0);
  expect(geometry.top).toBe(0);
  expect(geometry.right).toBe(geometry.width);
  expect(geometry.bottom).toBe(geometry.height);
  expect(geometry.horizontalOverflow).toEqual([]);
  return geometry;
}

async function mobileActions(page: Page, primary: string) {
  if ((page.viewportSize()?.width ?? 1280) > 620) return;
  const button = page.getByRole("button", { name: primary, exact: true });
  await expect(button).toBeInViewport();
  const footer = page.locator(".sms-auth-actions, .dialog-actions");
  const box = await footer.boundingBox();
  expect(Math.abs(box!.y + box!.height - page.viewportSize()!.height)).toBeLessThanOrEqual(1);
  const before = await button.boundingBox();
  await page.locator(".sms-auth-content, .dialog-content").evaluateAll(elements => elements.forEach(element => { element.scrollTop = element.scrollHeight; }));
  expect(await button.boundingBox()).toEqual(before);
  await page.locator(".sms-auth-content, .dialog-content").evaluateAll(elements => elements.forEach(element => { element.scrollTop = 0; }));
}

async function screenshot(page: Page, info: TestInfo, name: string) {
  const path = resolve(evidence, `${info.title.replace(/\W+/g, "-")}-${name}.png`);
  await page.screenshot({ path });
  await info.attach(name, { path, contentType: "image/png" });
}

for (const theme of ["light", "dark"] as const) {
  for (const size of sizes) {
    test(`${theme} ${size.name} SMS recovery and consent`, async ({ page }, info) => {
      await mkdir(evidence, { recursive: true });
      await page.setViewportSize(size);
      await page.emulateMedia({ colorScheme: theme, reducedMotion: "reduce" });
      const errors: string[] = [];
      const requests: { path: string; body: unknown; status?: number }[] = [];
      page.on("pageerror", error => errors.push(error.message));
      page.on("response", response => {
        const request = response.request();
        if (new URL(request.url()).pathname.startsWith("/v1/")) requests.push({
          path: new URL(request.url()).pathname,
          body: request.postDataJSON(), status: response.status(),
        });
      });
      await page.route("**/*", route => new URL(route.request().url()).hostname === "modal.nanocodex.localhost"
        ? route.continue() : route.abort("blockedbyclient"));
      await page.goto("/");
      const phone = page.getByRole("textbox", { name: "Mobile number" });
      await expect(phone).toBeFocused();
      await contained(page);
      const intro = await page.locator(".sms-auth-panel .wizard-intro").boundingBox();
      const form = await page.locator(".sms-otp-form").boundingBox();
      expect(form!.y).toBeGreaterThanOrEqual(intro!.y + intro!.height);
      expect(Math.abs(form!.x - intro!.x)).toBeLessThan(1);
      expect(form!.width).toBeLessThanOrEqual(372);
      expect(await page.locator("h1").evaluate(el => getComputedStyle(el).fontFamily)).toBe(await phone.evaluate(el => getComputedStyle(el).fontFamily));
      await mobileActions(page, "Text me a code");
      await screenshot(page, info, "phone");
      await phone.fill("123456");
      await phone.press("Enter");
      await expect(page.getByRole("alert")).toContainText("verification code");
      expect(requests.filter(r => r.path.endsWith("/sms/start"))).toHaveLength(0);
      await expect(phone).toHaveAttribute("aria-invalid", "true");
      await phone.fill("+1 202 555 0000");
      await phone.press("Enter");
      await expect(page.getByRole("alert")).toContainText("could not be delivered");
      await phone.fill("+1 202 555 0100");
      await phone.press("Tab");
      await expect(page.getByRole("button", { name: "Cancel", exact: true })).toBeFocused();
      await page.keyboard.press("Tab");
      const send = page.getByRole("button", { name: "Text me a code" });
      await expect(send).toBeFocused();
      expect(await send.evaluate(el => getComputedStyle(el).outlineStyle)).not.toBe("none");
      await page.keyboard.press("Enter");
      const code = page.getByRole("textbox", { name: "6-digit code" });
      await expect(code).toBeVisible();
      await expect(code).toBeFocused();
      await contained(page);
      await mobileActions(page, "Continue");
      await expect(page.getByRole("button", { name: "Use a different number" })).toHaveCount(0);
      await screenshot(page, info, "code");
      await code.fill("12");
      await code.press("Enter");
      await expect(page.getByRole("alert")).toContainText("six-digit");
      expect(requests.filter(r => r.path.endsWith("/sms/verify"))).toHaveLength(0);
      await code.fill("111111");
      await code.press("Enter");
      await expect(page.getByRole("alert")).toContainText("invalid or expired");
      await contained(page);
      await screenshot(page, info, "invalid-code");
      await code.fill("123456");
      await code.press("Enter");
      const approve = page.getByRole("button", { name: "Allow access" });
      await expect(approve).toBeEnabled();
      await expect(page.getByRole("heading", { name: "Connect to Atlas Workspace" })).toBeFocused();
      expect(await page.locator(".dialog-content").evaluate(el => el.scrollTop)).toBe(0);
      expect(await page.evaluate(() => (window as any).__hostReceipt)).toBeUndefined();
      const geometry = await contained(page);
      await mobileActions(page, "Allow access");
      await expect(page.getByRole("button", { name: "Use a different account" })).toHaveCount(0);
      await screenshot(page, info, "consent");
      await approve.focus();
      await page.keyboard.press("Enter");
      await expect(page.getByRole("status")).toHaveText("Request approved");
      const receipt = await page.evaluate(() => (window as any).__hostReceipt);
      expect(receipt.kind).toBe("approved");
      expect(receipt.result.accounts[0].capabilities.auth.mode).toBe("hosted");
      expect(requests.filter(r => r.path.endsWith("/sms/start")).map(r => r.body)).toEqual([
        { phone: "+12025550000" }, { phone: "+12025550100" },
      ]);
      expect(errors).toEqual([]);
      await writeFile(resolve(evidence, `${theme}-${size.name}-receipt.json`), JSON.stringify({ theme, viewport: size, geometry, requests, receipt, errors }, null, 2));
    });
  }
}

test("cancel and Escape never approve access", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Request cancelled");
  expect(await page.evaluate(() => (window as any).__hostReceipt.kind)).toBe("cancelled");
  await page.reload();
  await expect(page.getByRole("textbox", { name: "Mobile number" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("status")).toHaveText("Request cancelled");
  await page.reload();
  await page.getByRole("textbox", { name: "Mobile number" }).fill("+12025550100");
  await page.getByRole("button", { name: "Text me a code" }).click();
  await page.getByRole("textbox", { name: "6-digit code" }).fill("123456");
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await expect(page.getByRole("button", { name: "Allow access" })).toBeEnabled();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Request cancelled");
  expect(await page.evaluate(() => (window as any).__hostReceipt.kind)).toBe("cancelled");
});

for (const variant of [
  { theme: "light" as const, width: 1280, height: 900 },
  { theme: "dark" as const, width: 1280, height: 900 },
  { theme: "light" as const, width: 390, height: 844 },
  { theme: "dark" as const, width: 390, height: 844 },
]) {
  test(`${variant.theme} ${variant.width > 620 ? "desktop" : "mobile"} requested connection groups`, async ({ page }, info) => {
    await page.setViewportSize(variant);
    await page.emulateMedia({ colorScheme: variant.theme });
    await page.goto("/?connections=1");
    await page.getByRole("textbox", { name: "Mobile number" }).fill("+12025550100");
    await page.getByRole("button", { name: "Text me a code" }).click();
    await page.getByRole("textbox", { name: "6-digit code" }).fill("123456");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Connect to Atlas Workspace" })).toBeFocused();
    await expect(page.getByRole("button", { name: /GitHub.*Connected/ })).toBeDisabled();
    const connection = page.getByRole("button", { name: /Google Workspace.*Connect/ });
    await expect(connection).toBeEnabled();
    await expect(page.getByRole("button", { name: "Allow access", exact: true })).toBeDisabled();
    await contained(page);
    await screenshot(page, info, "connections");
    await page.keyboard.press("Tab");
    await expect(connection).toBeFocused();
    expect(await connection.evaluate(el => getComputedStyle(el).outlineOffset)).toBe("-3px");
    expect(await page.evaluate(() => (window as any).__hostReceipt)).toBeUndefined();
    const opened = page.waitForEvent("popup");
    await connection.click();
    const provider = await opened;
    await expect(page.getByRole("status")).toContainText("Finish connecting Google Workspace");
    await screenshot(page, info, "provider-pending");
    await provider.getByRole("button", { name: "Approve Gmail and Calendar" }).click();
    await expect(page.getByRole("button", { name: /Google Workspace.*Connected/ })).toBeDisabled();
    const approve = page.getByRole("button", { name: "Allow access", exact: true });
    await expect(approve).toBeEnabled();
    expect(await page.evaluate(() => (window as any).__hostReceipt)).toBeUndefined();
    await contained(page);
    await screenshot(page, info, "approval-ready");
    await approve.click();
    await expect(page.getByRole("status")).toHaveText("Request approved");
    expect(await page.evaluate(() => (window as any).__hostReceipt.kind)).toBe("approved");
  });
}

test("SDK popup presents centered two-column authorization", async ({ page }) => {
  await page.goto("/launcher.html");
  const opened = page.waitForEvent("popup");
  await page.getByRole("button", { name: "Connect account" }).click();
  const popup = await opened;
  await popup.getByRole("textbox", { name: "Mobile number" }).fill("+12025550100");
  await popup.getByRole("button", { name: "Text me a code" }).click();
  await popup.getByRole("textbox", { name: "6-digit code" }).fill("123456");
  await popup.getByRole("button", { name: "Continue", exact: true }).click();
  await expect(popup.getByRole("heading", { name: "Connect to Atlas Workspace" })).toBeFocused();
  const intro = await popup.locator(".wizard-review-page > .wizard-intro").boundingBox();
  const access = await popup.locator(".wizard-review-page > .wizard-sections").boundingBox();
  expect(access!.x).toBeGreaterThan(intro!.x + intro!.width);
  expect(access!.width).toBeLessThanOrEqual(372);
  expect(Math.abs(access!.y - intro!.y)).toBeLessThan(1);
  const actions = await popup.locator(".dialog-actions").boundingBox();
  const viewport = popup.viewportSize() ?? await popup.evaluate(() => ({width:innerWidth,height:innerHeight}));
  expect(Math.abs((intro!.x + access!.x + access!.width) / 2 - viewport.width / 2)).toBeLessThan(2);
  expect(Math.abs((intro!.y + actions!.y + actions!.height - 24) / 2 - viewport.height / 2)).toBeLessThan(2);
  await popup.getByRole("button", {name:/Run agents:/}).focus();
  await expect(popup.getByRole("tooltip")).toContainText("Run agents");
  await expect(popup.getByText("Access details", {exact:true})).toHaveCount(0);
  const tip = await popup.getByRole("tooltip").boundingBox();
  expect(tip!.x).toBeGreaterThanOrEqual(0);
  expect(tip!.x + tip!.width).toBeLessThanOrEqual(viewport.width);
  await popup.getByRole("button", {name:"Cancel", exact:true}).focus();
  await popup.mouse.move(0, 0);
  await expect(popup.getByRole("tooltip")).toHaveCount(0);
  await contained(popup);
  await popup.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(popup.getByRole("status")).toHaveText("Request cancelled");
  await popup.close();
});

test("provider cancellation and partial grants do not authorize the app", async ({ page }) => {
  await page.goto("/?connections=1");
  await page.getByRole("textbox", {name:"Mobile number"}).fill("+12025550100");
  await page.getByRole("button", {name:"Text me a code"}).click();
  await page.getByRole("textbox", {name:"6-digit code"}).fill("123456");
  await page.getByRole("button", {name:"Continue",exact:true}).click();
  for (const action of ["Cancel", "Approve Gmail only", "Approve Gmail and Calendar"]) {
    const opened = page.waitForEvent("popup");
    await page.getByRole("button", {name:/Google Workspace.*Connect/}).click();
    const provider = await opened;
    await provider.getByRole("button", {name:action,exact:true}).click();
    if (action !== "Approve Gmail and Calendar") {
      if (action === "Cancel") await expect(page.getByRole("alert")).toContainText("cancelled");
      else await expect(page.getByRole("button", {name:/Google Workspace.*Gmail connected.*Calendar requested/})).toBeEnabled();
      await expect(page.getByRole("button", {name:"Allow access",exact:true})).toBeDisabled();
    } else await expect(page.getByRole("button", {name:"Allow access",exact:true})).toBeEnabled();
    expect(await page.evaluate(() => (window as any).__hostReceipt)).toBeUndefined();
  }
  await page.getByRole("button", {name:"Cancel",exact:true}).click();
  await expect(page.getByRole("status")).toHaveText("Request cancelled");
});

for (const width of [1280, 390]) {
  test(`developer appearance ${width} survives sign-in and authorization`, async ({ page }, info) => {
    await page.setViewportSize({width, height:900});
    await page.emulateMedia({colorScheme:"light"});
    await page.goto("/?appearance=brand");
    const shell = page.locator(".connect-onboarding");
    await expect(shell).toHaveCSS("color-scheme", "dark");
    await expect(shell).toHaveCSS("font-family", "system-ui");
    const send = page.getByRole("button", {name:"Text me a code"});
    await expect(send).toHaveCSS("background-color", "rgb(196, 181, 253)");
    await expect(send).toHaveCSS("color", "rgb(0, 0, 0)");
    await expect(send).toHaveCSS("border-radius", "6px");
    await contained(page);
    await screenshot(page, info, "signin");
    await page.getByRole("textbox", {name:"Mobile number"}).fill("+12025550100");
    await send.click();
    await page.getByRole("textbox", {name:"6-digit code"}).fill("123456");
    await page.getByRole("button", {name:"Continue",exact:true}).click();
    const allow = page.getByRole("button", {name:"Allow access",exact:true});
    await expect(allow).toBeEnabled();
    await expect(allow).toHaveCSS("background-color", "rgb(196, 181, 253)");
    await page.mouse.move(0, 0);
    await contained(page);
    await screenshot(page, info, "authorization");
    await allow.click();
    await expect(page.getByRole("status")).toHaveText("Request approved");
  });
}
test("invalid appearance falls back without CSS injection", async ({page}) => {
  await page.goto("/?appearance=invalid");
  const shell = page.locator(".connect-onboarding");
  expect(await shell.getAttribute("style")).toBeNull();
  await expect(page.getByRole("textbox", {name:"Mobile number"})).toBeVisible();
});

test("SDK popup forwards developer appearance into hosted parser and UI", async ({page}) => {
  await page.goto("/launcher.html?themed=1");
  const opened = page.waitForEvent("popup");
  await page.getByRole("button", {name:"Connect account"}).click();
  const popup = await opened;
  const send = popup.getByRole("button", {name:"Text me a code"});
  await expect(send).toHaveCSS("background-color", "rgb(196, 181, 253)");
  await expect(send).toHaveCSS("border-radius", "6px");
  await expect(popup.locator(".connect-onboarding")).toHaveCSS("color-scheme", "dark");
  await expect(popup.locator(".connect-onboarding")).toHaveCSS("font-family", "system-ui");
  await popup.getByRole("button", {name:"Cancel",exact:true}).click();
  await expect(popup.getByRole("status")).toHaveText("Request cancelled");
});

test("session lookup keeps request identity visible without a standalone Cancel screen", async ({ page }, info) => {
  await page.setViewportSize({ width: 320, height: 360 });
  let release!: () => void;
  const pending = new Promise<void>(resolve => { release = resolve; });
  const writes: string[] = [];
  page.on("request", request => { if (request.method() === "POST") writes.push(request.url()); });
  await page.route("**/v1/me*", async route => { await pending; await route.continue(); });
  try {
    await page.goto("/");
    await expect(page.locator(".request-identity")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Connect to Atlas Workspace" })).toBeVisible();
    await expect(page.getByRole("status")).toContainText("Checking your account");
    await expect(page.getByRole("button", { name: "Cancel", exact: true })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Allow access", exact: true })).toHaveCount(0);
    expect(writes).toEqual([]);
    expect(await page.evaluate(() => (window as any).__hostReceipt)).toBeUndefined();
    await contained(page);
    await screenshot(page, info, "session-check");
  } finally {
    release();
  }
  await expect(page.getByRole("textbox", { name: "Mobile number" })).toBeFocused();
  await mobileActions(page, "Text me a code");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Request cancelled");
});

for (const viewport of [{ width: 1280, height: 900 }, { width: 320, height: 360 }]) {
  test(`SMS resend and number correction ${viewport.width}`, async ({ page }, info) => {
    await page.setViewportSize(viewport);
    const starts: unknown[] = [];
    const verifies: unknown[] = [];
    page.on("request", request => {
      if (new URL(request.url()).pathname === "/v1/auth/sms/start") starts.push(request.postDataJSON());
      if (new URL(request.url()).pathname === "/v1/auth/sms/verify") verifies.push(request.postDataJSON());
    });
    await page.goto("/");
    const phone = page.getByRole("textbox", { name: "Mobile number" });
    await phone.fill("+12025550100");
    await page.getByRole("button", { name: "Text me a code" }).click();
    const code = page.getByRole("textbox", { name: "6-digit code" });
    await code.fill("123456");
    // A failed resend must leave the currently entered code available to retry.
    await page.route("**/v1/auth/sms/start", route => route.fulfill({
      status: 503, json: { error: "sms_delivery_failed" },
    }), { times: 1 });
    await page.getByRole("button", { name: "Resend code", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("could not be delivered");
    await expect(code).toHaveValue("123456");
    await expect(page.getByRole("button", { name: "Continue", exact: true })).toBeEnabled();
    // Change number keeps the typed number editable and clears the prior code.
    await page.getByRole("button", { name: "Change number", exact: true }).click();
    await expect(phone).toBeFocused();
    await expect(phone).toHaveValue("+12025550100");
    await phone.fill("+12025550161");
    await phone.press("Enter");
    await expect(code).toHaveValue("");
    await code.fill("111111");
    await page.getByRole("button", { name: "Resend code", exact: true }).click();
    await expect(page.getByRole("status")).toContainText("Another code was sent");
    await expect(code).toHaveValue("");
    await contained(page);
    await mobileActions(page, "Continue");
    await screenshot(page, info, "resent-code");
    await code.fill("123456");
    await code.press("Enter");
    await expect(page.getByRole("button", { name: "Allow access", exact: true })).toBeEnabled();
    await page.getByRole("button", { name: "Allow access", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Request approved");
    expect(starts).toEqual([
      { phone: "+12025550100" }, { phone: "+12025550100" },
      { phone: "+12025550161" }, { phone: "+12025550161" },
    ]);
    expect(verifies).toEqual([{ phone: "+12025550161", challenge_id: "synthetic-sms-challenge", code: "123456" }]);
    await writeFile(resolve(evidence, `sms-recovery-${viewport.width}.json`), JSON.stringify({ viewport, starts, verifies, outcome: "Request approved" }, null, 2));
  });
}
