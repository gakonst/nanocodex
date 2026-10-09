import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

// Each family gates jobs in .github/workflows/ci.yml (see `gate`). Draft PRs
// skip the heavy families. See README.md for the selection policy.
export const families = [
  "hands", "windows", "vm", "voice", "python", "rust", "rust_extra", "wasm_rust",
  "wasm", "bindings", "apps", "preview", "policy", "codeql",
];
const heavyFamilies = ["hands", "windows", "vm", "voice", "python", "rust_extra", "preview", "codeql"];
// Workspace packages whose build a job exercises. A change to any package in
// their dependency closure (normal, build, or dev) selects the job.
const jobRoots = {
  hands: ["nanocodex-bin", "nanocodex-hand-daemon"],
  windows: ["nanocodex-bin", "nanocodex-hand-daemon"],
  vm: ["nanocodex-vm"],
  voice: ["nanocodex-voice-native"],
  python: ["nanocodex-python"],
  wasm_rust: ["nanocodex-wasm"],
};
const appPackages = new Set([
  "account", "chief-of-staff", "connect-dialog", "connect-playground", "email",
]);
const bindingPackages = new Set(["managed", "egress", "x-api", "connect-api", "nanocodex-computer"]);
const sharedPackages = new Set([
  "nanocodex", "nanocodex-tools", "nanocodex-react", "nanocodex-vite",
  "nanocodex-terminal", "nanocodex-connect-ui", "nanocodex-connect-protocol",
]);
// Deployed by the Cloudflare workflow; no ci.yml job builds or consumes them.
const cloudflarePackages = new Set(["media"]);
const bindingExamples = new Set(["node", "react-vite", "browser-cdn", "privy", "better-auth"]);
const jsSource = /\.(?:[cm]?[jt]sx?|jsonc?|css|html|svg|png|jpe?g|gif|webp|ico|woff2?|sql)$/;
const binaryAsset = /\.(?:png|jpe?g|gif|webp|ico|mp4|wav|woff2?)$/;
const rustInput = /(?:\.rs|\/Cargo\.toml)$/;
// Files read by a package outside its own directory.
const crossPackageInputs = {
  "js/nanocodex-tools/runtime/code-tools.mjs": "nanocodex-oai-tools", // Rust embedded copy; npm name is unchanged
};
// Workflow definitions and actions that ci.yml runs. Any change runs everything.
const ciDefinitions = /^\.github\/(?:actions\/|workflows\/(?:ci|js-preview)\.yml$)/;

const all = value => Object.fromEntries(families.map(name => [name, value]));
const full = () => ({ jobs: all(true), packages: "*" });

/** Workspace package graph from `cargo metadata --locked --no-deps` (no registry access). */
export function loadGraph(cwd = process.cwd()) {
  const meta = JSON.parse(execFileSync("cargo", ["metadata", "--locked", "--no-deps", "--format-version", "1", "--offline"], {
    cwd, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"],
  }));
  const root = meta.workspace_root;
  const names = new Set(meta.packages.map(p => p.name));
  const dirs = [];
  const external = Object.entries(crossPackageInputs);
  const dependents = new Map([...names].map(name => [name, new Set()]));
  for (const pkg of meta.packages) {
    const dir = relative(root, dirname(pkg.manifest_path));
    dirs.push([dir, pkg.name]);
    // Out-of-directory targets own their sibling modules under crates/ and
    // bin/; elsewhere only the file.
    for (const target of pkg.targets) {
      const src = relative(root, target.src_path);
      if (src.startsWith(dir + "/")) continue;
      external.push([/^(?:crates|bin)\//.test(src) ? dirname(src) + "/" : src, pkg.name]);
    }
    for (const dep of pkg.dependencies) {
      if (dep.path && names.has(dep.name)) dependents.get(dep.name).add(pkg.name);
    }
  }
  dirs.sort((a, b) => b[0].length - a[0].length);
  return { dirs, external, dependents };
}

function owners(path, graph) {
  const found = new Set(graph.external
    .filter(([prefix]) => prefix.endsWith("/") ? path.startsWith(prefix) : path === prefix)
    .map(([, name]) => name));
  const owner = graph.dirs.find(([dir]) => path.startsWith(dir + "/"));
  // js/, py/ and the examples root mix Rust with other languages; only Rust
  // inputs there feed Cargo. Crate directories embed arbitrary assets.
  const mixed = owner && (owner[0] === "examples" || /^(?:js|py)\//.test(owner[0]));
  if (owner && (!mixed || rustInput.test(path))) found.add(owner[1]);
  return found;
}

function withDependents(changed, graph) {
  const seen = new Set(changed);
  const queue = [...changed];
  while (queue.length) for (const next of graph.dependents.get(queue.pop()) ?? []) {
    if (!seen.has(next)) { seen.add(next); queue.push(next); }
  }
  return seen;
}

/** Map changed paths to CI families. `graph` null means Cargo is unavailable. */
export function selectJobs(paths, graph) {
  if (!graph) return full();
  const jobs = all(false);
  const changed = new Set();
  for (const path of paths) {
    const parts = path.split("/");
    const name = parts.at(-1);
    // Workspace-wide inputs and unsafe paths run everything.
    if (parts.some(part => part === ".." || part === "." || part === "")
      || /^(Cargo\.lock|rust-toolchain(?:\.toml)?)$/.test(name) || path === "Cargo.toml" || path.startsWith(".cargo/")
      || /^(package(?:-lock)?\.json|npm-shrinkwrap\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|yarn\.lock|bun\.lockb?|\.npmrc|\.pnpmfile\.cjs|turbo\.json)$/.test(name)
      || ciDefinitions.test(path) || path.startsWith("scripts/ci/")) return full();
    if (!binaryAsset.test(path)) jobs.policy = true; // Retain spelling checks for source and prose.
    // The terminal private-input journey crosses these browser and Vault boundaries.
    if (/^js\/(?:managed\/(?:src\/(?:browser-|vault-|credentials\.|index\.)|test\/private-input-)|egress\/src\/(?:broker\.|egress\.|vault-|credential-vault\.)|account\/worker\/managedProxy\.)/.test(path)) {
      jobs.hands = true;
    }
    // The real CLI and server_hand device-key journeys run in the shared Hand job.
    if (/^js\/(?:managed\/(?:src\/(?:hand-device|ssh-hand-setup\.)|test\/(?:support\/)?(?:hand-device-|server-hand\/))|egress\/src\/ssh\.)/.test(path)) jobs.hands = true;
    const packages = owners(path, graph);
    for (const pkg of packages) changed.add(pkg);
    if (packages.size && !/^(?:js|py|examples)\//.test(path)) continue;
    if (path === "js/nanocodex-tools/runtime/code-tools.mjs") {
      jobs.apps = jobs.bindings = jobs.preview = true;
    } else if (packages.size && rustInput.test(path)) {
      // Rust inside js/, py/ or examples/ is fully described by its package.
    } else if (path.startsWith("scripts/tests/openai-cua-")) {
      jobs.hands = jobs.windows = jobs.vm = true; // Bridges embedded by the native Hand.
    } else if (path.startsWith("scripts/cloudflare/")) {
      // Service release ordering is also checked while broad tests are paused.
      if (/^scripts\/cloudflare\/(?:deploy-workers|release-workers|release-plan)(?:\.test)?\.mjs$/.test(path)) jobs.bindings = true;
      // Other Cloudflare workflow inputs have script tests in the policy job.
    } else if (path.startsWith("third_party/codex-voice/") || path === "scripts/build-voice-native.py") {
      jobs.voice = true;
    } else if (path.startsWith("py/") || path.startsWith("examples/python/")) {
      jobs.python = true;
    } else if (path.startsWith("docs/")
      || /^(README\.md|CHANGELOG\.md|AGENTS\.md|LICENSE-APACHE|LICENSE-MIT)$/.test(path)) {
      // Documentation does not require compiled artifacts.
    } else if (/^(apple|macos)\//.test(path)) {
      // Native Apple source/build inputs are checked by the separate Apple workflows.
    } else if (path.startsWith("js/desktop-runtime/")) {
      jobs.hands = jobs.windows = true;
    } else if (path.startsWith("windows/") || path === "install.ps1") {
      jobs.windows = true;
    } else if (path.startsWith(".github/")) {
      jobs.codeql = true; // Other workflows run separately; CodeQL still audits them.
    } else if (["js/account/worker/index.ts", "js/account/worker/managedProxy.ts",
      "js/managed/src/browser-runtime.ts", "js/managed/src/browser-vault-totp.ts",
      "js/managed/test/browser-vault-totp.chrome.mjs",
      "js/egress/src/phone-service.ts", "js/egress/src/vault-totp.ts"].includes(path)) {
      // The public phone HTTP journey consumes the account entrypoint from
      // bindings; private browser service journeys consume managed from apps.
      jobs.apps = jobs.bindings = true;
    } else if (parts[0] === "js" && parts.length > 2 && cloudflarePackages.has(parts[1])) {
      // Checked by the Cloudflare workflow's own path filter.
    } else if (parts[0] === "js" && parts.length > 2
      && (appPackages.has(parts[1]) || bindingPackages.has(parts[1]) || sharedPackages.has(parts[1]))) {
      if (/\.md$/.test(path)) continue;
      if (!path.startsWith("js/nanocodex-vite/scripts/") && !jsSource.test(path)) return full();
      if (appPackages.has(parts[1]) || sharedPackages.has(parts[1])) jobs.apps = true;
      if (bindingPackages.has(parts[1]) || sharedPackages.has(parts[1])) jobs.bindings = true;
      if (["nanocodex", "nanocodex-vite", "nanocodex-tools"].includes(parts[1])) jobs.preview = true;
    } else if (parts[0] === "examples" && parts.length > 2 && jsSource.test(path)
      && (bindingExamples.has(parts[1]) || parts[1] === "astra-mpp-trial")) {
      if (parts[1] === "astra-mpp-trial") jobs.apps = true;
      else jobs.bindings = true;
    } else {
      return full();
    }
  }
  const hit = withDependents(changed, graph);
  for (const [family, roots] of Object.entries(jobRoots)) {
    if (roots.some(root => hit.has(root))) jobs[family] = true;
  }
  jobs.rust = jobs.rust_extra = hit.size > 0;
  // WASM consumers must retest against changed Rust bindings.
  if (jobs.wasm_rust) jobs.apps = jobs.bindings = jobs.preview = true;
  return { jobs, packages: [...hit].sort().join(" ") };
}

export function changedPaths(eventName, event, cwd = process.cwd()) {
  let base, head, separator;
  if (eventName === "pull_request") {
    base = event.pull_request?.base?.sha;
    head = event.pull_request?.head?.sha;
    separator = "..."; // Compare the PR to its merge base, not the moving base tip.
  } else if (eventName === "push") {
    base = event.before;
    head = event.after;
    separator = "..";
  } else {
    // merge_group, schedule and dispatch always verify everything.
    throw new Error(`full CI for event ${eventName || "unknown"}`);
  }
  for (const sha of [base, head]) {
    if (typeof sha !== "string" || !/^[a-f0-9]{40,64}$/i.test(sha) || /^0+$/.test(sha)) {
      throw new Error("missing, invalid, or zero diff endpoint");
    }
  }
  // NUL delimiters preserve unusual filenames; disabling renames retains both
  // the old deleted path and the new path when files move across categories.
  const output = execFileSync("git", ["diff", "--name-only", "-z", "--no-renames", `${base}${separator}${head}`, "--"], {
    cwd, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"],
  });
  return output.split("\0").filter(Boolean);
}

export function selectionForEvent(eventName, event, cwd) {
  let result;
  try {
    const paths = changedPaths(eventName, event, cwd);
    let graph = null;
    try { graph = loadGraph(cwd); } catch { /* full CI below */ }
    result = { ...selectJobs(paths, graph), reason: graph ? `classified ${paths.length} changed path(s)` : "full CI: cargo metadata unavailable" };
  } catch {
    // Missing history, malformed events and unsupported event types fail open.
    result = { ...full(), reason: "full CI: event or diff unavailable/unsupported" };
  }
  // Draft pull requests get the fast lane; marking ready reruns everything.
  result.heavy = !(eventName === "pull_request" && event?.pull_request?.draft === true);
  if (!result.heavy) for (const family of heavyFamilies) result.jobs[family] = false;
  // Commit previews are published for PR heads; a merge-queue candidate
  // becomes the next master push, which publishes its own.
  if (eventName === "merge_group") result.jobs.preview = false;
  result.jobs.wasm = result.jobs.bindings || result.jobs.apps || result.jobs.preview;
  return result;
}

// ci.yml job id -> whether the selection requires it (success) or not (skipped).
const gate = {
  changes: () => true,
  test: o => o.tests && o.rust_extra,
  "shared-hands": o => o.hands,
  "voice-native": o => o.voice,
  "windows-hand": o => o.windows,
  clippy: o => o.rust,
  "rust-extra": o => o.rust_extra,
  "vm-guest": o => o.vm,
  policy: o => o.policy,
  "wasm-build": o => o.wasm,
  "js-preview": o => o.preview,
  "wasm-quality": o => o.wasm_rust,
  bindings: o => o.bindings,
  python: o => o.python,
  apps: o => o.apps,
  codeql: o => o.codeql,
};

/** Check `toJSON(needs)` against the selection. Returns a list of violations. */
export function verify(needs) {
  const raw = needs?.changes?.outputs ?? {};
  const outputs = {};
  for (const key of [...families, "tests"]) {
    if (raw[key] !== "true" && raw[key] !== "false") return [`selection output ${key} is ${JSON.stringify(raw[key])}`];
    outputs[key] = raw[key] === "true";
  }
  const problems = [];
  for (const job of new Set([...Object.keys(gate), ...Object.keys(needs)])) {
    if (!gate[job]) { problems.push(`${job} is not mapped in select-jobs.mjs`); continue; }
    const want = gate[job](outputs) ? "success" : "skipped";
    const got = needs[job]?.result;
    if (got !== want) problems.push(`${job}: expected ${want}, got ${got ?? "missing"}`);
  }
  return problems;
}

export function main(env = process.env) {
  if (process.argv[2] === "verify") {
    const problems = verify(JSON.parse(env.NEEDS ?? "{}"));
    for (const problem of problems) console.error(`::error title=ci success::${problem}`);
    if (problems.length) process.exit(1);
    console.log("every selected job succeeded and every other job was skipped");
    return;
  }
  let result;
  try {
    result = selectionForEvent(env.GITHUB_EVENT_NAME, JSON.parse(readFileSync(env.GITHUB_EVENT_PATH, "utf8")));
  } catch {
    result = { ...full(), heavy: true, reason: "full CI: event payload unavailable/invalid" };
  }
  // The reusable publisher only runs in the upstream repository. Reflect that
  // restriction in the required-job gate instead of accepting unexpected skips.
  if (env.GITHUB_REPOSITORY && env.GITHUB_REPOSITORY !== "gakonst/nanocodex") result.jobs.preview = false;
  // Owner switch for the paused test steps; independent of path selection.
  const tests = env.NANOCODEX_CI_TESTS === "on";
  const outputs = [...Object.entries(result.jobs), ["tests", tests], ["packages", result.packages], ["heavy", result.heavy]]
    .map(([key, value]) => `${key}=${value}`).join("\n") + "\n";
  if (env.GITHUB_OUTPUT) appendFileSync(env.GITHUB_OUTPUT, outputs);
  const summary = `CI job selection (${result.reason})\n${outputs}`;
  console.log(summary);
  if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY, `\n\`\`\`text\n${summary}\`\`\`\n`);
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
