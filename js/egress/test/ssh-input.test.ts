import { expect, it } from "vitest";
import { executeBrokeredSsh, validateBrokeredSshRequest } from "../src/ssh";

const request = { identity_ref: "server", hostname: "server.example", port: 22, username: "deploy", command: ["cat"] };

it("preserves bounded SSH stdin and rejects non-string or oversized UTF-8 input", () => {
  expect(validateBrokeredSshRequest(request)).not.toHaveProperty("stdin");
  for (const stdin of ["", "credential\n", "\0", "é".repeat(32 * 1024)]) {
    expect(validateBrokeredSshRequest({ ...request, stdin })?.stdin).toBe(stdin);
  }
  for (const stdin of [null, 7, {}, "é".repeat(32 * 1024 + 1)]) {
    expect(validateBrokeredSshRequest({ ...request, stdin })).toBeUndefined();
  }
});

it("rejects a mismatched vault target before opening a socket or sending stdin", async () => {
  const identity = { privateKey: "unused fixture", hostname: "another.example", port: 22, username: "deploy", hostKeySha256: "SHA256:" + "a".repeat(43) };
  const input = validateBrokeredSshRequest({ ...request, stdin: "private input" })!;
  let connected = false;
  await expect(executeBrokeredSsh(identity, input, undefined, () => { connected = true; throw new Error("unexpected connection"); }))
    .rejects.toThrow("ssh_identity_target_mismatch");
  expect(connected).toBe(false);
});
