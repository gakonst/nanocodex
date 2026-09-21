import type { MediaRequest, MediaResult } from 'nanocodex-tools';
import ffmpegSource from './media/generated/ffmpeg.js.txt';
import ffprobeSource from './media/generated/ffprobe.js.txt';
import ffmpegWasm from './media/generated/ffmpeg.wasm.bin';
import ffprobeWasm from './media/generated/ffprobe.wasm.bin';
import workerSource from './media/worker.js.txt';

/** Isolated, single-threaded FFmpeg; no container, host process or egress. */
export function createMediaExecutor(loader: WorkerLoader) {
  return async (request: MediaRequest, signal?: AbortSignal): Promise<MediaResult> => {
    signal?.throwIfAborted();
    const probe = request.command === 'ffprobe';
    const worker = loader.load({
      compatibilityDate: '2026-07-29',
      mainModule: 'worker.js',
      modules: {
        'worker.js': {js: workerSource},
        'core.js': {js: probe ? ffprobeSource : ffmpegSource},
        'core.wasm': {wasm: probe ? ffprobeWasm : ffmpegWasm},
      },
      globalOutbound: null,
      limits: {cpuMs: 30_000, subRequests: 0},
      env: {},
    });
    const body = new FormData();
    body.set('args', JSON.stringify(request.args));
    const input = request.files[0];
    if (input) {
      body.set('inputPath', input.path);
      body.set('input', new Blob([input.data as Uint8Array<ArrayBuffer>]), 'input');
    }
    const outputPath = probe ? undefined : request.args.find(arg => /^\/output\.[a-z0-9]+$/.test(arg));
    if (outputPath) body.set('outputPath', outputPath);
    const response = await worker.getEntrypoint().fetch(new Request('https://media.internal/run', {method: 'POST', body, signal}));
    signal?.throwIfAborted();
    if (!response.ok) throw new Error(`WASM media execution failed (${response.status}): ${(await response.text()).slice(0, 1024)}`);
    const result = await response.formData();
    const fields = JSON.parse(String(result.get('result'))) as Omit<MediaResult, 'files'>;
    const output = result.get('output');
    signal?.throwIfAborted();
    return {...fields, files: output instanceof File && outputPath
      ? [{path: outputPath, data: new Uint8Array(await output.arrayBuffer())}] : []};
  };
}
