import { expect, test } from "@playwright/test";

test("services-only consent shows exact scoped resources and sends signed scope after explicit approval", async ({ page, context }, info) => {
  expect((await context.request.post("/v1/fixture/session", { data: { state: "persistent" } })).ok()).toBe(true);
  const requests: { path: string; method: string; body: unknown }[] = [];
  page.on("request", request => {
    if (new URL(request.url()).pathname.startsWith("/v1/")) requests.push({path: new URL(request.url()).pathname, method: request.method(), body: request.postDataJSON()});
  });
  await page.goto("/?services=1");
  await expect(page.getByRole("heading", { name: "Vault", exact: true })).toBeVisible();
  await expect(page.getByText("Vault items: synthetic_vault_item")).toBeVisible();
  await expect(page.getByText("Websites: https://login.example.test")).toBeVisible();
  await expect(page.getByText("Numbers: synthetic_number")).toBeVisible();
  await expect(page.getByText(/Read incoming messages, including verification codes/)).toBeVisible();
  await expect(page.getByText(/separately approve the quoted recurring costs/)).toBeVisible();
  await expect(page.getByText(/separately confirm release/)).toBeVisible();
  await expect(page.getByRole("button", {name: /^Run agents:/})).toHaveCount(0);
  expect(requests.filter(request => request.method === "POST")).toEqual([]);
  await page.setViewportSize({width: 390, height: 844});
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await info.attach("service-consent", {contentType: "image/png", body: await page.screenshot({fullPage: true})});
  await page.getByRole("button", {name: "Allow access", exact: true}).click();
  await expect(page.getByRole("status")).toHaveText("Request approved");
  const authorize = requests.find(request => request.path === "/v1/connect/hosted-authorization/authorize");
  expect(authorize).toBeDefined();
  expect(JSON.stringify(authorize!.body)).toContain("urn:nanocodex:services:");
  expect(JSON.stringify(authorize!.body)).not.toContain("urn:nanocodex:agent:run");
  await info.attach("service-consent-http.json", {contentType: "application/json", body: JSON.stringify(requests, null, 2)});
});
