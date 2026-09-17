import { expect, it, vi } from "vitest";
import { emailTools, type EmailConfig } from "../src/email-tool";

const context = () => ({ callId: "call", parentCallId: "", sessionId: "session", model: "test", signal: new AbortController().signal });
function fixture() {
  const execute = vi.fn().mockResolvedValue({ status: "accepted" });
  const authorize = vi.fn();
  const config: EmailConfig = { NANOCODEX_EMAIL_OWNER_ID: "owner", NANOCODEX_EMAIL_ADMIN_ID: "owner", NANOCODEX_EMAIL: { execute } };
  const tool = emailTools({ config, owner: "owner", agentId: "agent", authorize })[0]!;
  return { execute, authorize, config, tool };
}
it("does not expose another owner's mailbox or enable it in multiplayer", () => {
  const { config } = fixture();
  for (const value of [{}, { ...config, NANOCODEX_EMAIL: undefined }, { ...config, NANOCODEX_EMAIL_OWNER_ID: "other" }])
    expect(emailTools({ config: value, owner: "owner", agentId: "agent", authorize() {} })).toEqual([]);
  expect(emailTools({ config, owner: "owner", agentId: "agent", multiplayer: true, authorize() {} })).toEqual([]);
});
it("checks current authority on every invocation before service access", async () => {
  const f = fixture();
  await f.tool.handler({ operation: "status" }, context());
  f.authorize.mockImplementation(() => { throw new Error("forbidden"); });
  await expect(f.tool.handler({ operation: "list" }, context())).rejects.toThrow("forbidden");
  expect(f.execute).toHaveBeenCalledOnce();
  f.authorize.mockReset();
  f.config.NANOCODEX_EMAIL_OWNER_ID = "different";
  await expect(f.tool.handler({ operation: "status" }, context())).rejects.toThrow("unavailable");
  expect(f.execute).toHaveBeenCalledOnce();
});
it("derives owner and agent identity from the session", async () => {
  const f = fixture();
  const input = { operation: "send", operation_id: "11111111-1111-4111-8111-111111111111", to: ["test@example.com"], subject: "Hello", text: "Authorized message" };
  expect(await f.tool.handler(input, context())).toEqual({ status: "accepted" });
  expect(f.execute).toHaveBeenCalledWith({ ...input, owner_id: "owner", agent_id: "agent" });
});
it.each([
  { operation: "list", owner_id: "other" }, { operation: "send", agent_id: "other" },
  { operation: "send", from: "spoof@example.com" }, { operation: "status", to: ["other@example.com"] },
  { operation: "__proto__" }, { operation: "constructor" }, { operation: "unknown" }, null,
])("rejects unknown fields and operations before invoking the service", async input => {
  const f = fixture();
  await expect(f.tool.handler(input, context())).rejects.toThrow();
  expect(f.execute).not.toHaveBeenCalled();
});
it("does not dispatch an already canceled operation", async () => {
  const f = fixture();
  const controller = new AbortController();
  controller.abort();
  await expect(f.tool.handler({ operation: "send" }, { ...context(), signal: controller.signal })).rejects.toThrow();
  expect(f.execute).not.toHaveBeenCalled();
});
it("never retries a write after an ambiguous RPC failure or leaks provider errors", async () => {
  const f = fixture();
  f.execute.mockRejectedValue(new Error("private provider data"));
  await expect(f.tool.handler({ operation: "send" }, context())).rejects.toThrow("outcome may be unknown");
  expect(f.execute).toHaveBeenCalledOnce();
});

it.each([undefined, "", "other"])("requires the deployment email admin and rechecks revocation: %s", async admin => {
  const f = fixture();
  f.config.NANOCODEX_EMAIL_ADMIN_ID = admin;
  expect(emailTools({config:f.config,owner:"owner",agentId:"agent",authorize(){}})).toEqual([]);
  await expect(f.tool.handler({operation:"status"},context())).rejects.toThrow("unavailable");
  expect(f.execute).not.toHaveBeenCalled();
});
