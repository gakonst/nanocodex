import { cloudflare } from "@cloudflare/vite-plugin";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const repositoryRoot = fileURLToPath(new URL("..", import.meta.url));
const productionReactScan = fileURLToPath(new URL("./src/reactScan.production.ts", import.meta.url));

export default defineConfig(({ command }) => ({
  plugins: [react(), cloudflare()],
  resolve: {
    alias: command === "build" ? { "react-scan": productionReactScan } : undefined,
    preserveSymlinks: true,
    dedupe: [
      "react",
      "react-dom",
      "nanocodex-react",
      "nanocodex-tui",
      "@pierre/theme",
      "@shikijs/core",
      "@shikijs/engine-javascript",
      "@shikijs/langs",
      "@shikijs/primitive",
      "@shikijs/types",
      "@tanstack/react-virtual",
      "shiki",
      "streamdown",
    ],
  },
  worker: { format: "es" },
  server: {
    fs: {
      allow: [repositoryRoot],
    },
  },
}));
