import type { ToolContext } from "nanocodex-tools";
import type { RoutedTool } from "./namespace-tools";

const CHUNK_BYTES = 192 * 1024;
const noStore = { "cache-control": "private, no-store", "x-content-type-options": "nosniff" };
export class FileDownloadError extends Error {
  constructor(readonly code: string, message: string, readonly status = 503) { super(message); }
}

/** Reject traversal before selecting a Hand; a path must never change its owner. */
export function downloadPath(url: URL): string {
  const path = url.searchParams.get("path");
  if ([...url.searchParams.keys()].some(key => key !== "path") || url.searchParams.getAll("path").length !== 1
    || !path?.startsWith("/") || path.length > 8192 || /[\u0000-\u001f\u007f\\]/.test(path)
    || path.split("/").slice(1).some(segment => !segment || segment === "." || segment === "..")) {
    throw new FileDownloadError("invalid_file_path", "Expected a canonical absolute file path", 400);
  }
  return path;
}

export function fileDownloadFailure(error: unknown): Response {
  return Response.json(error instanceof FileDownloadError
    ? { error: error.code, message: error.message }
    : { error: "file_unavailable", message: "The file could not be read from its Hand" },
  { status: error instanceof FileDownloadError ? error.status : 503, headers: noStore });
}

function headers(path: string): HeadersInit {
  const name = path.slice(path.lastIndexOf("/") + 1);
  return { ...noStore, "content-type": "application/octet-stream",
    "content-disposition": `attachment; filename*=UTF-8''${encodeURIComponent(name).replace(/['()*]/g, c => `%${c.charCodeAt(0).toString(16)}`)}` };
}

export async function downloadBrainFile(bucket: R2Bucket, sessionId: string, path: string): Promise<Response> {
  const object = await bucket.get(`brains/${sessionId}/${path.slice("/brain/".length)}`);
  if (!object) throw new FileDownloadError("file_not_found", "The file is no longer available in /brain", 404);
  return new Response(object.body, { headers: { ...headers(path), "content-length": String(object.size) } });
}

/** Fixed, read-only range commands use the captured Hand route, never model execution. */
export function fileReadCommand(path: string, chunk: number, windows: boolean): string {
  if (!Number.isSafeInteger(chunk) || chunk < 0) throw new Error("Invalid chunk");
  if (windows) {
    const literal = `'${path.replace(/'/g, "''")}'`;
    return `$ErrorActionPreference='Stop'; $f=[System.IO.File]::OpenRead(${literal}); try { [Console]::WriteLine($f.Length); $null=$f.Seek(${chunk * CHUNK_BYTES},[System.IO.SeekOrigin]::Begin); $b=New-Object byte[] ${CHUNK_BYTES}; $n=0; while($n -lt $b.Length) { $r=$f.Read($b,$n,$b.Length-$n); if($r -eq 0){break}; $n+=$r }; [Console]::Write([Convert]::ToBase64String($b,0,$n)) } finally { $f.Dispose() }`;
  }
  const literal = `'${path.replace(/'/g, "'\"'\"'")}'`;
  return `set -o pipefail\nif ! test -f ${literal}; then exit 44; fi\nwc -c < ${literal} || exit 45\ndd if=${literal} bs=${CHUNK_BYTES} skip=${chunk} count=1 2>/dev/null | base64`;
}

export async function downloadHandFile(
  path: string, workspace: string, root: string, exec: RoutedTool,
  context: ToolContext, active: () => boolean, finished: () => void = () => {},
): Promise<Response> {
  const windows = /^[A-Za-z]:[\\/]/.test(workspace);
  const relative = path.slice(root.length + 1);
  const physical = `${workspace.replace(/[\\/]$/, "")}/${relative}`;
  let size: number | undefined;
  const read = async (chunk: number): Promise<Uint8Array> => {
    context.signal.throwIfAborted();
    if (!active()) throw new FileDownloadError("agent_unavailable", "The conversation is no longer available", 409);
    const value = await exec.handler({ cmd: fileReadCommand(physical, chunk, windows), workdir: workspace,
      shell: windows ? "powershell.exe" : "/bin/bash", login: false, max_output_tokens: 262144, yield_time_ms: 30000 },
    { ...context, callId: `${context.callId}-${chunk}` });
    const result = value as Record<PropertyKey, unknown> | null;
    const execution = (result?.[Symbol.for("nanocodex.toolResult")] ? result.structuredResult : result) as Record<string, unknown> | null;
    if (execution?.exit_code === 44) throw new FileDownloadError("file_not_found", "The file is no longer available on its Hand", 404);
    if (execution?.exit_code !== 0 || execution.session_id !== undefined || typeof execution.output !== "string")
      throw new FileDownloadError("file_read_failed", "The Hand could not complete the file read");
    const newline = execution.output.indexOf("\n");
    const length = execution.output.slice(0, newline).trim();
    if (newline < 0 || !/^\d+$/.test(length) || !Number.isSafeInteger(Number(length)))
      throw new FileDownloadError("file_read_failed", "The Hand returned invalid file metadata");
    const currentSize = Number(length);
    if (size !== undefined && size !== currentSize)
      throw new FileDownloadError("file_changed", "The file changed during download; open the link again", 409);
    size = currentSize;
    const encoded = execution.output.slice(newline + 1).replace(/\s/g, "");
    if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)
      || encoded.length > Math.ceil(CHUNK_BYTES / 3) * 4)
      throw new FileDownloadError("file_read_failed", "The Hand returned an incomplete file chunk");
    const binary = atob(encoded);
    if (binary.length !== Math.min(CHUNK_BYTES, Math.max(0, size - chunk * CHUNK_BYTES)))
      throw new FileDownloadError("file_read_failed", "The Hand returned an incomplete file chunk");
    if (!active()) throw new FileDownloadError("agent_unavailable", "The conversation is no longer available", 409);
    return Uint8Array.from(binary, character => character.charCodeAt(0));
  };
  let chunk = 0;
  let next: Uint8Array | undefined = await read(chunk++);
  let cancelled = false;
  const stream = new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const bytes = next ?? await read(chunk++);
        next = undefined;
        if (cancelled) return;
        if (bytes.length) controller.enqueue(bytes);
        if (chunk * CHUNK_BYTES >= size!) { controller.close(); finished(); }
      } catch (error) { finished(); if (!cancelled) controller.error(error); }
    },
    cancel() { cancelled = true; finished(); },
  });
  return new Response(stream, { headers: { ...headers(path), "content-length": String(size) } });
}
