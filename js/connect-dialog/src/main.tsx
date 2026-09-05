import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { logoutAccount } from "nanocodex-connect-ui/App";
import { App } from "./DialogApp";
import { startWalletHost } from "./protocol";
import "nanocodex-connect-ui/styles.css";

startWalletHost({ logout: logoutAccount });
document.documentElement.classList.add("connect-dialog-standalone");

const root = document.getElementById("root");
if (!root) throw new Error("Nanocodex Connect root element is missing");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
