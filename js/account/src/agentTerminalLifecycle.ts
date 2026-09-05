export class GenerationRequestOwner<T> {
  private current: { generation: number; request: Promise<T> } | undefined;

  run(generation: number, start: () => Promise<T>): Promise<T> {
    if (this.current?.generation === generation) return this.current.request;

    const request = start();
    this.current = { generation, request };
    void request.then(
      () => this.release(request),
      () => this.release(request),
    );
    return request;
  }

  private release(request: Promise<T>) {
    if (this.current?.request === request) this.current = undefined;
  }
}

/** Serializes replacement so two agent lifecycles never own the same browser runtime. */
export class SerializedReplacementOwner<T> {
  private readonly closeValue: (value: T) => Promise<void>;
  private current: T | undefined;
  private generation = 0;
  private tail: Promise<void> = Promise.resolve();

  constructor(closeValue: (value: T) => Promise<void>) {
    this.closeValue = closeValue;
  }

  replace(create: () => Promise<T>): Promise<T | undefined> {
    const generation = ++this.generation;
    return this.enqueue(async () => {
      await this.closeCurrent();
      if (generation !== this.generation) return undefined;
      const candidate = await create();
      if (generation !== this.generation) {
        await this.closeValue(candidate);
        return undefined;
      }
      this.current = candidate;
      return candidate;
    });
  }

  clear(): Promise<void> {
    ++this.generation;
    return this.enqueue(() => this.closeCurrent());
  }

  private enqueue<R>(operation: () => Promise<R>): Promise<R> {
    const result = this.tail.then(operation);
    this.tail = result.then(() => undefined, () => undefined);
    return result;
  }

  private async closeCurrent() {
    const current = this.current;
    this.current = undefined;
    if (current !== undefined) await this.closeValue(current);
  }
}

export function availableVisualHeight({
  elementTop,
  minimum = 0,
  viewportHeight,
  viewportOffsetTop,
}: {
  elementTop: number;
  minimum?: number;
  viewportHeight: number;
  viewportOffsetTop: number;
}): number {
  const relativeTop = elementTop - viewportOffsetTop;
  return Math.max(minimum, Math.floor(viewportHeight - relativeTop));
}

export function terminalRunningForStatus(
  status: "idle" | "starting" | "ready" | "stopped" | "error",
  running: boolean,
): boolean {
  return status === "ready" && running;
}
