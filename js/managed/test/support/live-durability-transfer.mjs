import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";

/** Public cutover journey, also reusable against a retained live-test thread. */
export async function transferLongThread({ id, origin, key, cli, cwd }) {
  // The public cutover API must carry the records as well as the head.
  let archive;
  for (let batch = 0; batch < 500; batch++) {
    const response = await fetch(`${origin}/v1/agents/${id}/durability`, {
      method: "POST", headers: { authorization: `Bearer ${key}` }, signal: AbortSignal.timeout(30_000),
    });
    assert.ok(response.ok, `durability export: HTTP ${response.status}: ${(await response.clone().text()).slice(0, 300)}`);
    if (response.status !== 202) { archive = await response.json(); break; }
  }
  assert.equal(archive?.format, "nanocodex-managed-durability-state-v2");
  assert.deepEqual(archive.durability.records, []);
  assert.ok(archive.managed_durability_records.objects > 0);
  await writeFile(`${cwd}/export-summary.json`, JSON.stringify({
    headBytes: Buffer.byteLength(archive.durability.payload), records: archive.managed_durability_records,
  }));
  let imported;
  for (let batch = 0; batch < 100; batch++) {
    const response = await fetch(`${origin}/v1/agents`, {
      method: "POST", headers: { authorization: `Bearer ${key}`, "content-type": "application/json", "idempotency-key": `import-${id}` },
      body: JSON.stringify({ durability: archive }), signal: AbortSignal.timeout(60_000),
    });
    const body = await response.json();
    if (response.status === 503 && body.error === "durability_import_pending") continue;
    assert.ok(response.ok, `durability import: HTTP ${response.status}: ${JSON.stringify(body)}`);
    imported = body; break;
  }
  const importedId = imported?.id ?? imported?.agent_id;
  assert.equal(typeof importedId, "string");
  const importedRun = await cli("run", "--agent", importedId, "Reply exactly LONG_THREAD_IMPORTED. Use no tools.");
  assert.match(importedRun.stderr, /LONG_THREAD_IMPORTED/);
  console.info(`bounded record export/import and real binary continuation passed: ${importedId}`);
  return importedId;
}
