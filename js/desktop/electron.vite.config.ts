import { defineConfig } from "electron-vite";
import react from "@vitejs/plugin-react";
import { resolve } from "node:path";

export default defineConfig({
  main: {
    build: {
      outDir: "dist/main",
      rollupOptions: {
        input: resolve("src/main/index.mjs"),
        external: ["electron", /^node:/],
      },
    },
  },
  preload: {
    build: {
      outDir: "dist/preload",
      rollupOptions: {
        input: resolve("src/preload/index.cjs"),
        external: ["electron"],
        output: { format: "cjs", entryFileNames: "index.cjs" },
      },
    },
  },
  renderer: {
    root: "src/renderer",
    plugins: [react()],
    build: {
      outDir: resolve("dist/renderer"),
      rollupOptions: { input: resolve("src/renderer/index.html") },
    },
  },
});
