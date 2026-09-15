// Packaged browser-kernel messages. This module installs no listeners.
export function isKernelRequest(value) {
  if (value === null || typeof value !== "object" || !("id" in value) ||
      typeof value.id !== "string" || !("type" in value)) return false;
  switch (value.type) {
    case "js-kernel:cancel":
    case "js-kernel:reset":
      return true;
    case "js-kernel:execute":
      return "script" in value && typeof value.script === "string";
    default:
      return false;
  }
}

export function isKernelResponse(value) {
  return value !== null && typeof value === "object" &&
    "id" in value && typeof value.id === "string" &&
    "ok" in value && typeof value.ok === "boolean";
}

export function kernelError(reason) {
  if (!(reason instanceof Error)) {
    return { message: typeof reason === "string" ? reason : "JavaScript execution failed", name: "Error" };
  }
  const error = { message: reason.message, name: reason.name };
  if (reason.stack != null) error.stack = reason.stack;
  return error;
}
