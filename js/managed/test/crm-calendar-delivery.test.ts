import { env, runInDurableObject, runDurableObjectAlarm } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { CalendarPushDelivery } from "../src/calendar-push-delivery";
// A callback must persist an immediate alarm; crashes retain retries, partial
// pages continue promptly, disable removes alarms, no model turn is needed.
it("durably delivers hints immediately, retries failures, and stops disabled sources", async () => {
  const namespace = (env as unknown as { NANOCODEX_CALENDAR_PUSH: DurableObjectNamespace<CalendarPushDelivery> }).NANOCODEX_CALENDAR_PUSH;
  const stub = namespace.getByName(crypto.randomUUID());
  let calls = 0, fail = true, enabled = true;
  await runInDurableObject(stub, async (instance) => {
    Object.defineProperty(instance, "env", { value: { NANOCODEX_SESSIONS: { getByName: () => ({ calendarPushReconcile: async () => {
      calls++; if (fail) throw new Error("synthetic transport"); return { enabled, complete: false };
    } }) } } });
  });
  const clock = vi.spyOn(Date,"now").mockReturnValue(Date.now()+60000);
  await stub.enqueue("source", "agent");
  await runInDurableObject(stub, async (_, state) => expect(await state.storage.getAlarm()).toBeLessThanOrEqual(Date.now() + 1000));
  await runDurableObjectAlarm(stub);
  expect(calls).toBe(1);
  await runInDurableObject(stub, async (_, state) => expect(await state.storage.getAlarm()).toBeGreaterThan(Date.now()));
  fail = false;
  await runDurableObjectAlarm(stub);
  expect(calls).toBe(2);
  enabled = false;
  await runDurableObjectAlarm(stub);
  await runInDurableObject(stub, async (_, state) => expect(await state.storage.getAlarm()).toBeNull());
  clock.mockRestore();
});
it("preserves a newly enqueued generation when an older RPC reports disabled", async () => {
  const namespace = (env as unknown as { NANOCODEX_CALENDAR_PUSH: DurableObjectNamespace<CalendarPushDelivery> }).NANOCODEX_CALENDAR_PUSH;
  const stub = namespace.getByName(crypto.randomUUID());
  const clock=vi.spyOn(Date,"now").mockReturnValue(Date.now()+60000);
  await runInDurableObject(stub,async instance=>{Object.defineProperty(instance,"env",{value:{NANOCODEX_SESSIONS:{getByName:()=>({calendarPushReconcile:async()=>{await instance.enqueue("source","agent");return {enabled:false,complete:true};}})}}});});
  await stub.enqueue("source","agent");
  await runDurableObjectAlarm(stub);
  await runInDurableObject(stub,async(_,state)=>{expect(await state.storage.get("delivery")).toBeTruthy();expect(await state.storage.getAlarm()).not.toBeNull();});
  clock.mockRestore();
});
