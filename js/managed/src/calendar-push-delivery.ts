import { DurableObject } from "cloudflare:workers";
import type { DurableAgentSession } from "./index";
type Environment = { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> };
/** One source per object. Only private worker/session capabilities enqueue it. */
export class CalendarPushDelivery extends DurableObject<Environment> {
  async enqueue(sourceId: string, agentId: string): Promise<void> {
    await this.ctx.storage.put("delivery", { sourceId, agentId, generation: crypto.randomUUID() });
    await this.ctx.storage.setAlarm(Date.now());
  }
  async alarm(): Promise<void> {
    const target = await this.ctx.storage.get<{ sourceId: string; agentId: string; generation: string }>("delivery");
    if (!target) return;
    // Persist recovery before RPC; a process interruption cannot lose the work.
    await this.ctx.storage.setAlarm(Date.now() + 60000);
    try {
      const result = await this.env.NANOCODEX_SESSIONS.getByName(target.agentId).calendarPushReconcile(target.sourceId);
      const current = await this.ctx.storage.get<{generation:string}>("delivery");
      if (current?.generation !== target.generation) { await this.ctx.storage.setAlarm(Date.now()); return; }
      if (!result.enabled) { await this.ctx.storage.deleteAlarm(); await this.ctx.storage.deleteAll(); return; }
      const requested = result.nextAt ?? Date.now() + (result.complete ? 3600000 : 1000);
      const existing = await this.ctx.storage.getAlarm();
      // A callback interleaving with the RPC can have scheduled an earlier alarm.
      await this.ctx.storage.setAlarm(Math.min(requested, existing !== null && existing < Date.now() + 1000 ? existing : requested));
    } catch { /* Durable one-minute retry already stored; never log invite data. */ }
  }
}
