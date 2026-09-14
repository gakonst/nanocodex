/** Account discovery is separate from local process ownership. Cached devices
 * are always restored offline; only a fresh service snapshot can connect them. */
function hand(value) {
  if (!value || typeof value.id !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,122}$/.test(value.id)
    || typeof value.name !== "string" || !value.name || Buffer.byteLength(value.name) > 128
    || typeof value.workspace !== "string" || !value.workspace.startsWith("/") || value.workspace.includes("\0")
    || !Array.isArray(value.capabilities) || value.capabilities.some(item => typeof item !== "string")) {
    throw new Error("The account returned an invalid Hand list.");
  }
  return { id: value.id, name: value.name, workspace: value.workspace, capabilities: [...value.capabilities] };
}

export function restoredAccountHands(value) {
  if (!Array.isArray(value)) return [];
  return value.flatMap(value => {
    try { return [{ ...hand(value), status: "offline" }]; } catch { return []; }
  });
}

export function mergeAccountHands(previous, current) {
  if (!Array.isArray(current)) throw new Error("The account returned an invalid Hand list.");
  const known = new Map(restoredAccountHands(previous).map(value => [value.id, value]));
  const ids = new Set();
  for (const value of current) {
    const next = hand(value);
    if (ids.has(next.id)) throw new Error("The account returned duplicate Hands.");
    ids.add(next.id); known.set(next.id, { ...next, status: "connected" });
  }
  return [...known.values()].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}
