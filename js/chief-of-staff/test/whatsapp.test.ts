import assert from "node:assert/strict";
import test from "node:test";
import type { WhatsAppRawMessage } from "@chat-adapter/whatsapp";
import {
  configurationReadiness,
  whatsAppMessageIdentity,
} from "../src/protocol.ts";

const raw = {
  message: {
    from: "15551234567",
    id: "wamid.HBgLMTU1NTEyMzQ1NjcVAgARGBI3QkY=",
    timestamp: "1700000000",
    type: "text",
    text: { body: "hello" },
  },
  phoneNumberId: "123456789012345",
  userId: "15551234567",
} as WhatsAppRawMessage;

test("WhatsApp identity is bound to the configured business phone and canonical user", () => {
  assert.deepEqual(
    whatsAppMessageIdentity(
      raw,
      "whatsapp:123456789012345:15551234567",
      "account-a",
      "123456789012345",
    ),
    {
      actorId: "15551234567",
      messageId: raw.message.id,
      channel: {
        accountId: "account-a",
        businessPhoneNumberId: "123456789012345",
        conversationId: "whatsapp:123456789012345:15551234567",
        platform: "whatsapp",
        userId: "15551234567",
      },
    },
  );

  assert.throws(
    () => whatsAppMessageIdentity(
      raw,
      "whatsapp:999999999999999:15551234567",
      "account-a",
      "123456789012345",
    ),
    /thread is not bound/,
  );
  assert.throws(
    () => whatsAppMessageIdentity(
      raw,
      "whatsapp:123456789012345:15551234567",
      "account-a",
      "999999999999999",
    ),
    /business phone identity/,
  );
});

test("WhatsApp readiness is independent from Slack configuration", () => {
  const readiness = configurationReadiness({
    CHIEF_OF_STAFF_PUBLIC_ORIGIN: "https://chief.example",
    NANOCODEX_API_KEY: `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`,
    WHATSAPP_ACCESS_TOKEN: "token".repeat(12),
    WHATSAPP_APP_SECRET: "app-secret-which-is-long-enough",
    WHATSAPP_PHONE_NUMBER_ID: "123456789012345",
    WHATSAPP_VERIFY_TOKEN: "verify-token-which-is-long-enough",
  });

  assert.equal(readiness.configured, false);
  assert.equal(readiness.whatsapp.configured, true);
  assert.equal(readiness.whatsapp.webhookUrl, "https://chief.example/webhooks/whatsapp");
});
