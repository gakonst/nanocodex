import { defineConfig } from "vitest/config";
export default defineConfig({ test: { environment: "node", include: ["test/provider-telemetry-routing.test.ts", "test/provider-probe-schedule.test.ts", "test/provider-probe-slots.test.ts", "test/thread-model-routing.test.ts", "test/gateway-runtime.test.ts", "test/subagent-model-routing.test.ts"] } });
