import { useCallback, useEffect, useRef, useState } from "react";
import { Agent, type ManagedAgent } from "nanocodex/managed";
import { createTools, type Tools } from "nanocodex/tools";
import { useAccountSession } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import {
  type CredentialSource,
  type ModelSessionStatus,
  useModelSession,
} from "./modelSession";
import {
  DETACHED_REFUSAL_MARKER,
  HOSTED_ECHO_TOOL,
  detachHostedToolsCatalog,
  proveDetachedToolRefusal,
  publishHostedToolsCatalog,
  replaceAndFenceHostedToolsCatalog,
  runAttachedEcho,
  type HostedToolExecution,
  type HostedToolsAttachment,
} from "./hostedToolsDemoRuntime";
import "./HostedToolsDemo.css";

type DemoRuntime = {
  active: HostedToolsAttachment | undefined;
  agent: ManagedAgent;
  generation: number;
  tools: Tools;
};

type Proofs = Readonly<{
  catalog?: string;
  detached?: string;
  echo?: string;
  fence?: string;
  reconnect?: string;
}>;

type Operation = "attach" | "detach" | "echo" | "fence" | "reconnect" | "reset";

const DEFAULT_ECHO = "hello from the managed Nanocodex agent";

export function HostedToolsDemo() {
  const account = useAccountSession();
  const [modelStatus, setModelStatus] = useState<ModelSessionStatus>();
  const [credentialSource, setCredentialSource] = useState<CredentialSource>();
  const { retrySession } = useModelSession({
    onStatusChange: setModelStatus,
    onSourceChange: setCredentialSource,
  });
  const [agentId, setAgentId] = useState<string>();
  const [busy, setBusy] = useState<Operation>();
  const [echoInput, setEchoInput] = useState(DEFAULT_ECHO);
  const [error, setError] = useState<string>();
  const [executions, setExecutions] = useState<readonly HostedToolExecution[]>([]);
  const [proofs, setProofs] = useState<Proofs>({});
  const accountIdRef = useRef(account.account?.id);
  const executionRef = useRef<readonly HostedToolExecution[]>([]);
  const runtimeRef = useRef<DemoRuntime | undefined>(undefined);
  accountIdRef.current = account.account?.id;

  useEffect(() => {
    const previous = runtimeRef.current;
    runtimeRef.current = undefined;
    executionRef.current = [];
    setAgentId(undefined);
    setBusy(undefined);
    setError(undefined);
    setExecutions([]);
    setProofs({});
    if (previous) void previous.tools.close();
  }, [account.account?.id]);
  useEffect(() => () => {
    const runtime = runtimeRef.current;
    runtimeRef.current = undefined;
    if (runtime) void runtime.tools.close();
  }, []);

  const ensureRuntime = useCallback(async (): Promise<DemoRuntime> => {
    if (runtimeRef.current) return runtimeRef.current;
    const accountId = accountIdRef.current;
    if (!accountId) throw new Error("sign in by SMS before attaching browser tools");
    const storageKey = `nanocodex.hosted-tools-demo.agent.v1.${accountId}`;
    const retainedId = safeGet(storageKey);
    const listed = await Agent.list();
    const agent = listed.find(({ id }) => id === retainedId)
      ?? await Agent.create();
    const tools = await createTools({
      mcp: false,
      attachmentId: "browser",
      machines: [{
        id: "browser",
        name: "This browser",
        workspace: "browser://current-tab",
        capabilities: ["browser-echo"],
      }],
      tools: {
        [HOSTED_ECHO_TOOL]: {
          description: "Echo a message in the caller's currently attached browser host.",
          parameters: {
            type: "object",
            properties: { message: { type: "string", minLength: 1, maxLength: 2_000 } },
            required: ["message"],
            additionalProperties: false,
          },
          outputSchema: {
            type: "object",
            properties: {
              echoed: { type: "string" },
              executed_by: { const: "browser caller" },
              host_generation: { type: "integer" },
            },
            required: ["echoed", "executed_by", "host_generation"],
            additionalProperties: false,
          },
          handler(input, context) {
            const message = echoMessage(input);
            const execution = Object.freeze({
              callId: context.callId,
              generation: runtimeRef.current?.generation ?? 0,
              message,
              sessionId: context.sessionId,
            });
            executionRef.current = Object.freeze([...executionRef.current, execution]);
            setExecutions(executionRef.current);
            return Object.freeze({
              echoed: message,
              executed_by: "browser caller",
              host_generation: execution.generation,
            });
          },
        },
      },
    });
    if (accountIdRef.current !== accountId) {
      await tools.close();
      throw new Error("the active account changed while the browser tool host was opening");
    }
    const runtime = { active: undefined, agent, generation: 0, tools };
    runtimeRef.current = runtime;
    safeSet(storageKey, agent.id);
    setAgentId(agent.id);
    return runtime;
  }, []);

  const operate = useCallback((operation: Operation, task: () => Promise<void>) => {
    if (busy) return;
    setBusy(operation);
    setError(undefined);
    void task().catch((cause) => {
      setError(clientFailureMessage(
        cause,
        "The hosted-tools proof could not finish. Check the account connection and retry this step.",
      ));
    }).finally(() => setBusy(undefined));
  }, [busy]);

  const attach = useCallback(() => operate("attach", async () => {
    const runtime = await ensureRuntime();
    const attachment = await publishHostedToolsCatalog(runtime.tools, runtime.agent);
    runtime.active = attachment;
    runtime.generation += 1;
    setProofs((current) => ({
      ...current,
      catalog: `Catalog acknowledged for host generation ${runtime.generation}.`,
    }));
  }), [ensureRuntime, operate]);

  const echo = useCallback(() => operate("echo", async () => {
    const runtime = runtimeRef.current;
    if (!runtime?.active?.client.connected) throw new Error("attach the browser tool catalog first");
    const result = await runAttachedEcho(runtime.agent, echoInput, () => executionRef.current);
    setProofs((current) => ({
      ...current,
      echo: `Browser handler #${executionRef.current.length} returned ${JSON.stringify(result.execution.message)}. Managed reply: ${visibleText(result.finalMessage)}`,
    }));
  }), [echoInput, operate]);

  const detach = useCallback(() => operate("detach", async () => {
    const runtime = runtimeRef.current;
    if (!runtime) throw new Error("publish the browser tool catalog first");
    if (runtime.active) {
      const active = runtime.active;
      runtime.active = undefined;
      await detachHostedToolsCatalog(active);
    }
    const finalMessage = await proveDetachedToolRefusal(runtime.agent, () => executionRef.current);
    setProofs((current) => ({
      ...current,
      detached: `No browser handler ran. Managed reply: ${visibleText(finalMessage)}`,
    }));
  }), [operate]);

  const reconnect = useCallback(() => operate("reconnect", async () => {
    const runtime = runtimeRef.current;
    if (!runtime) throw new Error("publish the browser tool catalog first");
    const attachment = await publishHostedToolsCatalog(runtime.tools, runtime.agent);
    runtime.active = attachment;
    runtime.generation += 1;
    setProofs((current) => ({
      ...current,
      reconnect: `Catalog republished and ready as host generation ${runtime.generation}.`,
    }));
  }), [operate]);

  const fence = useCallback(() => operate("fence", async () => {
    const runtime = runtimeRef.current;
    if (!runtime?.active) throw new Error("reconnect the browser tool host first");
    const stale = runtime.generation;
    const successor = await replaceAndFenceHostedToolsCatalog(
      runtime.tools,
      runtime.agent,
      runtime.active,
    );
    runtime.active = successor;
    runtime.generation += 1;
    setProofs((current) => ({
      ...current,
      fence: `Generation ${stale} closed only after generation ${runtime.generation} became routing-ready.`,
    }));
  }), [operate]);

  const reset = useCallback(() => operate("reset", async () => {
    const runtime = runtimeRef.current;
    runtimeRef.current = undefined;
    if (runtime) await runtime.tools.close();
    executionRef.current = [];
    setAgentId(undefined);
    setEchoInput(DEFAULT_ECHO);
    setExecutions([]);
    setProofs({});
  }), [operate]);

  if (account.status !== "ready" || modelStatus === undefined) return null;
  const modelReady = modelStatus.state === "ready"
    && modelStatus.ready
    && credentialSource === "brokered";
  const enabled = Boolean(account.account && modelReady);
  const complete = Boolean(proofs.fence);

  return (
    <section className="hosted-tools-demo page-grid" aria-labelledby="hosted-tools-title">
      <header className="hosted-tools-hero">
        <p className="eyebrow">Demos · Managed agent</p>
        <h1 id="hosted-tools-title">Attach a browser tool to a hosted agent.</h1>
        <p>
          This page publishes a caller-defined echo catalog through the managed broker. Provider
          credentials stay server-side; the browser receives only its account-scoped tool-host route.
        </p>
        <pre><code>{`const tools = await createTools({\n  attachmentId: "browser",\n  machines: [{ id: "browser", name: "This browser", workspace: "browser://current-tab", capabilities: ["browser-echo"] }],\n  tools: { ${HOSTED_ECHO_TOOL} },\n});\nawait tools.attach(agent.toolsTarget()).connect();`}</code></pre>
      </header>

      <dl className="hosted-tools-facts" aria-label="Hosted tool connection">
        <div><dt>Account</dt><dd>{account.account ? "SMS session" : "Required"}</dd></div>
        <div><dt>Agent</dt><dd>{agentId ? shortId(agentId) : "Chosen on attach"}</dd></div>
        <div><dt>Execution</dt><dd>{executions.length ? `${executions.length} browser call${executions.length === 1 ? "" : "s"}` : "No browser calls"}</dd></div>
      </dl>

      {!account.account ? (
        <div className="hosted-tools-gate" role="status">
          Sign in by SMS from the account menu to use the managed broker.
        </div>
      ) : !modelReady ? (
        <div className="hosted-tools-gate" role={modelStatus.state === "error" ? "alert" : "status"}>
          <span>{modelStatus.state === "error"
            ? modelStatus.error
            : "Connect ChatGPT or an OpenAI API key from the account menu before running hosted turns."}</span>
          <button type="button" onClick={() => void retrySession()}>Retry connection</button>
        </div>
      ) : null}

      {error ? <p className="hosted-tools-error" role="alert">{error}</p> : null}

      <ol className="hosted-tools-proof" aria-busy={busy !== undefined}>
        <ProofStep
          index="01"
          title="Publish and await catalog readiness"
          description="The SDK resolves connect only after the managed broker accepts the immutable catalog."
          proof={proofs.catalog}
          action="Attach browser echo"
          disabled={!enabled || Boolean(busy) || Boolean(proofs.catalog)}
          onAction={attach}
        />
        <ProofStep
          index="02"
          title="Execute in this browser"
          description="A durable managed turn discovers browser_echo and routes its exact call back here."
          proof={proofs.echo}
          action="Run attached echo"
          disabled={!proofs.catalog || Boolean(busy) || Boolean(proofs.echo)}
          onAction={echo}
        >
          <label className="hosted-tools-input">
            <span>Echo input</span>
            <input
              value={echoInput}
              maxLength={2_000}
              disabled={!proofs.catalog || Boolean(proofs.echo) || Boolean(busy)}
              onChange={(event) => setEchoInput(event.target.value)}
            />
          </label>
        </ProofStep>
        <ProofStep
          index="03"
          title="Detach and prove refusal"
          description={`The catalog is drained first. A follow-on turn must report ${DETACHED_REFUSAL_MARKER} without invoking the browser.`}
          proof={proofs.detached}
          action="Detach and request tool"
          disabled={!proofs.echo || Boolean(busy) || Boolean(proofs.detached)}
          onAction={detach}
        />
        <ProofStep
          index="04"
          title="Reconnect the same caller"
          description="A fresh attachment republishes the same catalog and receives a new routing generation."
          proof={proofs.reconnect}
          action="Reconnect browser host"
          disabled={!proofs.detached || Boolean(busy) || Boolean(proofs.reconnect)}
          onAction={reconnect}
        />
        <ProofStep
          index="05"
          title="Fence the stale host"
          description="A successor connection becomes ready, and the broker policy-closes the prior generation."
          proof={proofs.fence}
          action="Replace host and fence stale"
          disabled={!proofs.reconnect || Boolean(busy) || Boolean(proofs.fence)}
          onAction={fence}
        />
      </ol>

      {complete ? (
        <footer className="hosted-tools-complete">
          <strong>All broker proofs passed.</strong>
          <span>The current successor remains attached until this page closes or you reset the demo.</span>
          <button type="button" disabled={Boolean(busy)} onClick={reset}>Reset demo</button>
        </footer>
      ) : null}
    </section>
  );
}

function ProofStep({
  action,
  children,
  description,
  disabled,
  index,
  onAction,
  proof,
  title,
}: {
  action: string;
  children?: React.ReactNode;
  description: string;
  disabled: boolean;
  index: string;
  onAction(): void;
  proof?: string;
  title: string;
}) {
  return (
    <li className={proof ? "is-passed" : ""}>
      <span className="hosted-tools-step-index" aria-hidden="true">{index}</span>
      <div className="hosted-tools-step-copy">
        <header><h2>{title}</h2><strong>{proof ? "Passed" : "Not run"}</strong></header>
        <p>{description}</p>
        {children}
        {proof ? <output>{proof}</output> : null}
      </div>
      <button type="button" disabled={disabled} onClick={onAction}>{action}</button>
    </li>
  );
}

function echoMessage(input: unknown): string {
  if (!input || typeof input !== "object" || Array.isArray(input)) {
    throw new TypeError("browser_echo input must be an object");
  }
  const message = (input as Record<string, unknown>).message;
  if (typeof message !== "string" || !message.trim() || message.length > 2_000) {
    throw new TypeError("browser_echo message must contain 1 through 2,000 characters");
  }
  return message;
}

function shortId(value: string): string {
  return value.length > 18 ? `${value.slice(0, 8)}…${value.slice(-6)}` : value;
}

function visibleText(value: string): string {
  const compact = value.replace(/\s+/g, " ").trim();
  return compact.length > 360 ? `${compact.slice(0, 357)}…` : compact;
}

function safeGet(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}

function safeSet(key: string, value: string) {
  try { localStorage.setItem(key, value); } catch {}
}
