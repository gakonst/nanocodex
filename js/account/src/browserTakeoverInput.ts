import type { BrowserTakeoverAction } from './vaultIntake.ts';
const segments = (value: string) => Array.from(new Intl.Segmenter(undefined, { granularity: 'grapheme' }).segment(value), part => part.segment);
/** Queue a takeover action while a request is in flight.
 * Ordering of every gesture and text boundary is retained. Only adjacent moves,
 * adjacent text edits and redundant idle refreshes are merged, so typing during
 * a slow round trip becomes one batched edit instead of one request per key. */
export function enqueueTakeover(queue: BrowserTakeoverAction[], action: BrowserTakeoverAction) {
  const last = queue.at(-1);
  // An idle refresh (no viewport change) is redundant: every response carries a frame.
  if (action.action === 'observe' && !action.viewport && queue.length) return;
  if (action.action !== 'observe' && last?.action === 'observe' && !last.viewport) queue.pop();
  const tail = queue.at(-1);
  if (action.action === 'touch' && action.phase === 'move' && tail?.action === 'touch' && tail.phase === 'move') { queue[queue.length - 1] = action; return; }
  if (action.action === 'edit' && tail?.action === 'edit') {
    const merged = mergeEdits(tail, action);
    if (merged) { queue[queue.length - 1] = merged; return; }
  }
  queue.push(action);
}
function mergeEdits(a: BrowserTakeoverAction, b: BrowserTakeoverAction): BrowserTakeoverAction | undefined {
  const text = segments(a.text ?? ''); let deletes = a.delete_backward ?? 0, pending = b.delete_backward ?? 0;
  const cancelled = Math.min(pending, text.length); text.length -= cancelled; pending -= cancelled;
  if (pending && text.length) return undefined;
  deletes += pending;
  const next = text.join('') + (b.text ?? '');
  if (deletes > 128 || next.length > 512) return undefined;
  return { action: 'edit', delete_backward: deletes, text: next };
}
export function textEdits(before: string, after: string): BrowserTakeoverAction[] {
  const a = segments(before), b = segments(after); let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix++;
  const actions: BrowserTakeoverAction[] = []; let remaining = a.length - prefix;
  while (remaining) { const count = Math.min(remaining, 128); actions.push({ action: 'edit', delete_backward: count, text: '' }); remaining -= count; }
  let chunk = '';
  for (const point of b.slice(prefix).join('')) { if (chunk.length + point.length > 512) { actions.push({ action: 'edit', delete_backward: 0, text: chunk }); chunk = ''; } chunk += point; }
  if (chunk) actions.push({ action: 'edit', delete_backward: 0, text: chunk });
  return actions;
}
export function imagePoint(rect: Pick<DOMRect, 'left' | 'top' | 'width' | 'height'>, x: number, y: number) {
  return { x: Math.max(0, Math.min(1, (x - rect.left) / rect.width)), y: Math.max(0, Math.min(1, (y - rect.top) / rect.height)) };
}
