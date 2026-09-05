import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "nanocodex-terminal/styles.css";
import "./style.css";

const container = document.getElementById("root");
if (!container) throw new Error("Nanocodex side panel root is missing");

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
