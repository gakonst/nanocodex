import Script from "next/script";

import { AgentTerminal } from "./agent-terminal";
import { WorkspaceTerminal } from "./workspace-terminal";

export const dynamic = "force-dynamic";

export default function Home() {
  return (
    <main>
      <header>
        <div>
          <p className="eyebrow">NANOCODEX / VERCEL WORKFLOWS</p>
          <h1>Durable workflow, synchronized clients.</h1>
        </div>
        <span id="status" className="pill">no session</span>
      </header>

      <section className="setup" aria-label="Session controls">
        <label htmlFor="admin-token">
          Creation token <span>optional</span>
          <input id="admin-token" type="password" autoComplete="off" placeholder="Only if this deployment requires one" />
        </label>
        <button id="new-session" type="button">New workflow</button>
        <label htmlFor="session-id">
          Workflow session ID
          <input id="session-id" inputMode="text" autoComplete="off" placeholder="wrun_…" />
        </label>
        <button id="join-session" type="button" className="secondary">Join</button>
        <button id="copy-session" type="button" className="secondary">Copy ID</button>
        <button id="detach" type="button" className="secondary">Detach</button>
      </section>

      <p className="meta">
        Session <code id="session">none</code>. Open this deployment in another browser and join the same
        workflow ID to synchronize prompts, streaming output, and durable results. Model credentials remain
        inside the Vercel Function step.
      </p>

      <AgentTerminal />

      <form id="prompt-form">
        <label className="sr-only" htmlFor="prompt">Agent prompt</label>
        <textarea id="prompt" rows={3} maxLength={1_048_576} placeholder="Ask the durable workflow actor…" required />
        <button id="send" type="submit" disabled>Run durably</button>
      </form>

      <WorkspaceTerminal />

      <footer>
        <span id="activity">idle</span>
        <span>Rust/WASM · Workflow actor · Vercel WebSockets</span>
      </footer>
      <Script src="/app.js" strategy="afterInteractive" />
    </main>
  );
}
