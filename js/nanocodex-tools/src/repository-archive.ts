import { createGzipDecoder, createTarDecoder } from "modern-tar";
import type { Workspace } from "../tools/types.mjs";
import type { ShellFetch } from "./shell.js";

/** Extract a GitHub source snapshot directly into the host-owned workspace. */
export async function downloadRepositoryArchive(
  fetch: ShellFetch,
  workspace: Workspace,
  repository: string,
  ref: string,
  directory: string,
  signal?: AbortSignal,
): Promise<void> {
  signal?.throwIfAborted();
  const transfer = new AbortController();
  const transferSignal = signal ? AbortSignal.any([signal, transfer.signal]) : transfer.signal;
  const response = await (fetch.stream ?? fetch)(
    `https://api.github.com/repos/${repository}/tarball/${encodeURIComponent(ref)}`,
    { headers: { accept: "application/vnd.github+json" }, signal: transferSignal },
  );
  const iterator = (async function* () {
    if (response.body instanceof Uint8Array) yield response.body;
    else yield* response.body;
  })()[Symbol.asyncIterator]();
  if (response.status !== 200) {
    transfer.abort();
    await iterator.return(undefined);
    throw new Error(`repository archive download failed (HTTP ${response.status})`);
  }
  const source = new ReadableStream<Uint8Array>({
    async pull(controller) {
      signal?.throwIfAborted();
      const next = await iterator.next();
      if (next.done) controller.close();
      else controller.enqueue(next.value);
    },
    async cancel() { await iterator.return(undefined); },
  });
  const entries = source.pipeThrough(createGzipDecoder()).pipeThrough(createTarDecoder({ strict: true })).getReader();
  const writes = new Set<Promise<void>>();
  let writeFailure: unknown;
  let archiveRoot: string | undefined;
  let files = 0;
  const paths = new Set<string>();
  try {
    await workspace.mkdir(directory);
    for (;;) {
      signal?.throwIfAborted();
      if (writeFailure !== undefined) throw writeFailure;
      const { done, value: entry } = await entries.read();
      if (done) break;
      const name = entry.header.name.replace(/\/$/, "");
      const parts = name.split("/");
      if (parts.some((part) => !part || part === "." || part === ".." || part === ".git")
        || name.includes("\\") || name.includes("\0")) {
        await entry.body.cancel();
        throw new Error("repository archive contains an unsafe path");
      }
      archiveRoot ??= parts[0];
      if (parts[0] !== archiveRoot || (parts.length === 1 && entry.header.type !== "directory")) {
        await entry.body.cancel();
        throw new Error("repository archive must contain one root directory");
      }
      if (entry.header.type !== "directory" && entry.header.type !== "file") {
        await entry.body.cancel();
        throw new Error(`repository archive entry type '${entry.header.type}' is unsupported`);
      }
      if (parts.length === 1) { await entry.body.cancel(); continue; }
      const relative = parts.slice(1).join("/");
      if (paths.has(relative)) {
        await entry.body.cancel();
        throw new Error("repository archive contains duplicate paths");
      }
      paths.add(relative);
      const target = `${directory}/${relative}`;
      if (entry.header.type === "directory") {
        await entry.body.cancel();
        await workspace.mkdir(target);
        continue;
      }
      const contents = new Uint8Array(await new Response(entry.body).arrayBuffer());
      signal?.throwIfAborted();
      files += 1;
      // Overlap durable writes without retaining the complete archive in memory.
      const write = workspace.writeFile(target, contents)
        .catch((error: unknown) => { writeFailure ??= error; })
        .finally(() => { writes.delete(write); });
      writes.add(write);
      if (writes.size >= 8) await Promise.race(writes);
    }
    await Promise.all(writes);
    if (writeFailure !== undefined) throw writeFailure;
    if (!archiveRoot || files === 0) throw new Error("repository archive contains no files");
    signal?.throwIfAborted();
  } finally {
    // Stop an upstream read before cancelling the decoder, including when
    // validation or a durable write failed while the network was still active.
    transfer.abort();
    await entries.cancel().catch(() => {});
    entries.releaseLock();
    // The caller may clean up the destination only after every write has settled.
    await Promise.all(writes);
  }
}
