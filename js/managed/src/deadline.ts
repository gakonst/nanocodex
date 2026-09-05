type DeadlineOptions = Readonly<{
  retryable?: boolean;
}>;

export async function withHardDeadline<Result>(
  operation: string,
  timeoutMs: number,
  action: (signal: AbortSignal) => Promise<Result>,
  options: DeadlineOptions = {},
): Promise<Result> {
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const pending = Promise.resolve().then(() => action(controller.signal));
  // A timed-out operation may ignore cancellation and settle later. Observe it
  // without retaining it as the owner of the caller's retry lifecycle.
  void pending.catch(() => {});
  const deadline = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => {
      controller.abort();
      const error = new Error(`${operation} timed out after ${timeoutMs}ms`);
      reject(options.retryable ? Object.assign(error, { code: "retryable" }) : error);
    }, timeoutMs);
  });
  try {
    return await Promise.race([pending, deadline]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

export async function fetchResponseWithDeadline<Result>(
  binding: Pick<Fetcher, "fetch">,
  input: RequestInfo | URL,
  init: RequestInit,
  timeoutMs: number,
  operation: string,
  handle: (response: Response) => Promise<Result> | Result,
  options: DeadlineOptions = {},
): Promise<Result> {
  return withHardDeadline(operation, timeoutMs, async (signal) => {
    const response = await binding.fetch(input, { ...init, signal });
    if (signal.aborted) {
      void disposeResponse(response).catch(() => {});
      throw new Error(`${operation} completed after its deadline`);
    }
    try {
      return await handle(response);
    } finally {
      await disposeResponse(response);
    }
  }, options);
}

async function disposeResponse(response: Response): Promise<void> {
  if (response.body !== null && !response.bodyUsed) await response.body.cancel();
}
