import type { Turn } from "nanocodex";
import { withHardDeadline } from "./deadline";

type DeletionTurn = Pick<Turn, "cancel" | "dispose">;

export async function drainRuntimeForDeletion(
  timeoutMs: number,
  turns: readonly DeletionTurn[],
  shutdown: () => Promise<void>,
  inFlight: readonly Promise<unknown>[],
): Promise<void> {
  await withHardDeadline("managed runtime deletion drain", timeoutMs, async () => {
    const cancellations = turns.map(async (turn) => {
      try { await turn.cancel(); } catch { /* A terminal turn needs no cancellation. */ }
      finally { turn.dispose(); }
    });
    await Promise.all([
      Promise.all(cancellations),
      shutdown(),
      Promise.allSettled(inFlight),
    ]);
  });
}
