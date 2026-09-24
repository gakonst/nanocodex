import type { MediaRequest, MediaResult } from "nanocodex-tools";

/** The media assets and WorkerLoader live exclusively in the private media service. */
export function createMediaExecutor(service: Fetcher) {
  return async (request: MediaRequest, signal?: AbortSignal): Promise<MediaResult> => {
    signal?.throwIfAborted();
    const body = new FormData();
    body.set("command", request.command);
    body.set("args", JSON.stringify(request.args));
    const input = request.files[0];
    if (input) {
      body.set("inputPath", input.path);
      body.set("input", new Blob([input.data as Uint8Array<ArrayBuffer>]), "input");
    }
    const outputPath = request.command === "ffprobe" ? undefined
      : request.args.find(arg => /^\/output\.[a-z0-9]+$/.test(arg));
    if (outputPath) body.set("outputPath", outputPath);
    const response = await service.fetch(new Request("https://media.internal/run", { method: "POST", body, signal }));
    signal?.throwIfAborted();
    if (!response.ok) throw new Error(`WASM media execution failed (${response.status}): ${await response.text()}`);
    const result = await response.formData();
    const fields = JSON.parse(String(result.get("result"))) as Omit<MediaResult, "files">;
    const output = result.get("output");
    signal?.throwIfAborted();
    return { ...fields, files: output instanceof File && outputPath
      ? [{ path: outputPath, data: new Uint8Array(await output.arrayBuffer()) }] : [] };
  };
}
