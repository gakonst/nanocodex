import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = (name: string) => readFileSync(new URL(name, import.meta.url), "utf8");
const runtimeImports = (content: string) => [...content.matchAll(/^import\s+(?!type\b)[^;]+;$/gm)]
  .map(([statement]) => statement);

test("home entry does not eagerly import repository preparation or route-only UI", () => {
  const entry = runtimeImports(source("main.tsx"));
  const shell = runtimeImports(source("NanocodexApp.tsx"));
  const routeModules = [
    "routeLoaders", "CodeBrowser", "CommitCodeStream", "PierreWorkerProvider",
    "Docs", "Evals", "Changelog", "MonsterWorld", "Multiplayer",
    "DeviceConnect", "ChiefOfStaffDemo", "HostedToolsDemo",
  ];
  for (const module of routeModules) {
    for (const statement of [...entry, ...shell]) {
      assert.doesNotMatch(statement, new RegExp(`from ["']\\./${module}["']`),
        `${module} belongs behind a route interaction`);
    }
  }
  assert.match(source("main.tsx"), /import\("\.\/routeLoaders"\)/);
  assert.match(source("NanocodexApp.tsx"), /<Suspense fallback=\{<p role="status">Loading \{surface\}/);
});

test("intent preload helpers import route modules, not their data from the initial entry", () => {
  const helpers = source("routeModulePreloads.ts");
  for (const module of ["Docs", "Evals", "Changelog"]) {
    assert.match(helpers, new RegExp(`import\\("\\./${module}"\\)`));
  }
  assert.equal(runtimeImports(source("routeLoaders.ts"))
    .some((statement) => /from ["']\.\/(Docs|Evals|Changelog)["']/.test(statement)), false);
});
