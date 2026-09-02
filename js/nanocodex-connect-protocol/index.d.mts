/** Frames a valid broker authorization state for a Connect callback. */
export declare function scopedConnectConnectorState(value: unknown): string;

/** Reports whether a value is a framed Connect connector callback state. */
export declare function isScopedConnectConnectorState(value: unknown): value is string;

/** Returns the broker state from a valid framed callback state. */
export declare function unscopedConnectConnectorState(value: unknown): string | undefined;

export type CallbackCompletion = Readonly<{
  type: "nanocodex:callback-complete";
  connector: string;
  state: string;
  result: "success" | "error";
  error?: string | undefined;
  message?: string | undefined;
}>;

/** Frames a secret-free terminal browser callback notification. */
export declare function callbackCompletion(parameters: Readonly<{
  connector: string;
  state: string;
  result: "success" | "error";
  error?: string | undefined;
  message?: string | undefined;
}>): CallbackCompletion;

/** Reports whether a value is an exact terminal browser callback frame. */
export declare function isCallbackCompletion(value: unknown): value is CallbackCompletion;

/** Matches a callback frame to the exact connector and correlation state. */
export declare function callbackCompletionFor(
  value: unknown,
  expected: Readonly<{ connector: string; state: string }>,
): CallbackCompletion | undefined;

/** Returns the same-origin persistence key for a callback state. */
export declare function callbackCompletionStorageKey(state: string): string;

/** Returns the same-origin broadcast channel for a callback state. */
export declare function callbackCompletionChannelName(state: string): string;

/** Reports whether a value is a canonical callback correlation state. */
export declare function isCallbackCompletionState(value: unknown): value is string;
