import assert from "node:assert/strict";
import test from "node:test";
import { readScheduledAgents, scheduledAgentCandidates } from "./scheduledAgents.ts";

test("discovery surfaces denied access and malformed lists instead of claiming no schedules", async () => {
  await assert.rejects(scheduledAgentCandidates(async () => Response.json({ error: "forbidden" }, { status: 403 })), /forbidden/);
  await assert.rejects(scheduledAgentCandidates(async () => Response.json({ data: [42] })), /Invalid agent list/);
});


test("account discovery bounds parallel reads and preserves candidate order", async () => {
  let active = 0;
  let peak = 0;
  const candidates = Array.from({ length: 12 }, (_, index) => ({ id: String(index), title: String(index) }));
  const rows = await readScheduledAgents(candidates, async candidate => {
    active++;
    peak = Math.max(peak, active);
    await new Promise(resolve => setTimeout(resolve, 1));
    active--;
    return candidate.id;
  }, new AbortController().signal);
  assert.equal(peak, 4);
  assert.deepEqual(rows, candidates.map(candidate => candidate.id));
});

test("partial read failure and cancellation never report an empty successful account", async () => {
  const candidates = [{ id: "a", title: "A" }, { id: "b", title: "B" }];
  await assert.rejects(readScheduledAgents(candidates, async candidate => {
    if (candidate.id === "b") throw new Error("Read failed");
    return [];
  }, new AbortController().signal), /Read failed/);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(readScheduledAgents(candidates, async () => [], controller.signal), { name: "AbortError" });
});
