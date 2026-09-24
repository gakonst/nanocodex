import type { RemoteInput } from "./handRemote.ts";

/** DOM buttons is a bitset, including chord transitions carried by pointermove.
 * https://www.w3.org/TR/pointerevents/#chorded-button-interactions */
export class RemoteMouseButtons {
  private buttons = 0;
  get held(): boolean { return this.buttons !== 0; }
  reset(): void { this.buttons = 0; }
  update(buttons: number, position: { x: number; y: number } | undefined, send: (event: RemoteInput) => void): void {
    const next = buttons & 7;
    for (const [mask, button] of [[1, 0], [2, 1], [4, 2]] as const) {
      if ((this.buttons & mask) !== (next & mask)) send({ kind: "button", button, down: Boolean(next & mask), ...position });
    }
    this.buttons = next;
  }
}

/** Send the leading motion immediately, then coalesce bursts in short windows.
 * Preserve relative distance and ordering before button/key/scroll transitions. */
export class RemoteMotionBuffer {
  private pending?: RemoteInput;
  private timer?: ReturnType<typeof setTimeout>;
  private readonly send: (event: RemoteInput) => void;
  private readonly backlogged: () => boolean;
  constructor(send: (event: RemoteInput) => void, backlogged: () => boolean = () => false) {
    this.send = send; this.backlogged = backlogged;
  }
  input(event: RemoteInput): void {
    if (event.kind === "releaseAll") { this.clear(); this.send(event); return; }
    if (event.kind !== "move" && event.kind !== "relativeMove") { this.flush(); this.send(event); return; }
    if (this.pending && this.pending.kind !== event.kind) this.flush();
    if (event.kind === "relativeMove" && this.pending) {
      this.pending = { kind: "relativeMove", deltaX: (this.pending.deltaX ?? 0) + (event.deltaX ?? 0), deltaY: (this.pending.deltaY ?? 0) + (event.deltaY ?? 0) };
    } else this.pending = { ...event };
    if (this.timer === undefined) {
      // Arm before sending: a synchronous disconnect can clear this window.
      this.startWindow(event.kind); this.sendPending();
    }
  }
  private startWindow(kind: RemoteInput["kind"]): void {
    this.timer = setTimeout(() => {
      this.timer = undefined;
      if (!this.pending) return;
      // A trailing batch begins another window, preventing a fresh leading
      // sample immediately after it from doubling the sustained packet rate.
      this.startWindow(this.pending.kind); this.sendPending();
    }, kind === "relativeMove" && this.backlogged() ? 16 : 4);
  }
  flush(): void {
    clearTimeout(this.timer); this.timer = undefined; this.sendPending();
  }
  private sendPending(): void {
    const event = this.pending; this.pending = undefined;
    if (!event) return;
    if (event.kind !== "relativeMove") { this.send(event); return; }
    let x = event.deltaX ?? 0, y = event.deltaY ?? 0;
    if (!Number.isFinite(x) || !Number.isFinite(y)) return;
    do {
      const deltaX = Math.max(-4096, Math.min(4096, x)), deltaY = Math.max(-4096, Math.min(4096, y));
      this.send({ kind: "relativeMove", deltaX, deltaY }); x -= deltaX; y -= deltaY;
    } while (x !== 0 || y !== 0);
  }
  clear(): void { clearTimeout(this.timer); this.timer = undefined; this.pending = undefined; }
}
