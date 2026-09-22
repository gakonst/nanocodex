// Keep text entry on the official Sky API. Its element-targeted path preserves
// the caret/selection while focusing the editable control before native paste.
export async function executeSkyRequest(handleRpc, request) {
  const input = request.type === 'execute' && request.method === 'type_text'
    && request.args?.length === 1 ? request.args[0] : undefined;
  if (!input || typeof input.text !== 'string' || !input.window
      || input.element_id != null) return handleRpc(request);

  // Resolve at dispatch time, not from a cached click: Tab, dialogs, and mouse
  // actions can move the caret to another control in the same window.
  const state = await handleRpc({ type: 'execute', method: 'get_window_state',
    args: [{ window: input.window, include_screenshot: false }] });
  const focused = [];
  const visit = node => {
    if (!node) return;
    const states = node.states ?? [];
    if (states.includes('focused') && states.includes('editable')
        && states.includes('enabled') && !states.includes('defunct')) focused.push(node);
    for (const child of node.children ?? []) visit(child);
  };
  if (state?.ax_tree_source === 'at_spi' && state.window?.id === input.window.id) visit(state.ax_tree);
  if (focused.length !== 1) return handleRpc(request);
  const id = focused[0].id;
  if (typeof id !== 'string' || !/^\d+$/.test(id)) return handleRpc(request);
  return handleRpc({ ...request, args: [{ ...input, element_id: id }] });
}
