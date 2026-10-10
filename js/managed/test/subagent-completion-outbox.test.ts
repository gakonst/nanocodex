import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import {
  backOffDueSubagentCompletions, consumedSubagentCompletion, deferSubagentCompletion, dueSubagentCompletions, markSubagentCompletionReceipt,
  nextSubagentCompletionAttempt, pendingSubagentCompletions, recordSubagentCompletion, settleReleasedSubagentCompletions,
  settleSubagentCompletion, subagentCompletion, subagentCompletionAlarmAt,
} from "../src/index";

const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;

// The durable completion outbox on the object's real SQLite storage.
it("backs undecided completions off without a hot loop and settles each row once", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, state) => {
    const storage = state.storage;
    const started = Date.now();
    recordSubagentCompletion(storage, "child-a", 3);
    recordSubagentCompletion(storage, "child-a", 3); // a re-announcement is idempotent
    expect(pendingSubagentCompletions(storage)).toEqual([{ session_id: "child-a", revision: 3, receipt_committed: 0 }]);
    expect(dueSubagentCompletions(storage, Date.now()).length).toBe(1);

    // Every failed decision advances attempts and next_at: 2 s doubling, capped at 1 h.
    const schedule: { attempts: number; delay: number }[] = [];
    for (let attempt = 0; attempt < 14; attempt++) {
      const before = Date.now();
      const next = deferSubagentCompletion(storage, "child-a", 3)!;
      schedule.push({ attempts: next.attempts, delay: next.next_at - before });
    }
    expect(schedule.map(entry => entry.attempts)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]);
    const delays = schedule.map(entry => Math.round(entry.delay / 1000));
    expect(delays.slice(0, 6)).toEqual([2, 4, 8, 16, 32, 64]);
    expect(Math.max(...delays)).toBe(3600);
    expect(delays.at(-1)).toBe(3600);
    // A backed-off row is no longer due, so the alarm is not re-armed at once.
    expect(dueSubagentCompletions(storage, Date.now())).toEqual([]);
    expect(nextSubagentCompletionAttempt(storage)! - started).toBeGreaterThanOrEqual(3_600_000 - 1_000);

    // A committed receipt marks the row; exactly one decision settles it.
    markSubagentCompletionReceipt(storage, "child-a", 3);
    expect(subagentCompletion(storage, "child-a", 3)?.receipt_committed).toBe(1);
    expect(settleSubagentCompletion(storage, "child-a", 3, "receipt_committed")).toBe(true);
    expect(settleSubagentCompletion(storage, "child-a", 3, "woken")).toBe(false);
    expect(subagentCompletion(storage, "child-a", 3)?.settled).toBe("receipt_committed");
    // The tombstone is not reopened by a re-announcement, a late receipt or a defer.
    recordSubagentCompletion(storage, "child-a", 3);
    markSubagentCompletionReceipt(storage, "child-a", 3);
    expect(deferSubagentCompletion(storage, "child-a", 3)).toBeUndefined();
    expect(pendingSubagentCompletions(storage)).toEqual([]);
    expect(nextSubagentCompletionAttempt(storage)).toBeUndefined();

    // An explicit close settles every undecided row of that child.
    recordSubagentCompletion(storage, "child-b", 1);
    recordSubagentCompletion(storage, "child-b", 2);
    expect(settleReleasedSubagentCompletions(storage, "child-b")).toBe(2);
    expect(subagentCompletion(storage, "child-b", 2)?.settled).toBe("released");
    expect(nextSubagentCompletionAttempt(storage)).toBeUndefined();

    // Tombstones are retained for 7 days, then collected on the next record.
    storage.sql.exec("UPDATE managed_subagent_completions SET settled_at = ? WHERE session_id = 'child-a'", Date.now() - 8 * 24 * 60 * 60 * 1000);
    recordSubagentCompletion(storage, "child-c", 1);
    expect(subagentCompletion(storage, "child-a", 3)).toBeUndefined();
    expect(subagentCompletion(storage, "child-b", 1)?.settled).toBe("released");
    // An old registry journal re-announcing a decided completion weeks later
    // (tombstone collected) is ignored, and so is a stale receipt; a newer
    // revision of the same child is a new completion.
    expect(recordSubagentCompletion(storage, "child-a", 3)).toBe(false);
    markSubagentCompletionReceipt(storage, "child-a", 3);
    expect(subagentCompletion(storage, "child-a", 3)).toBeUndefined();
    expect(recordSubagentCompletion(storage, "child-b", 2)).toBe(false);
    expect(recordSubagentCompletion(storage, "child-a", 5)).toBe(true);
    expect(subagentCompletion(storage, "child-a", 5)?.settled).toBeNull();
  });
});

// A drain that throws (a failed runtime rebuild or decision) must not re-arm
// the alarm at once: every due row backs off, and the alarm has a floor.
it("backs due rows off after a failed drain and floors the outbox alarm", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, state) => {
    const storage = state.storage;
    expect(subagentCompletionAlarmAt(storage, Date.now())).toBeUndefined();
    recordSubagentCompletion(storage, "child-x", 1);
    markSubagentCompletionReceipt(storage, "child-y", 4);
    const now = Date.now();
    // Due now, yet the alarm is never scheduled sooner than 2 s.
    expect(dueSubagentCompletions(storage, now).length).toBe(2);
    expect(subagentCompletionAlarmAt(storage, now)).toBe(now + 2_000);
    const first = backOffDueSubagentCompletions(storage, now);
    expect(first.map(row => [row.session_id, row.attempts]).sort()).toEqual([["child-x", 1], ["child-y", 1]]);
    expect(first.every(row => row.next_at >= now + 2_000 - 50)).toBe(true);
    expect(dueSubagentCompletions(storage, Date.now())).toEqual([]);
    expect(backOffDueSubagentCompletions(storage, Date.now())).toEqual([]);
    // The next failure (once due again) doubles the delay.
    const later = first[0].next_at;
    const second = backOffDueSubagentCompletions(storage, later);
    expect(second.map(row => row.attempts)).toEqual([2, 2]);
    expect(second.every(row => row.next_at - Date.now() >= 4_000 - 50)).toBe(true);
    expect(subagentCompletionAlarmAt(storage, Date.now())! - Date.now()).toBeGreaterThanOrEqual(2_000);
  });
});

// An older completion still pending (it arrived while the parent was busy)
// must not wake an idle parent that already consumed a newer revision of the
// same child: that notice would repeat the newer result. Newer revisions are
// never suppressed.
it("settles an older pending completion as superseded once a newer one is consumed", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, state) => {
    const storage = state.storage;
    expect(recordSubagentCompletion(storage, "child", 1)).toBe(true); // R1: deferred while busy
    expect(recordSubagentCompletion(storage, "child", 2)).toBe(true); // R2
    markSubagentCompletionReceipt(storage, "child", 2); // the parent's committed wait showed R2
    // The idle drain decides R1 first (created first): consumed via R2, no wake.
    expect(pendingSubagentCompletions(storage).map(row => row.revision)).toEqual([1, 2]);
    expect(consumedSubagentCompletion(storage, "child", 1)).toBe("superseded");
    expect(consumedSubagentCompletion(storage, "child", 2)).toBe("receipt_committed");
    expect(settleSubagentCompletion(storage, "child", 1, "superseded")).toBe(true);
    expect(settleSubagentCompletion(storage, "child", 2, "receipt_committed")).toBe(true);
    expect(pendingSubagentCompletions(storage)).toEqual([]);

    // Settling a newer revision first supersedes every older pending one atomically.
    expect(recordSubagentCompletion(storage, "other", 4)).toBe(true);
    expect(recordSubagentCompletion(storage, "other", 5)).toBe(true);
    expect(recordSubagentCompletion(storage, "other", 7)).toBe(true);
    markSubagentCompletionReceipt(storage, "other", 5);
    expect(settleSubagentCompletion(storage, "other", 5, "receipt_committed")).toBe(true);
    expect(subagentCompletion(storage, "other", 4)?.settled).toBe("superseded");
    // R7 is newer than the consumed R5: still undecided, never suppressed.
    expect(subagentCompletion(storage, "other", 7)?.settled).toBeNull();
    expect(consumedSubagentCompletion(storage, "other", 7)).toBeUndefined();
    expect(recordSubagentCompletion(storage, "other", 4)).toBe(false); // an old re-announcement

    // A new revision after all that is a new completion for the idle parent.
    expect(recordSubagentCompletion(storage, "child", 3)).toBe(true);
    expect(consumedSubagentCompletion(storage, "child", 3)).toBeUndefined();
    expect(pendingSubagentCompletions(storage).map(row => [row.session_id, row.revision])).toEqual([["other", 7], ["child", 3]]);
  });
});
