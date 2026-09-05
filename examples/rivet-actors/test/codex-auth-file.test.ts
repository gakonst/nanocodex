import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, test } from "vitest";

import { createCodexAuthFileProvider } from "../src/codex-auth-file.js";

const temporaryDirectories: string[] = [];

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((path) =>
    rm(path, { recursive: true, force: true })
  ));
});

describe("Codex auth-file subscription provider", () => {
  test("reads a protected login and recovers only after Codex rotates it", async () => {
    const path = await authFile("access-one");
    const provider = createCodexAuthFileProvider(path);
    const first = await provider.snapshot();

    expect(first).toMatchObject({
      bearerToken: expect.any(String),
      accountId: "account-123",
      fedramp: false,
      revision: expect.any(Number),
    });
    await expect(provider.recover(first.revision)).rejects.toThrow(/run `codex login`/);

    await writeAuth(path, "access-two");
    const rotated = await provider.recover(first.revision);
    expect(rotated.revision).not.toBe(first.revision);
    expect(rotated.bearerToken).not.toBe(first.bearerToken);
  });

  test.runIf(process.platform !== "win32")("rejects an auth file readable by other users", async () => {
    const path = await authFile("access-token", 0o644);
    await expect(createCodexAuthFileProvider(path).snapshot()).rejects.toThrow(/group or other users/);
  });

  test("rejects an access token near expiry instead of refreshing it", async () => {
    const path = await authFile("expiring", 0o600, 60);
    await expect(createCodexAuthFileProvider(path).snapshot()).rejects.toThrow(/expires too soon/);
  });
});

async function authFile(tokenMarker: string, mode = 0o600, ttlSeconds = 3_600): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-codex-auth-test-"));
  temporaryDirectories.push(directory);
  const path = join(directory, "auth.json");
  await writeAuth(path, tokenMarker, mode, ttlSeconds);
  return path;
}

async function writeAuth(
  path: string,
  tokenMarker: string,
  mode = 0o600,
  ttlSeconds = 3_600,
): Promise<void> {
  const expiresAt = Math.floor(Date.now() / 1_000) + ttlSeconds;
  const accessToken = jwt({ exp: expiresAt, marker: tokenMarker });
  const idToken = jwt({
    exp: expiresAt,
    "https://api.openai.com/auth": {
      chatgpt_account_id: "account-123",
      chatgpt_account_is_fedramp: false,
    },
  });
  await writeFile(path, JSON.stringify({
    auth_mode: "chatgpt",
    tokens: {
      access_token: accessToken,
      account_id: "account-123",
      id_token: idToken,
      refresh_token: "must-not-be-used",
    },
  }), { mode });
}

function jwt(payload: Record<string, unknown>): string {
  return `${Buffer.from("{}").toString("base64url")}.${Buffer.from(JSON.stringify(payload)).toString("base64url")}.signature`;
}
