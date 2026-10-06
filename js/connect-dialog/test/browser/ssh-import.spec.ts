import { expect, test } from "@playwright/test";

test("CLI SSH consent displays the exact public target before approval without key transport", async ({ context, page }, info) => {
  await context.request.post("/v1/fixture/session", { data: { state: "persistent" } });
  const requests: { path: string; body: unknown }[] = [];
  page.on("request", request => {
    const path = new URL(request.url()).pathname;
    if (path.startsWith("/v1/")) requests.push({ path, body: request.postDataJSON() });
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/?ssh-import");
  await expect(page.getByRole("heading", { name: "Import local private key" })).toBeVisible();
  for (const text of ["synthetic-lab", "server.example.com", "2222", "deploy", `SHA256:${"a".repeat(43)}`]) {
    await expect(page.getByText(text, { exact: true })).toBeVisible();
  }
  expect(requests.some(r => r.path.endsWith("/authorize"))).toBe(false);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await info.attach("ssh-target-consent", { body: await page.screenshot(), contentType: "image/png" });
  await page.getByRole("button", { name: /Allow|Approve|Connect/, exact: false }).last().click();
  await expect(page.getByRole("status")).toHaveText("Request approved");
  const authorization = requests.find(r => r.path.endsWith("/hosted-authorization/authorize"));
  expect(authorization).toBeDefined();
  const resources = (authorization!.body as { resources: string[] }).resources;
  expect(resources).toContain(`urn:nanocodex:credential-import:ssh:pem-v1:sha256:${"a".repeat(43)}`);
  expect(resources).toContain(`urn:nanocodex:ssh-target:synthetic-lab:server.example.com:2222:deploy:SHA256%3A${"a".repeat(43)}`);
  expect(JSON.stringify(requests)).not.toContain("private_key");
  expect(JSON.stringify(requests)).not.toContain("BEGIN PRIVATE KEY");
  await info.attach("ssh-consent-network", { body: JSON.stringify(requests, null, 2), contentType: "application/json" });
});

test("SMS sign-in displays SSH target and waits for explicit import approval", async ({ context, page }, info) => {
  await context.request.post("/v1/fixture/session", { data: { state: "expired" } });
  const authorized: unknown[] = [];
  page.on("request", request => {
    if (new URL(request.url()).pathname.endsWith("/hosted-authorization/authorize")) authorized.push(request.postDataJSON());
  });
  await page.goto("/?ssh-import&wizard");
  await expect(page.getByRole("heading", { name: "Import local private key" })).toBeVisible();
  await expect(page.getByText("server.example.com", { exact: true })).toBeVisible();
  await page.getByRole("textbox", { name: "Mobile number" }).fill("+12025550100");
  await page.getByRole("button", { name: "Text me a code" }).click();
  await page.getByRole("textbox", { name: "6-digit code" }).fill("123456");
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await expect(page.getByRole("button", { name: "Allow access", exact: true })).toBeEnabled();
  expect(authorized).toEqual([]);
  await expect(page.getByText("server.example.com", { exact: true })).toBeVisible();
  await info.attach("signed-in-awaiting-ssh-consent", { body: await page.screenshot(), contentType: "image/png" });
  await page.getByRole("button", { name: "Allow access", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Request approved");
  expect(authorized).toHaveLength(1);
});
