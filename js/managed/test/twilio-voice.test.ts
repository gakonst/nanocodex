import { createHmac } from "node:crypto";
import { afterEach, describe, expect, it, vi } from "vitest";
import { buildTwilioVoiceTwiml, createTwilioVoiceCall, fetchTwilioVoiceCall, hangupTwilioVoiceCall, isE164PhoneNumber, verifyTwilioWebhookSignature, type TwilioVoiceEnv } from "../src/twilio-voice";

const sid = `CA${"a".repeat(32)}`;
const env: TwilioVoiceEnv = { TWILIO_ACCOUNT_SID: `AC${"b".repeat(32)}`, TWILIO_API_KEY_SID: `SK${"c".repeat(32)}`, TWILIO_API_KEY_SECRET: "test-key-secret", TWILIO_AUTH_TOKEN: "test-auth-token", TWILIO_VOICE_FROM_NUMBER: "+15551234567" };
const input = { to: "+442012345678", streamUrl: "wss://voice.example/stream", statusCallbackUrl: "https://voice.example/status?call=123&v=1", callId: "call-123" };
function mockCall(status = "queued") {
  const mock = vi.fn().mockResolvedValue(Response.json({ sid, status }));
  vi.stubGlobal("fetch", mock);
  return mock;
}
afterEach(() => vi.unstubAllGlobals());

describe("Twilio voice provider", () => {
  it("validates E.164 strictly", () => {
    for (const value of ["+12", "+123456789012345"]) expect(isE164PhoneNumber(value)).toBe(true);
    for (const value of ["+1", "+0123", "123", "+1 234", "+1234567890123456", "+123\n", null]) expect(isE164PhoneNumber(value)).toBe(false);
  });
  it("escapes XML attributes and nests a correlation Parameter in Connect Stream", () => {
    expect(buildTwilioVoiceTwiml("wss://voice.example/a&b", `a<&>"'`)).toBe('<Response><Connect><Stream url="wss://voice.example/a&amp;b"><Parameter name="callId" value="a&lt;&amp;&gt;&quot;&apos;" /></Stream></Connect></Response>');
  });
  it("creates one call with API-key auth and all progress callbacks", async () => {
    const mock = mockCall();
    expect(await createTwilioVoiceCall(env, input)).toEqual({ sid, status: "queued" });
    expect(mock).toHaveBeenCalledTimes(1);
    const [url, options] = mock.mock.calls[0];
    expect(url).toBe(`https://api.twilio.com/2010-04-01/Accounts/${env.TWILIO_ACCOUNT_SID}/Calls.json`);
    expect(options).toMatchObject({ method: "POST", redirect: "error", headers: { Authorization: `Basic ${btoa(`${env.TWILIO_API_KEY_SID}:${env.TWILIO_API_KEY_SECRET}`)}` } });
    const body = new URLSearchParams(options.body);
    expect(body.get("TimeLimit")).toBe("180");
    expect(body.get("Timeout")).toBe("30");
    expect(body.get("To")).toBe(input.to);
    expect(body.get("From")).toBe(env.TWILIO_VOICE_FROM_NUMBER);
    expect(body.get("Twiml")).toBe(buildTwilioVoiceTwiml(input.streamUrl, input.callId));
    expect(body.get("StatusCallback")).toBe(input.statusCallbackUrl);
    expect(body.get("StatusCallbackMethod")).toBe("POST");
    expect(body.getAll("StatusCallbackEvent")).toEqual(["initiated", "ringing", "answered", "completed"]);
  });
  it("enforces duration bounds and forwards an explicit provider limit", async () => {
    const mock = mockCall();
    for (const maxDurationSeconds of [0, 29, 601, 30.5, NaN, Infinity]) {
      await expect(createTwilioVoiceCall(env, { ...input, maxDurationSeconds })).rejects.toThrow("Invalid Twilio voice input");
    }
    expect(mock).not.toHaveBeenCalled();
    await createTwilioVoiceCall(env, { ...input, maxDurationSeconds: 600 });
    expect(new URLSearchParams(mock.mock.calls[0][1].body).get("TimeLimit")).toBe("600");
  });
  it("fetches status with auth-token fallback and hangs up via completed", async () => {
    const mock = mockCall("completed");
    const fallback = { ...env, TWILIO_API_KEY_SID: undefined, TWILIO_API_KEY_SECRET: undefined };
    await fetchTwilioVoiceCall(fallback, sid);
    expect(mock.mock.calls[0][1]).toMatchObject({ method: "GET", headers: { Authorization: `Basic ${btoa(`${env.TWILIO_ACCOUNT_SID}:${env.TWILIO_AUTH_TOKEN}`)}` } });
    mock.mockResolvedValueOnce(Response.json({ sid, status: "completed" }));
    await hangupTwilioVoiceCall(env, sid);
    expect(mock.mock.calls[1][0]).toContain(`/Calls/${sid}.json`);
    expect(mock.mock.calls[1][1]).toMatchObject({ method: "POST", body: "Status=completed" });
  });
  it("rejects invalid inputs before making requests", async () => {
    const mock = mockCall();
    for (const patch of [{ to: "123" }, { streamUrl: "https://voice.example/stream" }, { streamUrl: "wss://voice.example/stream?token=x" }, { statusCallbackUrl: "https://user:pass@voice.example/status" }, { statusCallbackUrl: "https://voice.example/#fragment" }, { callId: "x".repeat(257) }, { callId: "x\u0000" }]) {
      await expect(createTwilioVoiceCall(env, { ...input, ...patch })).rejects.toThrow("Invalid Twilio voice input");
    }
    await expect(fetchTwilioVoiceCall(env, "../../bad")).rejects.toThrow();
    await expect(createTwilioVoiceCall({ ...env, TWILIO_API_KEY_SECRET: undefined }, input)).rejects.toThrow("not configured");
    expect(mock).not.toHaveBeenCalled();
  });
  it.each(["network", "http", "oversized", "malformed", "wrong-sid"])("redacts %s failures without retrying writes", async (kind) => {
    const mock = mockCall();
    if (kind === "network") mock.mockRejectedValue(new Error("secret phone auth data"));
    if (kind === "http") mock.mockResolvedValue(new Response("secret", { status: 429 }));
    if (kind === "oversized") mock.mockResolvedValue(new Response("x".repeat(65537)));
    if (kind === "malformed") mock.mockResolvedValue(Response.json({ sid, status: "secret" }));
    if (kind === "wrong-sid") mock.mockResolvedValue(Response.json({ sid: `CA${"f".repeat(32)}`, status: "completed" }));
    await expect(hangupTwilioVoiceCall(env, sid)).rejects.toThrow(/^Twilio voice request failed; outcome may be unknown$/);
    expect(mock).toHaveBeenCalledTimes(1);
  });
});

describe("Twilio webhook signatures", () => {
  const url = "https://voice.example:443/status?b=2&a=%2f";
  const fields = new URLSearchParams([["To", "+15551234567"], ["CallSid", sid], ["Extra", "b"], ["Extra", "a"], ["Extra", "a"]]);
  const signature = createHmac("sha1", env.TWILIO_AUTH_TOKEN!).update(`${url}CallSid${sid}ExtraaExtrabTo+15551234567`).digest("base64");
  it("accepts sorted fields and sorted unique duplicate values using auth token", async () => {
    expect(await verifyTwilioWebhookSignature(env, url, fields, signature)).toBe(true);
  });
  it("verifies exact WSS handshake URLs without rewriting scheme or trailing slash", async () => {
    const streamUrl = "wss://voice.example/media/123/";
    const signed = createHmac("sha1", env.TWILIO_AUTH_TOKEN!).update(streamUrl).digest("base64");
    expect(await verifyTwilioWebhookSignature(env, streamUrl, new URLSearchParams(), signed)).toBe(true);
    expect(await verifyTwilioWebhookSignature(env, streamUrl.slice(0, -1), new URLSearchParams(), signed)).toBe(false);
    expect(await verifyTwilioWebhookSignature(env, streamUrl.replace("wss:", "https:"), new URLSearchParams(), signed)).toBe(false);
    const httpsUrl = streamUrl.replace("wss:", "https:");
    const httpsSigned = createHmac("sha1", env.TWILIO_AUTH_TOKEN!).update(httpsUrl).digest("base64");
    expect(await verifyTwilioWebhookSignature(env, httpsUrl, new URLSearchParams(), httpsSigned)).toBe(true);
  });
  it("binds the exact URL and every form value", async () => {
    expect(await verifyTwilioWebhookSignature(env, url.replace(":443", ""), fields, signature)).toBe(false);
    expect(await verifyTwilioWebhookSignature(env, url.replace("%2f", "%2F"), fields, signature)).toBe(false);
    const changed = new URLSearchParams(fields); changed.append("UnknownFutureField", "new");
    expect(await verifyTwilioWebhookSignature(env, url, changed, signature)).toBe(false);
    expect(await verifyTwilioWebhookSignature({ TWILIO_AUTH_TOKEN: env.TWILIO_API_KEY_SECRET }, url, fields, signature)).toBe(false);
  });
  it("fails closed for missing, malformed, mismatched, and oversized inputs", async () => {
    for (const bad of [null, "", "x", "!".repeat(28), "A".repeat(27) + "="]) expect(await verifyTwilioWebhookSignature(env, url, fields, bad)).toBe(false);
    expect(await verifyTwilioWebhookSignature({}, url, fields, signature)).toBe(false);
    expect(await verifyTwilioWebhookSignature(env, url, new URLSearchParams({ x: "x".repeat(65536) }), signature)).toBe(false);
    expect(await verifyTwilioWebhookSignature(env, url, new URLSearchParams(Array.from({ length: 129 }, () => ["x", "y"])), signature)).toBe(false);
  });
});
