import { WorkerEntrypoint } from "cloudflare:workers";
import type { MediaRequest } from "nanocodex-tools";
import { createMediaExecutor } from "./media-runtime";

type Env = { LOADER: WorkerLoader };

/** Service-binding-only endpoint: no default fetch handler, route, or workers.dev URL. */
export class MediaService extends WorkerEntrypoint<Env> {
  async fetch(request: Request): Promise<Response> {
    return runMediaRequest(request, this.env.LOADER);
  }
}

/** Single trusted service call; the test harness supplies the loader without a public production route. */
export async function runMediaRequest(request: Request, loader: WorkerLoader): Promise<Response> {
    if (request.method !== "POST" || new URL(request.url).pathname !== "/run") {
      return new Response("Not found", { status: 404 });
    }
    let form: FormData;
    try { form = await request.formData(); }
    catch { return new Response("Invalid media request", { status: 400 }); }
    const command = form.get("command");
    const rawArgs = form.get("args");
    const input = form.get("input");
    const inputPath = form.get("inputPath");
    const outputPath = form.get("outputPath");
    if ((command !== "ffmpeg" && command !== "ffprobe") || typeof rawArgs !== "string") {
      return new Response("Invalid media request", { status: 400 });
    }
    let args: unknown;
    try { args = JSON.parse(rawArgs); } catch { return new Response("Invalid media arguments", { status: 400 }); }
    if (!Array.isArray(args) || args.some(arg => typeof arg !== "string" || !arg || /[\x00-\x1f\x7f]/.test(arg))
      || (input !== null && (!(input instanceof File) || typeof inputPath !== "string" || !/^\/input\.[a-z0-9]+$/.test(inputPath)))
      || (input === null && inputPath !== null)
      || (outputPath !== null && (command !== "ffmpeg" || typeof outputPath !== "string" || !/^\/output\.(?:jpg|jpeg|wav)$/.test(outputPath) || !args.includes(outputPath)))) {
      return new Response("Invalid media request", { status: 400 });
    }
    const files = input instanceof File
      ? [{ path: inputPath as string, data: new Uint8Array(await input.arrayBuffer()) }] : [];
    const media: MediaRequest = { command, args, files };
    const result = await createMediaExecutor(loader)(media, request.signal);
    const response = new FormData();
    response.set("result", JSON.stringify({ stdout: result.stdout, stderr: result.stderr, exitCode: result.exitCode }));
    if (result.files.length) {
      const output = result.files[0]!;
      response.set("output", new Blob([output.data as Uint8Array<ArrayBuffer>]), output.path.slice(1));
    }
    return new Response(response);
}

/** Explicitly deny public/default fetch; callers must bind the MediaService entrypoint. */
export default { fetch(): Response { return new Response("Not found", { status: 404 }); } };
