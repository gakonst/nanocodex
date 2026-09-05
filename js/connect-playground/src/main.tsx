import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NanocodexProvider } from "nanocodex-react/connect";

import { App } from "./App";
import { config } from "./config";
import "nanocodex-terminal/styles.css";
import "./styles.css";

const root = document.getElementById("root");

if (!root) throw new Error("Connect playground root is missing");

const queryClient = new QueryClient();

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <NanocodexProvider config={config}>
        <App />
      </NanocodexProvider>
    </QueryClientProvider>
  </StrictMode>,
);
