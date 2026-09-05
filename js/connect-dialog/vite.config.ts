import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  base: "/connect-dialog/",
  plugins: [react()],
  resolve: {
    dedupe: ["react", "react-dom"],
  },
  server: {
    port: 4177,
    strictPort: true,
  },
  preview: {
    port: 4177,
    strictPort: true,
  },
});
