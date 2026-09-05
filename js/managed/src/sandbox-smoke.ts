import type { ToolMap } from "nanocodex";

import type { Sandbox } from "./sandbox-runtime";
import { cloudflareSandboxTools, destroyCloudflareSandbox } from "./sandbox-tools";

export async function cloudflareSandboxSmokeSetup(
  namespace: DurableObjectNamespace<Sandbox>,
  probeId: string,
  localBucket: boolean,
  publicOrigin: string,
  previewSecret: string,
): Promise<Record<string, unknown>> {
  requireProbeId(probeId);
  const started = Date.now();
  const marker = `CLOUDFLARE_SANDBOX_OK_${probeId}`;
  let tools = cloudflareSandboxTools(
    namespace,
    probeId,
    localBucket,
    publicOrigin,
    previewSecret,
  );
  try {
    // Wrangler's emulated R2 watcher takes its initial filesystem snapshot
    // asynchronously after mountBucket() returns. Let that baseline settle so
    // this write is observed as a change; production uses the direct R2 mount.
    if (localBucket) await exec(tools, "sleep 2");
    const write = record(await exec(
      tools,
      `printf %s ${marker} > probe.txt && test "$(cat probe.txt)" = ${marker} && printf EXEC_OK`,
    ));
    assert(write.exit_code === 0 && write.output === "EXEC_OK", "shell write/read failed");

    const list = record(await exec(tools, "find . -maxdepth 1 -name probe.txt -print"));
    assert(list.output === "./probe.txt\n", "shell listing omitted probe.txt");

    const nonzero = record(await exec(tools, "sh -c 'printf partial; printf failed >&2; exit 7'"));
    assert(
      nonzero.exit_code === 7 && nonzero.output === "partialfailed",
      "non-zero command result was not preserved",
    );

    const yielded = record(await invoke(tools, "exec_command", {
      cmd: "printf first; sleep 1; printf second",
      yield_time_ms: 250,
    }));
    assert(
      typeof yielded.session_id === "number" && yielded.output === "first",
      "command did not yield a resumable session",
    );
    const resumed = record(await invoke(tools, "write_stdin", {
      session_id: yielded.session_id,
    }));
    assert(
      resumed.exit_code === 0 && resumed.output === "second",
      "session resume replayed or lost command output",
    );

    const flood = record(await exec(
      tools,
      "node -e 'process.stdout.write(\"x\".repeat(140000)); process.stderr.write(\"y\".repeat(140000))'",
    ));
    assert(
      new TextEncoder().encode(String(flood.output)).byteLength === 40_000
        && flood.original_token_count === 70_000,
      "combined command output was not capped",
    );

    const server = record(await exec(tools, [
      "nohup node -e 'require(\"http\").createServer((q,s)=>require(\"fs\").createReadStream(\"/workspace/probe.txt\").pipe(s)).listen(8000)'",
      ">/tmp/nanocodex-sandbox-smoke.log 2>&1 &",
      "for i in $(seq 1 100); do curl -fsS http://127.0.0.1:8000/probe.txt >/dev/null && exit 0; sleep .1; done; exit 1",
    ].join(" ")));
    assert(server.exit_code === 0, "background preview server did not become ready");
    const preview = record(await invoke(tools, "preview", { port: 8000 }));
    const previewUrl = new URL("probe.txt", String(preview.url));
    assert(previewUrl.protocol === "https:", `preview returned an invalid URL: ${previewUrl.href}`);

    return {
      status: "ready",
      probe_id: probeId,
      marker,
      preview_url: previewUrl.href,
      checks: [
        "write_exec_read",
        "directory_list",
        "nonzero_exit",
        "session_resume",
        "bounded_output",
        "shell_managed_preview",
      ],
      duration_ms: Date.now() - started,
    };
  } catch (error) {
    await destroyCloudflareSandbox(namespace, probeId).catch(() => {});
    throw error;
  }
}

export async function cloudflareSandboxSmokeFinish(
  namespace: DurableObjectNamespace<Sandbox>,
  probeId: string,
  localBucket: boolean,
): Promise<Record<string, unknown>> {
  requireProbeId(probeId);
  const started = Date.now();
  const marker = `CLOUDFLARE_SANDBOX_OK_${probeId}`;
  try {
    await destroyCloudflareSandbox(namespace, probeId);
    const tools = cloudflareSandboxTools(namespace, probeId, localBucket);
    const persisted = record(await exec(tools, "cat probe.txt"));
    assert(persisted.output === marker, "R2 workspace did not survive container destruction");
    await exec(tools, "rm -f probe.txt");
    return {
      status: "ok",
      probe_id: probeId,
      checks: ["r2_restart_persistence"],
      duration_ms: Date.now() - started,
    };
  } finally {
    await destroyCloudflareSandbox(namespace, probeId).catch(() => {});
  }
}

function exec(tools: ToolMap, cmd: string): Promise<unknown> {
  return invoke(tools, "exec_command", { cmd });
}

async function invoke(tools: ToolMap, name: string, input: unknown): Promise<unknown> {
  const tool = tools[name];
  if (!tool) throw new Error(`missing tool: ${name}`);
  return tool.handler(input, {
    callId: "smoke",
    model: "gpt-5.6-sol",
    parentCallId: "smoke",
    sessionId: "smoke",
    signal: new AbortController().signal,
  });
}

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("tool returned a non-object result");
  }
  return value as Record<string, unknown>;
}

function requireProbeId(value: string): void {
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(value)) {
    throw new Error("sandbox smoke probe ID must be a safe identifier");
  }
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
