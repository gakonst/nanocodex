import { useState } from "react";
import "./LocalSponsoredTrialReset.css";

export function LocalSponsoredTrialReset({ onReset }: { onReset(): Promise<void> }) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();

  const reset = () => {
    if (pending) return;
    setPending(true);
    setError(undefined);
    void fetch("/api/dev/sponsored-trial/reset", {
      method: "POST",
      credentials: "same-origin",
      headers: { accept: "application/json" },
    }).then(async (response) => {
      if (!response.ok) {
        await response.body?.cancel();
        throw new Error("The local trial could not be reset.");
      }
      await response.body?.cancel();
      await onReset();
    }).catch((cause) => setError(errorMessage(cause)))
      .finally(() => setPending(false));
  };

  return <div className="homepage-trial-reset">
    <button disabled={pending} type="button" onClick={reset}>
      {pending ? "Resetting…" : "Reset trial"}
    </button>
    {error ? <span role="alert">{error}</span> : null}
  </div>;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
