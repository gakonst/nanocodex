import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

test("direct native auth profile binds existing managed namespaces and is disabled in local development", async () => {
  const source = await readFile(new URL("../wrangler.jsonc", import.meta.url), "utf8");
  const parsed = ts.parseConfigFileTextToJson("wrangler.jsonc", source);
  assert.equal(parsed.error, undefined);
  const bindings = parsed.config.durable_objects.bindings;
  for (const [name, class_name] of [["NANOCODEX_LIVE_API_KEYS", "ApiKeyRecord"], ["NANOCODEX_LIVE_SESSIONS", "DurableAgentSession"]]) {
    assert.deepEqual(bindings.filter((b: { name: string }) => b.name === name), [{ name, class_name, script_name: "nanocodex-durable-agent" }]);
    assert.equal(parsed.config.env.development.durable_objects.bindings.some((b: { name: string }) => b.name === name), false);
    assert.equal((parsed.config.migrations ?? []).some((m: { new_sqlite_classes?: string[]; new_classes?: string[] }) => [...m.new_sqlite_classes ?? [], ...m.new_classes ?? []].includes(class_name)), false);
  }
});
