export declare class ManagedError extends Error {
  readonly code: string;
  readonly status: number | undefined;
  constructor(
    code: string,
    message: string,
    options?: { status?: number | undefined; cause?: unknown },
  );
}
