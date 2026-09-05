import react from "@vitejs/plugin-react";
import { nanocodex } from "nanocodex-vite/cloudflare";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const repositoryRoot = fileURLToPath(new URL("../..", import.meta.url));

export default defineConfig({
  plugins: [react(), nanocodex({ chatGpt: false })],
  build: {
    // Vite 8's optional Lightning CSS loader currently resolves its npm entry
    // incorrectly in isolated npm installs. This example needs no CSS-specific
    // transforms, so use Vite's supported esbuild minifier.
    cssMinify: "esbuild",
  },
  resolve: {
    dedupe: ["better-auth", "nanocodex", "react", "react-dom"],
  },
  optimizeDeps: {
    exclude: ["nanocodex"],
  },
  worker: { format: "es" },
  server: {
    fs: { allow: [repositoryRoot] },
  },
});
