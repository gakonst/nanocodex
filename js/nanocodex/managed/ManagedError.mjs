export class ManagedError extends Error {
  constructor(code, message, options = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "ManagedError";
    this.code = code;
    this.status = options.status;
  }
}
