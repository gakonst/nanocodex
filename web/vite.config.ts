import { cloudflare } from "@cloudflare/vite-plugin";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const repositoryRoot = fileURLToPath(new URL("..", import.meta.url));

export default defineConfig({
  plugins: [react(), cloudflare()],
  resolve: {
    preserveSymlinks: true,
    dedupe: ["react", "react-dom"],
  },
  worker: { format: "es" },
  server: {
    fs: {
      allow: [repositoryRoot],
    },
  },
});
