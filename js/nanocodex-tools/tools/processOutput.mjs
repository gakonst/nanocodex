import { mkdtemp, open, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { StringDecoder } from "node:string_decoder";

// Serialize disk operations; callers pause each source until append completes.
// Reply budgets limit each read, never how much unread output is retained.
export async function createProcessOutput() {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-output-"));
  let file;
  try { file = await open(join(directory, "output"), "wx+", 0o600); }
  catch (error) { await rm(directory, { recursive: true, force: true }); throw error; }
  let queued = Promise.resolve();
  let accepted = 0;
  let written = 0;
  let consumed = 0;
  let closing;
  const decoder = new StringDecoder("utf8");
  const enqueue = operation => {
    const result = queued.then(operation);
    queued = result.catch(() => {});
    return result;
  };
  return {
    get unread() { return accepted - consumed; },
    append(data) {
      if (closing) return Promise.reject(new Error("Process output is closed."));
      const buffer = Buffer.isBuffer(data) ? data : Buffer.from(data);
      accepted += buffer.length;
      return enqueue(async () => {
        let offset = 0;
        while (offset < buffer.length) {
          const { bytesWritten } = await file.write(buffer, offset, buffer.length - offset, written);
          if (!bytesWritten) throw new Error("Could not write process output.");
          offset += bytesWritten;
          written += bytesWritten;
        }
      });
    },
    read(maxBytes) {
      return enqueue(async () => {
        const buffer = Buffer.alloc(Math.min(maxBytes, written - consumed));
        const { bytesRead } = await file.read(buffer, 0, buffer.length, consumed);
        consumed += bytesRead;
        return decoder.write(buffer.subarray(0, bytesRead));
      });
    },
    end() { return decoder.end(); },
    close() {
      return closing ??= enqueue(async () => {
        try { await file.close(); }
        finally { await rm(directory, { recursive: true, force: true }); }
      });
    },
  };
}
