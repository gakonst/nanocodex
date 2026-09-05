type BrowserAgentScope = Pick<typeof globalThis, "WebAssembly" | "WebSocket" | "Worker"> & {
  crypto?: Pick<Crypto, "randomUUID">;
  isSecureContext?: boolean;
  navigator?: {
    locks?: Pick<LockManager, "request">;
    storage?: Pick<StorageManager, "getDirectory">;
  };
};

/** Returns the first platform capability the complete browser agent actually requires. */
export function browserAgentCapabilityError(
  scope: BrowserAgentScope = globalThis,
): string | undefined {
  if (scope.isSecureContext === false) {
    return "The browser agent needs a secure HTTPS page for its private workspace.";
  }
  if (typeof scope.Worker !== "function") {
    return "The browser agent needs Web Workers. Update Safari or open this page in a current browser.";
  }
  if (
    typeof scope.WebAssembly !== "object"
    || typeof scope.WebAssembly.instantiate !== "function"
  ) {
    return "The browser agent needs WebAssembly. Update Safari or open this page in a current browser.";
  }
  if (typeof scope.WebSocket !== "function") {
    return "The browser agent needs WebSockets. Check browser content restrictions, then retry.";
  }
  if (typeof scope.crypto?.randomUUID !== "function") {
    return "The browser agent needs secure random IDs. Open this page over HTTPS in a current browser.";
  }
  if (typeof scope.navigator?.storage?.getDirectory !== "function") {
    return "The browser agent needs private workspace storage (OPFS). Update Safari or allow website storage, then retry.";
  }
  if (typeof scope.navigator?.locks?.request !== "function") {
    return "The browser agent needs Web Locks to protect its workspace. Update Safari or open this page in a current browser.";
  }
  return undefined;
}
