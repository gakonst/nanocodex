import { spawn } from "node:child_process";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

import { readCodexSubscription } from "./codex-auth-file.mjs";

const codexHome = process.env.CODEX_HOME ?? join(homedir(), ".codex");
const authFile = resolve(process.env.NANOCODEX_CODEX_AUTH_FILE ?? join(codexHome, "auth.json"));
const auth = await readCodexSubscription(authFile);

process.stderr.write(
  `Using the local Codex subscription access token at ${authFile}; it expires at ${new Date(auth.expiresAt).toISOString()}.\n`,
);
const child = spawn("npx", ["--yes", "vercel@58.4.4", "dev"], {
  env: {
    ...process.env,
    NANOCODEX_AUTH_MODE: "chatgpt",
    CHATGPT_ACCESS_TOKEN: auth.accessToken,
    CHATGPT_ACCOUNT_ID: auth.accountId,
    CHATGPT_FEDRAMP: String(auth.fedramp),
    WORKFLOW_SEQUENTIAL_REPLAYS: "1",
  },
  stdio: "inherit",
});
child.once("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exitCode = code ?? 1;
});
