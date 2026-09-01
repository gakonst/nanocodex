import { randomBytes } from "node:crypto";
import { mkdir } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const webRoot = resolve(dirname(scriptPath), "..");
const rawPort = process.env.NANOCODEX_CONNECT_DEV_PORT?.trim() || "5190";
if (!/^[1-9][0-9]*$/.test(rawPort) || Number(rawPort) > 65_535) {
  throw new Error("NANOCODEX_CONNECT_DEV_PORT must be a valid TCP port");
}

const port = Number(rawPort);
const publicOrigin = `http://localhost:${port}`;
const statePath = resolve(homedir(), ".nanocodex", "web-development");

// This intentionally avoids the repository .env and provider credential
// discovery. The Connect UI remains usable without OAuth client credentials;
// individual provider handoffs simply remain unavailable in this lightweight
// local stack.
for (const name of [
  "NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_ID",
  "NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_SECRET",
  "NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_ID",
  "NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_SECRET",
  "NANOCODEX_LOCAL_X_OAUTH_CLIENT_ID",
  "NANOCODEX_LOCAL_X_OAUTH_CLIENT_SECRET",
  "NANOCODEX_LOCAL_SLACK_OAUTH_CLIENT_ID",
  "NANOCODEX_LOCAL_SLACK_OAUTH_CLIENT_SECRET",
]) delete process.env[name];

Object.assign(process.env, {
  CLOUDFLARE_ENV: "development",
  CLOUDFLARE_INCLUDE_PROCESS_ENV: "false",
  CLOUDFLARE_LOAD_DEV_VARS_FROM_DOT_ENV: "false",
  GIT_MIRROR_TOKEN: randomBytes(32).toString("base64url"),
  NANOCODEX_LOCAL_ADMIN_TOKEN: randomBytes(32).toString("base64url"),
  NANOCODEX_LOCAL_AGENT_IDLE_TIMEOUT_MS: "1000",
  NANOCODEX_LOCAL_PUBLIC_ORIGIN: publicOrigin,
  NANOCODEX_LOCAL_STATE_PATH: statePath,
});

await mkdir(statePath, { recursive: true });

// Import Vite only after the local Worker environment is complete because the
// configuration constructs its auxiliary Worker topology during module load.
const { createServer } = await import("vite");
const server = await createServer({
  configFile: resolve(webRoot, "vite.config.ts"),
  envDir: false,
  server: { host: "127.0.0.1", port, strictPort: true },
});

await server.listen();

console.log(`\nNanocodex Connect is ready at ${publicOrigin}/connect`);
console.log("Start the CLI wizard with:");
console.log(`NANOCODEX_CONNECT_DEVICE_BASE_URL=${publicOrigin}/v1/device nanocodex login\n`);

let closing = false;
async function close() {
  if (closing) return;
  closing = true;
  await server.close();
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => {
    close().then(() => process.exit(0), (error) => {
      console.error(error);
      process.exit(1);
    });
  });
}
