import { readFile } from "node:fs/promises";

import { describe, expect, it } from "vitest";

describe("browser demo", () => {
  it("uses resumable WebSockets and synchronizes only client-safe state", async () => {
    const source = await readFile(new URL("../public/app.js", import.meta.url), "utf8");
    expect(source).toContain("new WebSocket");
    expect(source).toContain("startIndex");
    expect(source).toContain("stream_event");
    expect(source).toContain("window.addEventListener(\"storage\"");
    expect(source).toContain("assistant.delta");
    expect(source).toContain("nanocodex:workflow-session");
    expect(source).toContain("nanocodex:agent-terminal-snapshot");
    expect(source).not.toContain("CHATGPT_ACCESS_TOKEN");
    expect(source).not.toContain("OPENAI_API_KEY");
    expect(source).not.toContain("cloud_api_");
  });

  it("renders the agent stream through wterm instead of the bundled TUI", async () => {
    const page = await readFile(new URL("../app/page.tsx", import.meta.url), "utf8");
    const terminal = await readFile(
      new URL("../app/agent-terminal.tsx", import.meta.url),
      "utf8",
    );
    expect(page).toContain("<AgentTerminal />");
    expect(page).toContain('htmlFor="prompt"');
    expect(page).toContain('strategy="afterInteractive"');
    expect(page).not.toContain('id="transcript"');
    expect(terminal).toContain('from "@wterm/react"');
    expect(terminal).toContain("mounted ? (");
    expect(terminal).toContain("renderAgentTerminal");
    expect(terminal).toContain("labelWtermInput");
    expect(terminal).not.toContain("nanocodex-tui-react");
  });

  it("nonces the Next bootstrap so the terminal islands can hydrate", async () => {
    const proxy = await readFile(new URL("../proxy.ts", import.meta.url), "utf8");
    const page = await readFile(new URL("../app/page.tsx", import.meta.url), "utf8");
    expect(proxy).toContain("crypto.randomUUID()");
    expect(proxy).toContain("'strict-dynamic'");
    expect(proxy).toContain("'wasm-unsafe-eval'");
    expect(proxy).toContain('requestHeaders.set("Content-Security-Policy", policy)');
    expect(proxy).not.toContain("'unsafe-inline'");
    expect(page).toContain('dynamic = "force-dynamic"');
  });

  it("keeps the PTY attachment separate from the replayable agent stream", async () => {
    const terminal = await readFile(
      new URL("../app/workspace-terminal.tsx", import.meta.url),
      "utf8",
    );
    expect(terminal).toContain("terminalStartFrame");
    expect(terminal).toContain("terminalInputFrame");
    expect(terminal).toContain("NANOCODEX_TERMINAL_TOKEN");
    expect(terminal).not.toContain("startIndex");
    expect(terminal).not.toContain("stream_event");
  });

  it("keeps model credentials behind the server-side workflow step", async () => {
    const page = await readFile(new URL("../app/page.tsx", import.meta.url), "utf8");
    const workflow = await readFile(
      new URL("../workflows/nanocodex-actor.ts", import.meta.url),
      "utf8",
    );
    expect(page).toContain("Model credentials remain");
    expect(workflow).toContain('"use workflow"');
    expect(workflow).toContain('"use step"');
    expect(workflow).toContain("getWritable<SessionEvent>");
  });
});
