import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";

const manifestUrl = new URL(
  "../../../crates/experimental/nanocodex-computer/runtime/src/browser_api_manifest.json",
  import.meta.url,
);
const outputUrl = new URL("../browser-api.d.mts", import.meta.url);
const manifest = JSON.parse(await readFile(manifestUrl, "utf8"));

const sections = [
  "// Generated from the runtime browser API manifest. Run `pnpm generate:api` after changing it.",
  "",
];
for (const { text } of Object.values(manifest.types)) {
  sections.push(
    text
      .replace(/^(type|interface) /, "export $1 ")
      .replace(/= string \| (\([^\n]+=>[^\n]+);$/, "= string | ($1);"),
    "",
  );
}
for (const [name, members] of Object.entries(manifest.interfaces)) {
  sections.push(`export interface ${name} {`);
  for (const member of Object.values(members)) {
    for (const declaration of member.declarations) {
      sections.push(...declaration.text.split("\n").map(line => `  ${line}`));
    }
  }
  sections.push("}", "");
}
const generated = sections.join("\n");

if (process.argv.includes("--check")) {
  assert.equal(await readFile(outputUrl, "utf8"), generated, "browser-api.d.mts is stale");
} else {
  await writeFile(outputUrl, generated);
}
