import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { registry } from "./registry.js";
import { startWebClient } from "./web-server.js";

const computePort = process.env.PORT ?? process.env.RIVET_PORT;
const stopping = shutdownSignal();
if (process.env.RIVETKIT_RUNTIME_MODE === "serverless") {
  await runServerless(stopping);
} else {
  await runEnvoy(stopping);
}

async function runServerless(stopping: Promise<"SIGINT" | "SIGTERM">): Promise<void> {
  const listening = registry.listen({
    host: process.env.NANOCODEX_WEB_HOST ?? "0.0.0.0",
    port: parsePort(process.env.NANOCODEX_WEB_PORT ?? computePort ?? "3000"),
    publicDir: fileURLToPath(new URL("../web/", import.meta.url)),
  });
  try {
    const first = await Promise.race([
      listening.then(() => ({ kind: "stopped" as const })),
      stopping.then((signal) => ({ kind: "signal" as const, signal })),
    ]);
    if (first.kind === "signal") process.exitCode = first.signal === "SIGINT" ? 130 : 143;
  } finally {
    await Promise.race([registry.shutdown(), delay(15_000)]);
    await Promise.race([listening, delay(15_000)]);
  }
}

async function runEnvoy(stopping: Promise<"SIGINT" | "SIGTERM">): Promise<void> {
  const web = await startWebClient({
    ...(process.env.NANOCODEX_WEB_HOST === undefined && computePort === undefined
      ? {}
      : { host: process.env.NANOCODEX_WEB_HOST ?? "0.0.0.0" }),
    ...(process.env.NANOCODEX_WEB_PORT === undefined && computePort === undefined
      ? {}
      : { port: parsePort(process.env.NANOCODEX_WEB_PORT ?? computePort) }),
  });
  process.stderr.write(`Nanocodex browser client: ${web.url}\n`);
  try {
    const first = await Promise.race([
      registry.startAndWait().then(() => ({ kind: "ready" as const })),
      stopping.then((signal) => ({ kind: "signal" as const, signal })),
    ]);
    const signal = first.kind === "signal" ? first.signal : await stopping;
    process.exitCode = signal === "SIGINT" ? 130 : 143;
  } finally {
    await Promise.race([registry.shutdown(), delay(15_000)]);
    await web.close();
  }
}

function shutdownSignal(): Promise<"SIGINT" | "SIGTERM"> {
  return new Promise((resolve) => {
    const received = (signal: "SIGINT" | "SIGTERM") => {
      process.removeListener("SIGINT", onInterrupt);
      process.removeListener("SIGTERM", onTerminate);
      terminateLocalEngineChildren();
      resolve(signal);
    };
    const onInterrupt = () => received("SIGINT");
    const onTerminate = () => received("SIGTERM");
    process.once("SIGINT", onInterrupt);
    process.once("SIGTERM", onTerminate);
  });
}

function terminateLocalEngineChildren(): void {
  if (process.env.RIVET_ENDPOINT || process.platform === "win32") return;
  // Rivet's local engine starts as our direct child but creates its own
  // process group. Terminate it synchronously before npm can tear down the
  // JavaScript process and orphan the engine.
  spawnSync("pkill", ["-TERM", "-P", String(process.pid)], { stdio: "ignore" });
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds).unref());
}

function parsePort(raw: string | undefined): number {
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new Error(`web port must be an integer between 1 and 65535; got ${JSON.stringify(raw)}`);
  }
  return port;
}
