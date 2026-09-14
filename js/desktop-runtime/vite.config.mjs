import { defineConfig } from "vite";
import { builtinModules } from "node:module";

export default defineConfig({
  ssr: { noExternal: true },
  build: {
    ssr: "src/host.mjs",
    outDir: "dist",
    target: "node22",
    minify: false,
    rollupOptions: {
      external: [...builtinModules, /^node:/],
      output: { entryFileNames: "host.mjs", chunkFileNames: "chunks/[name]-[hash].mjs" },
    },
  },
});
