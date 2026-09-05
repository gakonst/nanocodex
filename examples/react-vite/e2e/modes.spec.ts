import { expect, test } from "@playwright/test";

test("OpenAI stays the default and MPP diagnostics are opt-in", async ({ page }) => {
  await page.goto("/");

  const payment = page.getByLabel("Payment");
  await expect(payment).toHaveValue("openai");
  await expect(page.getByLabel("Default OpenAI Responses WebSocket endpoint")).toHaveValue(
    "wss://api.openai.com/v1/responses",
  );
  await expect(page.getByText("Tempo root", { exact: false })).toHaveCount(0);
  await expect(page.getByText("MPP paid", { exact: true })).toHaveCount(0);

  await payment.selectOption("mpp");
  await expect(page.getByLabel("Default OpenAI Responses WebSocket endpoint")).toHaveValue(
    "wss://openai.mpp.tempo.xyz/v1/responses",
  );
  await expect(page.getByText("Tempo root", { exact: false })).toBeVisible();
  await expect(page.getByText("MPP paid", { exact: true })).toBeVisible();

  await payment.selectOption("openai");
  await expect(page.getByText("Tempo root", { exact: false })).toHaveCount(0);
  await expect(page.getByText("MPP paid", { exact: true })).toHaveCount(0);
});

test("live browser MPP uses a delegated key and completes a paid turn", async ({ page }) => {
  test.skip(process.env.LIVE_MPP_E2E !== "1", "set LIVE_MPP_E2E=1 for the paid browser smoke");

  await page.goto("/");
  await page.getByLabel("Payment").selectOption("mpp");
  await page.getByRole("button", { name: "Start agent" }).click();
  await expect(page.getByText("ready", { exact: true })).toBeVisible({ timeout: 5 * 60_000 });
  await expect(page.getByText(/Tempo root 0x[0-9a-f]{4}…[0-9a-f]{4} delegates to access key 0x/i)).toBeVisible();

  await page.getByLabel("Next prompt").fill("Reply with exactly MPP_BROWSER_OK and nothing else.");
  await page.getByRole("button", { name: "Queue turn" }).click();
  await expect(page.getByText("MPP_BROWSER_OK", { exact: true })).toBeVisible({ timeout: 5 * 60_000 });
  await expect(page.locator(".json-card")).toContainText('"type":"run.completed"');
  await expect(page.getByText("MPP paid", { exact: true }).locator("..")).not.toContainText("—");
});
