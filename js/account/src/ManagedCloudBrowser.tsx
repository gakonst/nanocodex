import { useEffect, useRef, useState } from "react";
import { Globe, X, ArrowDown, ArrowUp, CornerDownLeft } from "lucide-react";
import type { Agent } from "nanocodex/managed";
import "./ManagedCloudBrowser.css";

type BrowserAgent = Pick<Agent.Agent, "browser">;
type State = Awaited<ReturnType<BrowserAgent["browser"]["state"]>>;

export function ManagedCloudBrowser({ agent }: { agent: BrowserAgent }) {
  const [open, setOpen] = useState(false);
  const [state, setState] = useState<State>();
  useEffect(() => {
    let stopped = false;
    const refresh = async () => {
      try { const next = await agent.browser.state(); if (!stopped) setState(next); } catch { /* Panel presents errors on demand. */ }
    };
    void refresh();
    const timer = setInterval(() => { void refresh(); }, 4000);
    return () => { stopped = true; clearInterval(timer); };
  }, [agent]);
  return <>
    <button type="button" className="cloud-browser-open" onClick={() => setOpen(true)} title="Cloud browser" aria-label={state?.mode === "human" ? "Cloud browser needs you" : "Cloud browser"}>
      <Globe size={16} />{state?.mode === "human" && <span>Needs you</span>}
    </button>
    {open && <BrowserPanel agent={agent} onClose={() => setOpen(false)} />}
  </>;
}

function BrowserPanel({ agent, onClose }: { agent: BrowserAgent; onClose(): void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const image = useRef<HTMLImageElement>(null);
  const [state, setState] = useState<State>();
  const [target, setTarget] = useState("");
  const [frame, setFrame] = useState("");
  const [viewport, setViewport] = useState({ width: 1, height: 1 });
  const [address, setAddress] = useState("");
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const touch = useRef<{ x: number; y: number } | undefined>(undefined);
  const human = state?.mode === "human";
  useEffect(() => { dialog.current?.showModal(); }, []);
  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const next = await agent.browser.state();
        const selected = next.tabs.some((tab) => tab.id === target) ? target : next.tabs[0]?.id ?? "";
        if (stopped) return;
        setState(next);
        if (selected !== target) { setTarget(selected); setFrame(""); }
        if (selected) {
          const result = await agent.browser.action("frame", { generation: next.generation, target: selected });
          if (!stopped) { setFrame(`data:image/jpeg;base64,${result.data}`); setViewport({ width: result.width, height: result.height }); }
        } else setFrame("");
      } catch (e) { if (!stopped) setError(e instanceof Error ? e.message : "Browser unavailable"); }
      if (!stopped) timer = setTimeout(() => { void refresh(); }, 1000);
    };
    void refresh();
    return () => { stopped = true; clearTimeout(timer); };
  }, [agent, target]);
  const tab = state?.tabs.find((entry) => entry.id === target);
  useEffect(() => { setAddress(tab?.url ?? ""); setText(""); }, [tab?.url]);
  const run = async (operation: () => Promise<unknown>) => {
    setBusy(true); setError("");
    try { await operation(); setState(await agent.browser.state()); }
    catch (e) { setError(e instanceof Error ? e.message : "Browser action failed"); }
    finally { setBusy(false); }
  };
  const act = (operation: "click" | "scroll" | "type" | "key" | "navigate", extra: Record<string, string | number>) => {
    if (!human || !state || busy) return;
    void run(() => agent.browser.action(operation, { generation: state.generation, target, ...extra }));
  };
  const click = (clientX: number, clientY: number) => {
    const element = image.current;
    if (!element) return;
    const rect = element.getBoundingClientRect();
    act("click", { x: (clientX - rect.left) * viewport.width / rect.width, y: (clientY - rect.top) * viewport.height / rect.height });
  };
  return <dialog ref={dialog} className="cloud-browser" onCancel={onClose}>
    <header><strong>Cloud browser</strong><button onClick={onClose} aria-label="Close browser"><X size={20} /></button></header>
    <div className="cloud-browser-control">
      <span>{human ? state?.reason || "You have control" : "Agent has control"}</span>
      <button disabled={busy || !state} onClick={() => void run(async () => {
        if (human) { setText(""); await agent.browser.release(state!.generation); }
        else await agent.browser.takeover();
      })}>{human ? "Return to agent" : "Take control"}</button>
    </div>
    {!!state?.tabs.length && <nav aria-label="Browser tabs">{state.tabs.map((entry) => <button key={entry.id} aria-pressed={target === entry.id}
      onClick={() => { setTarget(entry.id); setText(""); }}>{entry.title || entry.url || "Tab"}</button>)}</nav>}
    <form className="cloud-browser-address" onSubmit={(event) => { event.preventDefault(); act("navigate", { url: address }); }}>
      <input aria-label="Website address" type="url" value={address} onChange={(e) => setAddress(e.target.value)} readOnly={!human} />
      {human && <button disabled={busy || !target}>Go</button>}
    </form>
    {error && <p role="alert" className="cloud-browser-error">{error}</p>}
    <div className="cloud-browser-page" onWheel={(event) => { if (human) act("scroll", { deltaY: Math.max(-2000, Math.min(2000, event.deltaY)) }); }}>
      {frame ? <img ref={image} src={frame} alt="Live cloud browser page" draggable={false}
        onClick={(event) => { if (event.detail !== 0) click(event.clientX, event.clientY); }}
        onTouchStart={(event) => { const point = event.touches[0]; if (point) touch.current = { x: point.clientX, y: point.clientY }; }}
        onTouchEnd={(event) => {
          const start = touch.current; const end = event.changedTouches[0]; touch.current = undefined;
          if (!start || !end || !human) return;
          event.preventDefault();
          const delta = start.y - end.clientY;
          if (Math.abs(delta) > 20) act("scroll", { deltaY: Math.round(delta * 3) });
          else click(end.clientX, end.clientY);
        }} /> : <p>{state?.available ? "Loading browser…" : "Ask the agent to open a website to start its cloud browser."}</p>}
    </div>
    {human && <footer>
      <p>Tap a field on the page, then enter text here. Input goes directly to this browser, outside the conversation.</p>
      <form onSubmit={(event) => { event.preventDefault(); if (!text || !state) return; const value = text; setText(""); void run(() => agent.browser.action("type", { target, generation: state.generation, pageUrl: tab?.url, text: value })); }}>
        <input type="password" aria-label="Private browser input" placeholder="Private input" value={text} onChange={(e) => setText(e.target.value)} autoComplete="off" />
        <button disabled={busy || !text || !target}>Type</button>
      </form>
      <div className="cloud-browser-keys">
        <button disabled={busy || !target} onClick={() => act("key", { key: "Tab" })}>Tab</button>
        <button disabled={busy || !target} onClick={() => act("key", { key: "Backspace" })}>⌫</button>
        <button disabled={busy || !target} onClick={() => act("key", { key: "Enter" })} aria-label="Enter"><CornerDownLeft size={18} /></button>
        <button disabled={busy || !target} onClick={() => act("scroll", { deltaY: -500 })} aria-label="Scroll up"><ArrowUp size={18} /></button>
        <button disabled={busy || !target} onClick={() => act("scroll", { deltaY: 500 })} aria-label="Scroll down"><ArrowDown size={18} /></button>
      </div>
    </footer>}
  </dialog>;
}
