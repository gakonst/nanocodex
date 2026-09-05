export class BaseError extends Error {
  constructor(message, options = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "Connect.BaseError";
    this.code = options.code ?? "connect_error";
    this.status = options.status;
    this.metaMessages = Object.freeze([...(options.metaMessages ?? [])]);
  }
}

export class HttpError extends BaseError {
  constructor(status, message, options = {}) {
    super(message, { ...options, code: options.code ?? "http_error", status });
    this.name = "Connect.HttpError";
  }
}

export class InvalidResponseError extends BaseError {
  constructor(message, options = {}) {
    super(message, { ...options, code: "invalid_response" });
    this.name = "Connect.InvalidResponseError";
  }
}

export class UserRejectedRequestError extends BaseError {
  constructor(message = "The Nanocodex request was rejected.", options = {}) {
    super(message, { ...options, code: "user_rejected_request" });
    this.name = "Connect.UserRejectedRequestError";
  }
}

export class DialogBusyError extends BaseError {
  constructor() {
    super("The Nanocodex dialog is already handling a request.", {
      code: "dialog_busy",
    });
    this.name = "Connect.DialogBusyError";
  }
}
