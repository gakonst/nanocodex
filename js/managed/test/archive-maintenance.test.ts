import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { ArchiveMaintenance } from "../src/archive-maintenance";

it("retains archive backoff across reconstruction and clears it only after successful I/O", async () => {
  const sessions = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_instance, state) => {
    let now = 1_000, attempts = 0;
    const maintenance = new ArchiveMaintenance(state.storage, () => now);
    await expect(maintenance.start(async () => {
      attempts++;
      throw new Error("bucket unavailable");
    })).rejects.toThrow("bucket unavailable");
    const reopened = new ArchiveMaintenance(state.storage, () => now);
    expect(reopened.nextAttemptAt()).toBe(61_000);
    for (let i = 0; i < 100; i++) {
      expect(reopened.start(async () => { attempts++; })).toBeUndefined();
    }
    expect(attempts).toBe(1);
    now = 61_000;
    await reopened.start(async () => { attempts++; });
    expect(attempts).toBe(2);
    expect(reopened.nextAttemptAt()).toBe(0);
  });
});

it("owns one upload while retaining a durable recovery deadline before that upload settles", async () => {
  const sessions = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_instance, state) => {
    let release!: () => void;
    const upload = new Promise<void>((resolve) => { release = resolve; });
    const maintenance = new ArchiveMaintenance(state.storage, () => 5_000);
    const task = maintenance.start(() => upload);
    expect(maintenance.nextAttemptAt()).toBe(65_000);
    expect(maintenance.start(async () => { throw new Error("duplicate upload"); })).toBeUndefined();
    release();
    await task;
    expect(maintenance.nextAttemptAt()).toBe(0);
  });
});
