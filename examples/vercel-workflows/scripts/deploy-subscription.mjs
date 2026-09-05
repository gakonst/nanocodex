import { spawn } from "node:child_process";
import { cp, mkdtemp, readFile, rm, unlink, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { readCodexSubscription } from "./codex-auth-file.mjs";

const exampleRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryRoot = resolve(exampleRoot, "../..");
const codexHome = process.env.CODEX_HOME ?? join(homedir(), ".codex");
const authFile = resolve(process.env.NANOCODEX_CODEX_AUTH_FILE ?? join(codexHome, "auth.json"));
const project = process.env.VERCEL_PROJECT?.trim() || "nanocodex-vercel-workflows";
const scope = process.env.VERCEL_SCOPE?.trim();
const scopeArgs = scope ? ["--scope", scope] : [];
const excludedStagingNames = new Set([
  ".next",
  ".swc",
  ".vercel",
  ".workflow-data",
  "node_modules",
  "tsconfig.tsbuildinfo",
]);
const auth = await readCodexSubscription(authFile);

process.stderr.write(
  `Deploying access-token-only ChatGPT subscription auth to Vercel project ${project}; token expires at ${new Date(auth.expiresAt).toISOString()}.\n`,
);
await run(
  "npx",
  [
    "--yes",
    "vercel@58.4.4",
    "link",
    "--yes",
    "--project",
    project,
    ...scopeArgs,
  ],
  exampleRoot,
);
await addEnvironment("NANOCODEX_AUTH_MODE", "chatgpt", false);
await addEnvironment("CHATGPT_ACCESS_TOKEN", auth.accessToken, true);
await addEnvironment("CHATGPT_ACCOUNT_ID", auth.accountId, true);
await addEnvironment("CHATGPT_FEDRAMP", String(auth.fedramp), false);
await addEnvironment("WORKFLOW_SEQUENTIAL_REPLAYS", "1", false);
if (process.env.NANOCODEX_ADMIN_TOKEN?.trim()) {
  await addEnvironment("NANOCODEX_ADMIN_TOKEN", process.env.NANOCODEX_ADMIN_TOKEN.trim(), true);
}
if (process.env.NANOCODEX_TERMINAL_TOKEN?.trim()) {
  await addEnvironment(
    "NANOCODEX_TERMINAL_TOKEN",
    process.env.NANOCODEX_TERMINAL_TOKEN.trim(),
    true,
  );
}

await run("./js/nanocodex-vite/scripts/build-js-package.sh", [], repositoryRoot);
const temporaryRoot = await mkdtemp(join(tmpdir(), "nanocodex-vercel-deploy-"));
const stagingRoot = join(temporaryRoot, "app");
try {
  await cp(exampleRoot, stagingRoot, {
    recursive: true,
    filter: (source) => {
      const name = basename(source);
      return !name.startsWith(".env") && !excludedStagingNames.has(name);
    },
  });
  const packed = (await capture(
    "npm",
    ["pack", resolve(repositoryRoot, "js/nanocodex"), "--pack-destination", temporaryRoot],
    repositoryRoot,
  )).trim().split("\n").at(-1);
  if (!packed) throw new Error("npm pack did not return an archive name");
  const archive = basename(packed);
  await cp(join(temporaryRoot, archive), join(stagingRoot, archive));
  const packagePath = join(stagingRoot, "package.json");
  const packageJson = JSON.parse(await readFile(packagePath, "utf8"));
  packageJson.dependencies.nanocodex = `file:./${archive}`;
  await writeFile(packagePath, `${JSON.stringify(packageJson, null, 2)}\n`);
  await unlink(join(stagingRoot, "package-lock.json")).catch(() => {});
  await run(
    "npx",
    ["--yes", "npm@11.6.2", "install", "--package-lock-only", "--ignore-scripts"],
    stagingRoot,
  );
  await run(
    "npx",
    ["--yes", "vercel@58.4.4", "link", "--yes", "--project", project, ...scopeArgs],
    stagingRoot,
  );
  await run(
    "npx",
    ["--yes", "vercel@58.4.4", "deploy", "--prod", "--yes", ...scopeArgs],
    stagingRoot,
  );
} finally {
  await rm(temporaryRoot, { recursive: true, force: true });
}

async function addEnvironment(name, value, sensitive) {
  await runWithInput(
    "npx",
    [
      "--yes",
      "vercel@58.4.4",
      "env",
      "add",
      name,
      "production",
      "--force",
      sensitive ? "--sensitive" : "--no-sensitive",
      "--yes",
    ],
    exampleRoot,
    value,
  );
}

function run(command, args, cwd) {
  return child(command, args, cwd, undefined, "inherit");
}

async function capture(command, args, cwd) {
  return child(command, args, cwd, undefined, "pipe");
}

function runWithInput(command, args, cwd, input) {
  return child(command, args, cwd, input, "inherit");
}

function child(command, args, cwd, input, stdout) {
  return new Promise((resolveChild, rejectChild) => {
    const process = spawn(command, args, {
      cwd,
      env: processEnv(),
      stdio: [input === undefined ? "inherit" : "pipe", stdout, "inherit"],
    });
    let output = "";
    if (stdout === "pipe") {
      process.stdout.setEncoding("utf8");
      process.stdout.on("data", (chunk) => { output += chunk; });
    }
    process.once("error", rejectChild);
    process.once("exit", (code, signal) => {
      if (code === 0) resolveChild(output);
      else rejectChild(new Error(signal
        ? `${command} exited on ${signal}`
        : `${command} exited with status ${String(code)}`));
    });
    if (input !== undefined) process.stdin.end(input);
  });
}

function processEnv() {
  return { ...process.env, NO_COLOR: "1" };
}
