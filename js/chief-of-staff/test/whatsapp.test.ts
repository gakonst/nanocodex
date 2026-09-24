import assert from "node:assert/strict";
import test from "node:test";
import type { WhatsAppRawMessage } from "@chat-adapter/whatsapp";
import {
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
      "123456789012345",
    ),
    {
      actorId: "15551234567",
      messageId: raw.message.id,
      channel: {
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
      "123456789012345",
    ),
    /thread is not bound/,
  );
  assert.throws(
    () => whatsAppMessageIdentity(
      raw,
      "whatsapp:123456789012345:15551234567",
      "999999999999999",
    ),
    /business phone identity/,
  );
});
