export declare class BaseError extends Error {
  readonly code: string;
  readonly status: number | undefined;
  readonly metaMessages: readonly string[];
  constructor(message: string, options?: Readonly<{
    cause?: unknown;
    code?: string | undefined;
    status?: number | undefined;
    metaMessages?: readonly string[] | undefined;
  }>);
}

export declare class HttpError extends BaseError {
  constructor(status: number, message: string, options?: Readonly<{
    cause?: unknown;
    code?: string | undefined;
  }>);
}

export declare class InvalidResponseError extends BaseError {}
export declare class UserRejectedRequestError extends BaseError {}
export declare class DialogBusyError extends BaseError {}
