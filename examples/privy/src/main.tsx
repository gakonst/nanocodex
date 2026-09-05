import { PrivyProvider } from "@privy-io/react-auth";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App, type PublicConfiguration } from "./App";
import "./styles.css";

async function start() {
  const root = createRoot(document.getElementById("root")!);
  try {
    const response = await fetch("/api/config", {
      credentials: "same-origin",
      headers: { accept: "application/json" },
    });
    if (!response.ok) throw new Error(`Configuration request failed (${response.status}).`);
    const configuration = await response.json() as PublicConfiguration;
    root.render(
      <StrictMode>
        <PrivyProvider
          appId={configuration.privyAppId}
          config={{
            loginMethods: ["email"],
            appearance: { theme: "dark", accentColor: "#9eff6e" },
          }}
        >
          <App configuration={configuration} />
        </PrivyProvider>
      </StrictMode>,
    );
  } catch (reason) {
    const message = reason instanceof Error ? reason.message : "Unable to start the example.";
    root.render(<main className="boot-error" role="alert">{message}</main>);
  }
}

void start();
