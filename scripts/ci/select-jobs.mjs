import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const full = () => ({ native: true, voice: true, python: true });
const none = () => ({ native: false, voice: false, python: false });
// These app/UI sources are not inputs to the gated native or Python jobs.
// Shared JS runtime packages deliberately remain unknown (and run everything).
const webPackages = new Set([
  "account", "chief-of-staff", "connect-dialog",
  "connect-playground", "egress", "email", "managed",
  "nanocodex-connect-ui", "nanocodex-react", "nanocodex-terminal", "x-api",
]);

export function selectJobs(paths) {
  const jobs = none();
  for (const path of paths) {
    const parts = path.split("/");
    const name = parts.at(-1);
    // Check build/configuration inputs before documentation or app allowlists.
    if (/^(crates|bin|scripts|\.github|\.cargo|third_party)\//.test(path)
      || /^(Cargo\.(toml|lock)|rust-toolchain(?:\.toml)?)$/.test(name)
      || /^(package(?:-lock)?\.json|npm-shrinkwrap\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|yarn\.lock|bun\.lockb?|\.npmrc|\.pnpmfile\.cjs|turbo\.json)$/.test(name)
      || parts.some(part => part === ".." || part === "." || part === "")) return full();
    if (path.startsWith("js/desktop-runtime/") || path.startsWith("windows/")) {
      jobs.native = true;
    } else if (path.startsWith("py/") || path.startsWith("examples/python/")) {
      jobs.python = true;
    } else if (path.startsWith("docs/")
      || /^(README\.md|CHANGELOG\.md|AGENTS\.md|next-steps\.md|LICENSE-APACHE|LICENSE-MIT)$/.test(path)
      || (parts[0] === "js" && webPackages.has(parts[1]) && parts.length > 2)) {
      // Always-on Rust, WASM, bindings, apps, quality and policy jobs still run.
    } else {
      return full();
    }
  }
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
  const outputs = Object.entries(result.jobs).map(([key, value]) => `${key}=${value}`).join("\n") + "\n";
  if (env.GITHUB_OUTPUT) appendFileSync(env.GITHUB_OUTPUT, outputs);
  const summary = `CI job selection (${result.reason})\n${outputs}`;
  console.log(summary);
  if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY, `\n\`\`\`text\n${summary}\`\`\`\n`);
  return result.jobs;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
