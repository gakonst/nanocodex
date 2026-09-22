import { defineConfig } from "vitest/config";
export default defineConfig({ test: { environment: "node", include: ["test/thread-model-routing.test.ts", "test/gateway-runtime.test.ts"] } });
