import react from "@vitejs/plugin-react";
import { nanocodex } from "nanocodex-vite";
import { defineConfig } from "vite";

const serverPort = process.env.PORT ? Number(process.env.PORT) : undefined;

if (serverPort !== undefined
  && (!Number.isSafeInteger(serverPort) || serverPort < 1 || serverPort > 65_535)) {
  throw new Error("PORT must be a valid TCP port");
}

export default defineConfig({
  plugins: [react(), nanocodex({ chatGpt: false })],
  resolve: {
    dedupe: ["@tanstack/react-query", "react", "react-dom"],
  },
  optimizeDeps: {
    exclude: ["nanocodex", "nanocodex-react"],
  },
  server: {
    allowedHosts: [".nanocodex.localhost"],
    host: process.env.HOST,
    origin: process.env.PORTLESS_URL,
    port: serverPort,
  },
  preview: {
    port: 4176,
    strictPort: true,
  },
});
