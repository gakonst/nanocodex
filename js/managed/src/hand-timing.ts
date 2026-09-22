/** Request-local timings; never retain credentials or capability-bearing paths. */
const timings = new WeakMap<Request, { id: string; path: string; start: number; epoch: number; stages: Record<string, number> }>();

export function beginHandTiming(request: Request): void {
  const path = new URL(request.url).pathname;
  if (!/^\/v1\/(?:account\/(?:hands(?:\/|$)|tool-host$|vm-host$)|vm-host-attachments\/)/.test(path)) return;
  const vm = path.startsWith("/v1/vm-host-attachments/");
  const endpoint = vm ? path.match(/^\/v1\/vm-host-attachments\/[^/]+\/[^/]+\/(tool-host|hands\/(?:host|ice|renew))$/)?.[1] : undefined;
  timings.set(request, { id: crypto.randomUUID(),
    path: vm ? `/v1/vm-host-attachments/:pool/:allocation/${endpoint ?? ":invalid"}` : path,
    start: performance.now(), epoch: Date.now(), stages: {} });
}

export function recordHandTiming(request: Request, name: string, duration: number): void {
  const timing = timings.get(request);
  if (timing) timing.stages[name] = duration;
}

export async function timeHandStage<T>(request: Request, name: string, work: () => Promise<T>): Promise<T> {
  const start = performance.now();
  try { return await work(); } finally { recordHandTiming(request, name, performance.now() - start); }
}

export function finishHandTiming(request: Request, response: Response): Response {
  const timing = timings.get(request);
  if (!timing) return response;
  timings.delete(request);
  timing.stages.total = performance.now() - timing.start;
  console.info({ type: "hand.request", request_id: timing.id, method: request.method,
    path: timing.path, status: response.status, timings_ms: timing.stages,
    started_at_ms: timing.epoch, finished_at_ms: Date.now(),
    request_colo: typeof request.cf?.colo === "string" ? request.cf.colo : undefined });
  // Keep the upgraded socket while exposing the same correlation ID to native clients.
  const headers = new Headers(response.headers);
  headers.set("x-nanocodex-request-id", timing.id);
  headers.append("server-timing", Object.entries(timing.stages)
    .map(([name, duration]) => `hand_${name};dur=${duration.toFixed(1)}`).join(", "));
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers,
    ...(response.status === 101 ? { webSocket: response.webSocket } : {}),
  });
}
