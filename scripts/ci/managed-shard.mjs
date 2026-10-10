#!/usr/bin/env node
// Runs one CI shard of js/managed's complete "npm test" chain.
//
// The chain in js/managed/package.json is the source of truth: every
// "vitest run" or "npm run test:*" step is assigned to exactly one shard, so a
// journey added to "test" is covered in CI without editing the workflow.
// Usage: node scripts/ci/managed-shard.mjs INDEX/TOTAL  (INDEX is 1-based)
//        node scripts/ci/managed-shard.mjs --list TOTAL
import { appendFileSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const managed = fileURLToPath(new URL("../../js/managed/", import.meta.url));
const pkg = JSON.parse(readFileSync(new URL("package.json", "file://" + managed), "utf8"));

export function steps(script = pkg.scripts.test) {
  return script.split("&&").map(step => step.trim()).filter(Boolean).map(command => {
    const npm = /^npm run (\S+)$/.exec(command);
    if (npm) return { name: npm[1], command };
    if (/^vitest run( |$)/.test(command)) return { name: command, command: "pnpm exec " + command };
    throw new Error("Unsupported js/managed test step: " + command);
  });
}

// Whole scripts that ci.yml's bindings job already runs with dedicated
// artifacts ("Exercise ..." and "Test managed Claude ..." steps).
export const coveredByBindings = new Set([
  "test:sites", "test:session-control", "test:startup", "test:claude-managed",
  "test:hand-preparation", "test:cua-routing", "test:hand-paths", "test:hand-reconnect-agent",
]);

// Weighted greedy assignment keeps shards balanced as journeys are added.
// Weights are approximate CI seconds; unknown steps count as 60.
const weights = {
  "test:recovery": 300, "test:routing": 180, "test:services": 150,
  "test:provider-vault": 120, "test:agent-runs": 120, "test:connect-signin": 90, "test:crm-search": 90,
  "test:user-data": 90, "test:hosted-tools": 90, "test:apps": 90,
};
export function assign(list, total) {
  const shards = Array.from({ length: total }, () => ({ load: 0, steps: [] }));
  const ordered = list.map((step, index) => ({ ...step, index, weight: weights[step.name] ?? 60 }))
    .sort((a, b) => b.weight - a.weight || a.index - b.index);
  for (const step of ordered) {
    const target = shards.reduce((best, shard) => (shard.load < best.load ? shard : best));
    target.load += step.weight;
    target.steps.push(step);
  }
  for (const shard of shards) shard.steps.sort((a, b) => a.index - b.index);
  return shards;
}

// The bare workerd "vitest run" step is the longest by far, so every shard runs
// its own slice of it (vitest --shard); the other steps are assigned whole.
export function plan(total) {
  const all = steps().filter(step => !coveredByBindings.has(step.name));
  const whole = all.filter(step => step.name !== "vitest run");
  const split = all.some(step => step.name === "vitest run");
  return assign(whole, total).map((shard, i) => [
    ...(split ? [{ name: "vitest run --shard=" + (i + 1) + "/" + total, command: "pnpm exec vitest run --shard=" + (i + 1) + "/" + total }] : []),
    ...shard.steps,
  ]);
}

function main(argv) {
  if (argv[0] === "--list") {
    for (const [i, shard] of plan(Number(argv[1])).entries()) console.log(i + 1 + ": " + shard.map(s => s.name).join(", "));
    return;
  }
  const [index, total] = (argv[0] ?? "").split("/").map(Number);
  if (!Number.isInteger(index) || !Number.isInteger(total) || index < 1 || index > total) throw new Error("usage: managed-shard.mjs INDEX/TOTAL");
  const mine = plan(total)[index - 1];
  const results = [];
  for (const step of mine) {
    console.log("::group::" + step.name);
    const started = Date.now();
    const run = spawnSync(step.command, { cwd: managed, shell: true, stdio: "inherit" });
    const seconds = Math.round((Date.now() - started) / 1000);
    console.log("::endgroup::");
    const ok = run.status === 0;
    if (!ok) console.log("::error title=managed journey failed::" + step.name + " exited " + (run.status ?? run.signal));
    results.push({ name: step.name, ok, seconds });
  }
  const table = "| step | result | seconds |\n|---|---|---|\n" + results.map(r => "| " + r.name + " | " + (r.ok ? "pass" : "FAIL") + " | " + r.seconds + " |").join("\n") + "\n";
  console.log(table);
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, "\n### js/managed shard " + index + "/" + total + "\n\n" + table);
  if (results.some(r => !r.ok)) process.exit(1);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main(process.argv.slice(2));
