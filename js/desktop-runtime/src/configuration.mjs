import { createHash } from "node:crypto";
import { mkdir, readFile, rename, stat, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { homedir } from "node:os";
import { parseEnv } from "node:util";
import { DEFAULT_ORIGIN } from "./runtime.mjs";

export async function desktopEnvironment(file = process.env.NANOCODEX_ENV_FILE) {
  if (!file) return { ...process.env };
  try { return { ...parseEnv(await readFile(file, "utf8")), ...process.env }; }
  catch (error) { if (error.code === "ENOENT") return { ...process.env }; throw error; }
}

export async function desktopDefaults(environment = process.env) {
  const defaults = {};
  const candidates = {
    binary: [environment.NANOCODEX_HAND_BINARY, environment.NANOCODEX_ENV_FILE && join(dirname(environment.NANOCODEX_ENV_FILE), "target", "debug", "nanocodex2")],
    rootfs: [environment.NANOCODEX_VM_ROOTFS],
    guestRuntime: [environment.NANOCODEX_VM_GUEST_RUNTIME, environment.NANOCODEX_ENV_FILE && join(dirname(environment.NANOCODEX_ENV_FILE), "target", "aarch64-unknown-linux-musl", "debug", "nanocodex-vm-guest")],
  };
  await Promise.all(Object.entries(candidates).map(async ([name, paths]) => {
    for (const path of paths.filter(Boolean)) {
      try { if ((await stat(path)).isFile()) { defaults[name] = path; break; } } catch { /* Unavailable defaults stay unset. */ }
    }
  }));
  return defaults;
}

/** Preference files contain no key. A digest fences saved grants and tab drafts
 * to the account that created them. OS credential storage belongs to each app. */
export async function desktopPreferences({ directory, apiKey, baseUrl = DEFAULT_ORIGIN }) {
  const path = join(directory ?? join(homedir(), "Library", "Application Support", "Nanocodex", "Native"), "desktop.json");
  const scopeFor = connection => connection?.apiKey
    ? createHash("sha256").update(`${connection.baseUrl}\0${connection.apiKey}`).digest("hex")
    : undefined;
  let scope = scopeFor({ apiKey, baseUrl });
  let store = {};
  try { store = JSON.parse(await readFile(path, "utf8")); }
  catch (error) { if (error.code !== "ENOENT" && !(error instanceof SyntaxError)) throw error; }
  let saving = Promise.resolve();
  const write = () => {
    const contents = JSON.stringify(store);
    saving = saving.catch(() => {}).then(async () => {
      await mkdir(dirname(path), { recursive: true, mode: 0o700 });
      await writeFile(`${path}.tmp`, contents, { mode: 0o600 });
      await rename(`${path}.tmp`, path);
    });
    return saving;
  };
  return {
    saved: scope && store.scope === scope ? store.preferences ?? {} : {},
    async persist(preferences) { store = { scope, preferences }; await write(); },
    async saveConnection(connection) { scope = scopeFor(connection); },
    async close() { await saving; },
  };
}
