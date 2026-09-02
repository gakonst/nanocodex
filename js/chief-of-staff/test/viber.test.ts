import assert from "node:assert/strict";
import test from "node:test";
import {
  readViberWebhook,
  sendViberText,
  ViberDeliveryError,
  ViberWebhookError,
} from "../src/viber.ts";

const authToken = "viber-test-token-that-is-long-enough-and-not-real";

test("Viber verifies the exact callback body and preserves large message tokens", async () => {
  const body = `{"event":"message","timestamp":1457764197627,"message_token":5741311803571721087,"sender":{"id":"01234567890A="},"message":{"type":"picture","text":"diagram","media":"https://example.com/diagram.png"}}`;
  const callback = await readViberWebhook(new Request("https://chief.example/webhooks/viber", {
    body,
    headers: { "x-viber-content-signature": await signature(body) },
    method: "POST",
  }), authToken);

  assert.deepEqual(callback, {
    actorId: "01234567890A=",
    kind: "message",
    messageId: "5741311803571721087",
    text: "[Viber image]\ndiagram\nhttps://example.com/diagram.png",
  });
});

test("Viber rejects a valid signature if the callback body changes", async () => {
  const original = `{"event":"message","message_token":1,"sender":{"id":"user="},"message":{"type":"text","text":"yes"}}`;
  const changed = original.replace("yes", "no");

  await assert.rejects(
    readViberWebhook(new Request("https://chief.example/webhooks/viber", {
      body: changed,
      headers: { "x-viber-content-signature": await signature(original) },
      method: "POST",
    }), authToken),
    (error: unknown) => error instanceof ViberWebhookError && error.code === "invalid_signature",
  );
});

test("Viber outbound requests keep credentials in headers and check provider status", async () => {
  let request: Request | undefined;
  await sendViberText({
    authToken,
    botName: "Nanocodex",
    receiver: "01234567890A=",
    text: "Done",
  }, async (input, init) => {
    request = new Request(input, init);
    return Response.json({ status: 0, status_message: "ok" });
  });

  assert.ok(request);
  assert.equal(request.headers.get("x-viber-auth-token"), authToken);
  assert.equal((await request.json() as { text?: unknown }).text, "Done");

  await assert.rejects(
    sendViberText({
      authToken,
      botName: "Nanocodex",
      receiver: "01234567890A=",
      text: "Done",
    }, async () => Response.json({ status: 6, status_message: "notSubscribed" })),
    (error: unknown) => error instanceof ViberDeliveryError
      && error.code === "provider_status_6",
  );
});

async function signature(body: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(authToken),
    { hash: "SHA-256", name: "HMAC" },
    false,
    ["sign"],
  );
  const result = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(body));
  return Buffer.from(result).toString("hex");
}
