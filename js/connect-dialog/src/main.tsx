import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { logoutAccount } from "nanocodex-connect-ui/App";
import { App } from "./DialogApp";
import { startWalletHost } from "./protocol";
import "nanocodex-connect-ui/styles.css";

startWalletHost({ logout: logoutAccount });
document.documentElement.classList.add("connect-dialog-standalone");
function syncAppearance() {
  try {
    const theme = localStorage.getItem("nanocodex-theme");
    if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
    else delete document.documentElement.dataset.theme;
  } catch {
    // Storage can be unavailable in third-party frames. Use the system.
  }
}
syncAppearance();
window.addEventListener("storage", (event) => {
  if (event.key === "nanocodex-theme" || event.key === null) syncAppearance();
});

const root = document.getElementById("root");
if (!root) throw new Error("Nanocodex Connect root element is missing");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
