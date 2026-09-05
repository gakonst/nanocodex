import react from "@vitejs/plugin-react";
import { nanocodex } from "nanocodex-vite/cloudflare";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const repositoryRoot = fileURLToPath(new URL("../..", import.meta.url));

export default defineConfig({
  plugins: [react(), nanocodex()],
  build: {
    manifest: true,
  },
  worker: { format: "es" },
  server: {
    fs: {
      // The example consumes the generated WASM package and browser host from
      // js/nanocodex without copying either artifact into the application.
      allow: [repositoryRoot],
    },
  },
});
