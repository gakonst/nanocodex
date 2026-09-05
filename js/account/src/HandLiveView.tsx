import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { Monitor } from "lucide-react";
import type { ManagedAgent, ManagedHandSurface, ManagedHandFrame } from "nanocodex/managed";
import "./HandLiveView.css";

const surfaceKey = (surface: ManagedHandSurface) => `${surface.source}:${surface.machine_id}:${surface.id}`;

export function HandLiveView({ agent }: { agent: ManagedAgent }) {
  const [open, setOpen] = useState(false);
  const [paused, setPaused] = useState(false);
  const [visible, setVisible] = useState(document.visibilityState === "visible");
  const [surfaces, setSurfaces] = useState<readonly ManagedHandSurface[]>([]);
  const [selected, setSelected] = useState("");
  const [frame, setFrame] = useState<ManagedHandFrame>();
  const [status, setStatus] = useState("Connecting…");
  const [attempt, setAttempt] = useState(0);
  const surface = surfaces.find((candidate) => surfaceKey(candidate) === selected) ?? surfaces[0];

  useEffect(() => {
    const visibility = () => setVisible(document.visibilityState === "visible");
    const escape = (event: KeyboardEvent) => { if (event.key === "Escape") setOpen(false); };
    document.addEventListener("visibilitychange", visibility);
    document.addEventListener("keydown", escape);
    return () => { document.removeEventListener("visibilitychange", visibility); document.removeEventListener("keydown", escape); };
  }, []);
  useEffect(() => {
    if (!open || !visible) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const next = await agent.hands.list({ signal: controller.signal });
        if (!controller.signal.aborted) setSurfaces(next);
      } catch {
        if (!controller.signal.aborted) { setSurfaces([]); setStatus("Hands unavailable"); }
      } finally {
        if (!controller.signal.aborted) timer = setTimeout(refresh, 3_000);
      }
    };
    void refresh();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [agent, open, visible]);
  useEffect(() => {
    setFrame(undefined);
    if (!open || paused || !visible || !surface) return;
    const controller = new AbortController();
    let retry: ReturnType<typeof setTimeout>;
    setStatus("Connecting…");
    void (async () => {
      try {
        for await (const result of agent.hands.frames(surface, { signal: controller.signal })) {
          if (controller.signal.aborted) break;
          setFrame(result.status === "frame" ? result.frame : undefined);
          setStatus(result.status === "frame" ? "Live" : result.message);
        }
      } catch {
        if (!controller.signal.aborted) {
          setFrame(undefined); setStatus("Reconnecting…");
          retry = setTimeout(() => setAttempt((value) => value + 1), 1_000);
        }
      }
    })();
    return () => { controller.abort(); clearTimeout(retry); };
  }, [agent, open, paused, visible, surface?.id, surface?.source, surface?.route_token, attempt]);

  return <>
    <button type="button" aria-label="Live view" title="Live view" aria-expanded={open} onClick={() => setOpen(!open)}><Monitor size={18} /></button>
    {open && createPortal(<section className="hand-live-view" aria-label="Hand live view">
      <div className="hand-live-toolbar">
        <strong>Live view</strong>
        <button type="button" onClick={() => setPaused(!paused)}>{paused ? "Resume" : "Pause"}</button>
        <button type="button" aria-label="Close live view" onClick={() => setOpen(false)}>Close</button>
      </div>
      {surfaces.length > 0 && <label>Screen<select aria-label="Screen" value={surface ? surfaceKey(surface) : ""} onChange={(event) => setSelected(event.target.value)}>
        {surfaces.map((entry) => <option key={surfaceKey(entry)} value={surfaceKey(entry)}>{entry.machine_name} · {entry.name}</option>)}
      </select></label>}
      <div className="hand-live-screen">
        {frame && !paused && visible ? <img alt={surface?.name ?? "Hand screen"} width={frame.width} height={frame.height}
          src={`data:${frame.mime_type};base64,${frame.data}`} onError={() => { setFrame(undefined); setStatus("Invalid screen image"); }} />
          : <p>{paused ? "Paused" : !visible ? "Paused while hidden" : !surface ? "No screens connected" : status}</p>}
      </div>
      <small role="status">{paused ? "Paused" : !surface ? "No screens connected" : status}{frame && !paused ? ` · ${new Date(frame.captured_at).toLocaleTimeString()}` : ""}</small>
    </section>, document.body)}
  </>;
}
