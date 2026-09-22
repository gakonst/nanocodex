// A disposable desktop-user process around the unmodified OpenAI Sky service.
// The model and its trusted worker remain in their existing Codex sandbox.
import { pathToFileURL } from 'node:url';
const { handleRpc } = await import(pathToFileURL(process.argv[2]).href);
const drags = new Set();
let queue = Promise.resolve();
process.on('message', message => {
  queue = queue.then(async () => {
    try {
      if (message.cleanup) {
        for (const handle_id of drags) {
          try { await handleRpc({ type: 'drag_end', handle_id }); }
          catch { /* The native helper may already have exited. */ }
        }
        drags.clear();
        process.send?.({ id: message.id });
        return;
      }
      const request = message.request;
      // Track before starting: a partial native failure still needs release.
      if (request.type === 'drag_start') drags.add(request.handle_id);
      const value = await handleRpc(request);
      if (request.type === 'drag_end') drags.delete(request.handle_id);
      process.send?.({ id: message.id, value });
    } catch (error) {
      process.send?.({ id: message.id, error: String(error.message ?? error) });
    }
  }).catch(() => process.exit(1));
});
process.on('disconnect', () => process.exit(0));
// Sky's existing process-exit hook terminates its native helper. The supervisor
// additionally terminates the complete process group after bounded cleanup.
process.on('SIGTERM', () => process.exit(0));
