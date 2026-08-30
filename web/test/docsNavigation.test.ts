import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import test from "node:test";

const navigation = readFileSync(new URL("../src/docsNavigation.ts", import.meta.url), "utf8");
const docs = readFileSync(new URL("../src/Docs.tsx", import.meta.url), "utf8");
const application = readFileSync(new URL("../src/NanocodexApp.tsx", import.meta.url), "utf8");

test("documentation manifest has four task-oriented top-level groups", () => {
  const using = navigation.indexOf('label: "USING NANOCODEX"');
  const evaluating = navigation.indexOf('label: "EVALUATING NANOCODEX"');
  const embedding = navigation.indexOf('label: "EMBEDDING NANOCODEX"');
  const advanced = navigation.indexOf('label: "ADVANCED FUNCTIONALITY"');
  assert.ok(using >= 0 && using < evaluating && evaluating < embedding && embedding < advanced);
  assert.equal(navigation.match(/^    label: "[A-Z][A-Z ]+",$/gm)?.length, 4);

  for (const route of [
    "/docs/getting-started",
    "/docs/architecture",
    "/docs/architecture/managed",
    "/docs/architecture/tools-execution",
    "/docs/architecture/durability-portability",
    "/docs/evals",
    "/docs/harness/focused-run",
    "/docs/harness/evidence",
    "/docs/harness/dashboard-worksets",
    "/docs/harness/read-results",
    "/docs/sdks/rust",
    "/docs/sdks/rust/agent-lifecycle",
    "/docs/sdks/rust/turns-events-control",
    "/docs/sdks/rust/tools-code-mode-mcp",
    "/docs/sdks/rust/handles-forks-subagents",
    "/docs/sdks/rust/durability",
    "/docs/sdks/javascript",
    "/docs/sdks/javascript/install-entrypoints",
    "/docs/sdks/javascript/agent-lifecycle",
    "/docs/sdks/javascript/tools-code-mode-subagents",
    "/docs/sdks/javascript/transports-auth",
    "/docs/sdks/javascript/browser-workspace",
    "/docs/sdks/javascript/react",
    "/docs/deployments/cloudflare",
    "/docs/deployments/vercel",
  ]) {
    assert.ok(navigation.includes(`"${route}"`), `missing ${route}`);
  }

  const evalGroup = navigation.slice(evaluating, embedding);
  const evalRoutes = [
    "/docs/evals",
    "/docs/harness/focused-run",
    "/docs/harness/dashboard-worksets",
    "/docs/harness/evidence",
    "/docs/harness/read-results",
  ];
  let previous = -1;
  for (const route of evalRoutes) {
    const position = evalGroup.indexOf(`"${route}"`);
    assert.ok(position > previous, `${route} is out of order in Evals`);
    previous = position;
  }

  const embeddingGroup = navigation.slice(embedding, advanced);
  assert.ok(
    embeddingGroup.indexOf('id: "typescript-sdk"') < embeddingGroup.indexOf('id: "rust-sdk"'),
    "JavaScript / TypeScript must lead the embedding path",
  );
});

test("documentation source checker proves manifest parity, frontmatter, H1s, and internal links", () => {
  const script = new URL("../scripts/check-docs.mjs", import.meta.url);
  const output = execFileSync(process.execPath, [script.pathname, "--source-only"], {
    encoding: "utf8",
  });
  assert.match(output, /checked 37 documentation sources/);
});

test("navigation intent prepares the parsed overview, not only the Docs component", () => {
  assert.match(docs, /resolvedPageCache/);
  assert.match(docs, /export async function preloadDocsRoute/);
  assert.match(application, /preloadDocsRoute\("\/docs"\)/);
  assert.doesNotMatch(application, /\bloadDocs|import\(/);
  assert.match(application, /surfaceNavigationId\.current !== navigationId/);
});
