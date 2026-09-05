import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { createCodexAuthFileProvider } from "../src/codex-auth-file.js";

const cloudToken = process.env.RIVET_CLOUD_TOKEN?.trim();
const codexHome = process.env.CODEX_HOME ?? join(homedir(), ".codex");
const authFile = resolve(process.env.NANOCODEX_CODEX_AUTH_FILE ?? join(codexHome, "auth.json"));
const auth = await createCodexAuthFileProvider(authFile).snapshot();
const namespace = process.env.RIVET_NAMESPACE?.trim() || "production";
const refreshToken = process.env.CHATGPT_REFRESH_TOKEN?.trim();
const disposableCredentialId = createHash("sha256")
  .update(auth.bearerToken)
  .digest("hex")
  .slice(0, 16);
const actorKey = process.env.NANOCODEX_AUTH_ACTOR_KEY?.trim()
  || (refreshToken
    ? `nanocodex-subscription-${namespace}`
    : `nanocodex-subscription-${namespace}-${disposableCredentialId}`);
const capability = process.env.NANOCODEX_AUTH_CAPABILITY?.trim()
  || randomBytes(32).toString("base64url");
const repository = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");

const environment = [
  "NANOCODEX_AUTH_MODE=chatgpt",
  `NANOCODEX_AUTH_ACTOR_KEY=${actorKey}`,
  `NANOCODEX_AUTH_CAPABILITY=${capability}`,
  `CHATGPT_ACCESS_TOKEN=${auth.bearerToken}`,
  `CHATGPT_ACCOUNT_ID=${auth.accountId}`,
  `CHATGPT_FEDRAMP=${String(auth.fedramp)}`,
];
const publicUrl = process.env.NANOCODEX_PUBLIC_URL?.trim();
if (publicUrl) environment.push(`NANOCODEX_PUBLIC_URL=${publicUrl}`);
if (refreshToken) environment.push(`CHATGPT_REFRESH_TOKEN=${refreshToken}`);

process.stderr.write(
  `Deploying the current Codex subscription access token to Rivet namespace ${namespace}; `
    + `${refreshToken ? "dedicated refresh is enabled" : "no refresh token will be copied"}.\n`,
);
const child = spawn("npx", [
  "@rivetkit/cli@2.3.10",
  "deploy",
  ...(cloudToken ? ["--token", cloudToken] : []),
  "--dockerfile",
  "examples/rivet-actors/Dockerfile",
  "--build-context",
  ".",
  "--namespace",
  namespace,
  ...(process.env.RIVET_REUSE_IMAGE === "1" ? ["--reuse-image"] : []),
  ...environment.flatMap((value) => ["--env", value]),
], {
  cwd: repository,
  env: process.env,
  stdio: "inherit",
});

const exit = await new Promise<{ code: number | null; signal: NodeJS.Signals | null }>(
  (resolveExit, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolveExit({ code, signal }));
  },
);
if (exit.code !== 0) {
  throw new Error(exit.signal
    ? `Rivet deployment exited on ${exit.signal}`
    : `Rivet deployment exited with status ${String(exit.code)}`);
}

function requiredSecret(name: string): string {
  const value = process.env[name];
  if (!value?.trim()) throw new Error(`${name} is not configured`);
  return value;
}
