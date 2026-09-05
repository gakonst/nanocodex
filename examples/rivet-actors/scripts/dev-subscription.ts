import { homedir } from "node:os";
import { join, resolve } from "node:path";

import { createCodexAuthFileProvider } from "../src/codex-auth-file.js";

const codexHome = process.env.CODEX_HOME ?? join(homedir(), ".codex");
const authFile = resolve(process.env.NANOCODEX_CODEX_AUTH_FILE ?? join(codexHome, "auth.json"));
const stateRoot = process.env.XDG_STATE_HOME ?? join(homedir(), ".local", "state");

process.env.NANOCODEX_AUTH_MODE = "chatgpt";
process.env.NANOCODEX_CODEX_AUTH_FILE = authFile;
process.env.RIVET__file_system__path ??= join(
  stateRoot,
  "nanocodex",
  "rivet-subscription-demo",
  "engine-db",
);

await createCodexAuthFileProvider(authFile).snapshot();
process.stderr.write(
  `Using the local Codex subscription login at ${authFile}; refresh tokens are not used or persisted.\n`,
);
await import("../src/server.js");
