import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

// WASM means an artifact is needed, including cache reuse for JS-only checks.
// Rust separately controls workspace quality and WASM-target Clippy.
const families = ["native", "voice", "python", "rust", "wasm", "bindings", "apps", "preview", "policy", "codeql"];
const full = () => Object.fromEntries(families.map(name => [name, true]));
const none = () => Object.fromEntries(families.map(name => [name, false]));
const appPackages = new Set([
  "account", "chief-of-staff", "connect-dialog", "connect-playground", "email",
]);
const bindingPackages = new Set(["managed", "egress", "x-api", "connect-api", "nanocodex-computer"]);
const sharedPackages = new Set([
  "nanocodex", "nanocodex-tools", "nanocodex-react", "nanocodex-vite",
  "nanocodex-terminal", "nanocodex-connect-ui", "nanocodex-connect-protocol",
]);
const bindingExamples = new Set(["node", "react-vite", "browser-cdn", "privy", "better-auth"]);
const jsSource = /\.(?:[cm]?[jt]sx?|jsonc?|css|html|svg|png|jpe?g|gif|webp|ico|woff2?)$/;
const binaryAsset = /\.(?:png|jpe?g|gif|webp|ico|mp4|wav|woff2?)$/;

// These bridge scripts are embedded by provision.rs in the native Hand helper.
// Keep its platform matrix, but they do not feed voice or Python artifacts.
// Exact paths leave new build inputs and unknown CUA files conservative.
const cuaNativePaths = new Set([
  "crates/experimental/nanocodex-computer/src/openai-cua-app-server.mjs",
  "crates/experimental/nanocodex-computer/src/openai-cua-native-host.mjs",
  "crates/experimental/nanocodex-computer/src/openai-cua-gui-readiness.mjs",
  "scripts/tests/openai-cua-app-server.test.mjs",
  "scripts/tests/openai-cua-native-host.test.mjs",
  "scripts/tests/openai-cua-gui-readiness.test.mjs",
  "scripts/tests/openai-cua-headless-upstream.test.mjs",
]);

export function selectJobs(paths) {
  const jobs = none();
  for (const path of paths) {
    const parts = path.split("/");
    const name = parts.at(-1);
    // Resolve unsafe/unknown configuration before any directory allowlist.
    if (parts.some(part => part === ".." || part === "." || part === "")
      || /^(Cargo\.(toml|lock)|rust-toolchain(?:\.toml)?)$/.test(name)
      || /^(package(?:-lock)?\.json|npm-shrinkwrap\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|yarn\.lock|bun\.lockb?|\.npmrc|\.pnpmfile\.cjs|turbo\.json)$/.test(name)) return full();
    if (!binaryAsset.test(path)) jobs.policy = true; // Retain spelling checks for source and prose.
    if (cuaNativePaths.has(path)) {
      jobs.native = true;
    } else if (/^(crates|bin|scripts|\.github|\.cargo|third_party)\//.test(path)) {
      return full();
    } else if (path.startsWith("py/") || path.startsWith("examples/python/")) {
      jobs.python = true;
      if (path.endsWith(".rs")) jobs.rust = true;
    } else if (path.endsWith(".rs")) {
      return full();
    } else if (path.startsWith("docs/")
      || /^(README\.md|CHANGELOG\.md|AGENTS\.md|next-steps\.md|LICENSE-APACHE|LICENSE-MIT)$/.test(path)) {
      // Documentation does not require compiled artifacts.
    } else if (/^(apple|macos)\//.test(path)) {
      // Native Apple source/build inputs are checked by the separate Apple workflows.
    } else if (path.startsWith("js/desktop-runtime/") || path.startsWith("windows/")) {
      jobs.native = true;
    } else if (parts[0] === "js" && parts.length > 2
      && (appPackages.has(parts[1]) || bindingPackages.has(parts[1]) || sharedPackages.has(parts[1]))) {
      if (/\.md$/.test(path)) continue;
      // The SDK embeds a generated copy in Rust; keep its canonical source conservative.
      if (path === "js/nanocodex-tools/runtime/code-tools.mjs") return full();
      if (path.startsWith("js/nanocodex-vite/scripts/")) {
        jobs.rust = true; // WASM build/cache orchestration is Rust build input.
      } else if (!jsSource.test(path)) {
        return full();
      }
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
  // Every selected consumer downloads the same-run WASM artifact. Never allow
  // an intentionally skipped producer to silently skip a required consumer.
  jobs.wasm = jobs.bindings || jobs.apps || jobs.preview;
  return jobs;
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
  try {
    const paths = changedPaths(eventName, event, cwd);
    return { jobs: selectJobs(paths), reason: `classified ${paths.length} changed path(s)` };
  } catch {
    // Missing history, malformed events and unsupported event types fail open.
    return { jobs: full(), reason: "full CI: event or diff unavailable/unsupported" };
  }
}

export function main(env = process.env) {
  let result;
  try {
    result = selectionForEvent(env.GITHUB_EVENT_NAME, JSON.parse(readFileSync(env.GITHUB_EVENT_PATH, "utf8")));
  } catch {
    result = { jobs: full(), reason: "full CI: event payload unavailable/invalid" };
  }
  // The reusable publisher only runs in the upstream repository. Reflect that
  // restriction in the required-job gate instead of accepting unexpected skips.
  if (env.GITHUB_REPOSITORY && env.GITHUB_REPOSITORY !== "gakonst/nanocodex") result.jobs.preview = false;
  const outputs = Object.entries(result.jobs).map(([key, value]) => `${key}=${value}`).join("\n") + "\n";
  if (env.GITHUB_OUTPUT) appendFileSync(env.GITHUB_OUTPUT, outputs);
  const summary = `CI job selection (${result.reason})\n${outputs}`;
  console.log(summary);
  if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY, `\n\`\`\`text\n${summary}\`\`\`\n`);
  return result.jobs;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
