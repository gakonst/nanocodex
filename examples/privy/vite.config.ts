import react from "@vitejs/plugin-react";
import { nanocodex } from "nanocodex-vite/cloudflare";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [nanocodex({ chatGpt: false }), react()],
  build: { manifest: true },
  worker: { format: "es" },
});
