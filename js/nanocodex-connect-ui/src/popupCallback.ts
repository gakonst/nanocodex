import {
  callbackCompletionChannelName,
  callbackCompletionFor,
  callbackCompletionStorageKey,
  type CallbackCompletion,
} from "nanocodex-connect-protocol";

type PopupCallbackRuntime = Pick<Window, "addEventListener" | "removeEventListener" | "localStorage"> & Readonly<{
  BroadcastChannel?: typeof BroadcastChannel | undefined;
  location: Pick<Location, "origin">;
}>;

/**
 * Receives one exact popup completion across opener-preserving and COOP-separated
 * browser contexts. Same-origin storage makes a completion durable across a
 * parent reload; the OAuth state prevents another callback from settling it.
 */
export function observePopupCallback(
  expected: Readonly<{ connector: string; origin: string; source?: unknown; state: string }>,
  receive: (completion: CallbackCompletion) => void,
  runtime: PopupCallbackRuntime = window,
): () => void {
  if (runtime.location.origin !== expected.origin) {
    throw new Error("The popup callback origin does not match this page.");
  }
  const storageKey = callbackCompletionStorageKey(expected.state);
  let channel: BroadcastChannel | undefined;
  let delivered = false;

  const accept = (value: unknown) => {
    if (delivered) return;
    const completion = callbackCompletionFor(value, expected);
    if (!completion) return;
    delivered = true;
    dispose();
    try { runtime.localStorage.removeItem(storageKey); } catch {}
    receive(completion);
  };
  const onMessage = (event: MessageEvent<unknown>) => {
    if (event.origin !== expected.origin || event.source !== expected.source) return;
    accept(event.data);
  };
  const onStorage = (event: StorageEvent) => {
    try {
      if (event.key !== storageKey || event.storageArea !== runtime.localStorage || event.newValue === null) return;
      accept(JSON.parse(event.newValue));
    } catch {}
  };
  const onBroadcast = (event: MessageEvent<unknown>) => accept(event.data);
  const dispose = () => {
    runtime.removeEventListener("message", onMessage as EventListener);
    runtime.removeEventListener("storage", onStorage as EventListener);
    channel?.removeEventListener("message", onBroadcast);
    channel?.close();
    channel = undefined;
  };

  runtime.addEventListener("message", onMessage as EventListener);
  runtime.addEventListener("storage", onStorage as EventListener);
  try {
    if (runtime.BroadcastChannel) {
      channel = new runtime.BroadcastChannel(callbackCompletionChannelName(expected.state));
      channel.addEventListener("message", onBroadcast);
    }
  } catch {}
  try {
    const stored = runtime.localStorage.getItem(storageKey);
    if (stored !== null) accept(JSON.parse(stored));
  } catch {}
  return dispose;
}
