import type { BrowserTakeoverAction } from './vaultIntake.ts';
/** Only adjacent moves may be replaced: all input and gesture boundaries retain order. */
export function enqueueTakeover(queue: BrowserTakeoverAction[], action: BrowserTakeoverAction) {
  const last = queue.at(-1);
  if (action.action === 'touch' && action.phase === 'move' && last?.action === 'touch' && last.phase === 'move') queue[queue.length - 1] = action;
  else queue.push(action);
}
const segments = (value: string) => Array.from(new Intl.Segmenter(undefined, { granularity: 'grapheme' }).segment(value), part => part.segment);
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
