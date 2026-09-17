import { useQuery } from "@tanstack/react-query";
import { useAccountSession } from "./AccountSession";
import { accountQueryKey } from "./queryClient";
import { useEffect, useRef, useState, type PointerEvent, type KeyboardEvent, type MouseEvent } from "react";
import { createPortal } from "react-dom";
import { Monitor, X } from "lucide-react";
import { canStartBroadcast, listRemoteHands, RemoteBrowserSession, remoteKeys, type RemoteHand, type BroadcastPreset, type RemoteState } from "./handRemote";
import "./RemoteScreens.css";

export function RemoteScreens({ showLabel = false }: { showLabel?: boolean }) {
  const accountId = useAccountSession().account?.id;
  const [open, setOpen] = useState(false);
  // Warm discovery while the account UI is visible; no viewer/media connection
  // is opened until a screen is selected. The dialog reuses this account cache.
  useQuery({
    queryKey: [...accountQueryKey(accountId), "remote-screens"],
    queryFn: ({ signal }) => listRemoteHands(signal),
    enabled: Boolean(accountId), staleTime: 5_000,
  });
  return <>
    <button type="button" className="remote-screens-open" aria-haspopup="dialog" aria-label="Remote screens" title="Remote screens"
      onClick={() => setOpen(true)}><Monitor size={17} aria-hidden="true" />{showLabel && "Screens"}</button>
    {open && createPortal(<ScreensDialog key={accountId} onClose={() => setOpen(false)} />, document.body)}
  </>;
}

function ScreensDialog({ onClose }: { onClose(): void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const accountId = useAccountSession().account?.id;
  const query = useQuery({
    queryKey: [...accountQueryKey(accountId), "remote-screens"],
    queryFn: ({ signal }) => listRemoteHands(signal),
    enabled: Boolean(accountId),
    staleTime: 5_000,
    refetchInterval: 5_000,
  });
  const hands = query.data ?? [];
  const error = query.error?.message;
  const [selected, setSelected] = useState<RemoteHand>();
  useEffect(() => {
    dialog.current?.showModal();
    return () => dialog.current?.close();
  }, []);
  return <dialog ref={dialog} className="remote-screens" aria-labelledby="remote-screens-title"
    onCancel={event => { event.preventDefault(); if (!selected) onClose(); }}>
    <header><h2 id="remote-screens-title">{selected ? `${selected.machine_name} · ${selected.name}` : "Remote screens"}</h2>
      <button type="button" aria-label="Close remote screens" onClick={onClose}><X size={18} /></button></header>
    {selected ? <Screen key={`${selected.machine_id}:${selected.id}`} hand={selected} onBack={() => setSelected(undefined)} /> : <div className="remote-screen-list">
      {error && <p role="alert">{error}</p>}
      {!hands.length && !error && <p role={query.isPending && accountId ? "status" : undefined}>
        {!accountId ? "Sign in to view your remote screens." : query.isPending ? "Loading remote screens…"
          : "Start screen sharing on a connected Hand to view and control it here."}
      </p>}
      {hands.map(hand => <button type="button" key={`${hand.machine_id}:${hand.id}`} data-testid={`remote-screen:${hand.machine_id}:${hand.id}`} onClick={() => setSelected(hand)}>
        <Monitor size={22} aria-hidden="true" /><span><strong>{hand.machine_name}</strong><small>{hand.name}</small></span>
        <small>{hand.controllable ? "View and control" : "View only"}</small>
      </button>)}
    </div>}
  </dialog>;
}

type Pointer = { x: number; y: number; originX: number; originY: number; pressed: boolean; button: number; touch: boolean };
function Screen({ hand, onBack }: { hand: RemoteHand; onBack(): void }) {
  const view = useRef<HTMLDivElement>(null);
  const picture = useRef<HTMLDivElement>(null);
  const virtualCursor = useRef<SVGSVGElement>(null);
  const pointerPosition = useRef({ x: 0.5, y: 0.5 });
  const ownedLock = useRef(false);
  const ownedFullscreen = useRef(false);
  const captureAttempt = useRef(0);
  const [pointerLocked, setPointerLocked] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [captureNotice, setCaptureNotice] = useState("");
  const video = useRef<HTMLVideoElement>(null);
  const frameCanvas = useRef<HTMLCanvasElement>(null);
  const keyboardInput = useRef<HTMLTextAreaElement>(null);
  const session = useRef<RemoteBrowserSession | undefined>(undefined);
  const pointers = useRef(new Map<number, Pointer>());
  const keys = useRef(new Set<number>());
  const lastEscape = useRef(0);
  const [state, setState] = useState<RemoteState>({ status: "Connecting…", connected: false, controlling: false, connecting: true });
  const [text, setText] = useState("");
  const streamEndpoint = useRef<HTMLInputElement>(null);
  const [streamPreset, setStreamPreset] = useState<BroadcastPreset>("source");
  const [streamOpen, setStreamOpen] = useState(false);
  const activeHand = session.current?.hand ?? hand;
  useEffect(() => {
    let mounted = true;
    const discardInput = () => {
      pointers.current.clear(); keys.current.clear(); lastEscape.current = 0; setText("");
      if (keyboardInput.current) keyboardInput.current.value = "";
    };
    const connection = new RemoteBrowserSession(hand, video.current!, next => {
      if (!mounted) return;
      if (!next.connected || !next.controlling) discardInput();
      setState(next);
    }, frameCanvas.current!);
    session.current = connection;
    const release = () => { discardInput(); connection.releaseControl(); };
    const pause = () => { discardInput(); connection.suspend(); };
    const resume = () => { if (!document.hidden) connection.resume(); };
    const visibility = () => {
      // A hidden tab can keep viewing/broadcasting; only pagehide suspends it.
      if (document.hidden) release();
      else resume();
    };
    if (document.hidden) pause(); else void connection.connect();
    window.addEventListener("blur", release); window.addEventListener("pagehide", pause); window.addEventListener("pageshow", resume);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      mounted = false;
      ++captureAttempt.current;
      if (picture.current && document.pointerLockElement === picture.current) document.exitPointerLock();
      if (view.current && document.fullscreenElement === view.current) void document.exitFullscreen().catch(() => {});
      window.removeEventListener("blur", release); window.removeEventListener("pagehide", pause); window.removeEventListener("pageshow", resume);
      document.removeEventListener("visibilitychange", visibility); connection.close(); session.current = undefined;
    };
  }, [hand]);

  useEffect(() => {
    const lockChanged = () => {
      const locked = document.pointerLockElement === picture.current;
      const released = ownedLock.current && !locked;
      ownedLock.current = locked; setPointerLocked(locked);
      if (locked) { setCaptureNotice(""); keyboardInput.current?.focus({ preventScroll: true }); }
      if (released) { releaseInput(); session.current?.releaseControl(); }
    };
    const fullscreenChanged = () => {
      const active = document.fullscreenElement === view.current;
      if (ownedFullscreen.current && !active) releaseControl();
      ownedFullscreen.current = active; setFullscreen(active);
    };
    const lockFailed = () => setCaptureNotice("Mouse capture was unavailable. You can still control inside the picture.");
    document.addEventListener("pointerlockchange", lockChanged);
    document.addEventListener("pointerlockerror", lockFailed);
    document.addEventListener("fullscreenchange", fullscreenChanged);
    return () => {
      document.removeEventListener("pointerlockchange", lockChanged);
      document.removeEventListener("pointerlockerror", lockFailed);
      document.removeEventListener("fullscreenchange", fullscreenChanged);
    };
  }, []);

  useEffect(() => {
    if (!state.controlling && !state.controlPending) {
      ++captureAttempt.current;
      if (picture.current && document.pointerLockElement === picture.current) document.exitPointerLock();
    } else if (state.controlling && pointerLocked) {
      // Relative zero enables the publisher's captured cursor without warping it.
      session.current?.input(state.relativePointer ? { kind: "relativeMove", deltaX: 0, deltaY: 0 }
        : { kind: "move", ...pointerPosition.current });
      keyboardInput.current?.focus({ preventScroll: true });
      positionVirtualCursor();
    }
  }, [state.controlling, state.controlPending, state.relativePointer, pointerLocked, fullscreen]);

  function releaseControl() {
    ++captureAttempt.current;
    releaseInput(); session.current?.releaseControl();
    if (picture.current && document.pointerLockElement === picture.current) document.exitPointerLock();
  }
  function enterFullscreen() {
    if (!view.current) return;
    if (view.current.requestFullscreen) void view.current.requestFullscreen().catch(() => setExpanded(true));
    else setExpanded(true);
  }
  function toggleFullscreen() {
    if (fullscreen) { releaseControl(); void document.exitFullscreen().catch(() => {}); }
    else if (expanded) { releaseControl(); setExpanded(false); }
    else enterFullscreen();
  }
  function takeControl() {
    if (state.controlling || state.controlPending) { releaseControl(); return; }
    setCaptureNotice(""); session.current?.takeControl();
    const attempt = ++captureAttempt.current;
    // Request pointer lock while this click has user activation.
    // Touch screens retain their existing gestures and never hide a pointer.
    if (window.matchMedia("(any-pointer: fine)").matches && picture.current?.requestPointerLock) {
      try {
        const request = picture.current.requestPointerLock();
        void Promise.resolve(request).then(() => {
          if (attempt !== captureAttempt.current && document.pointerLockElement === picture.current) document.exitPointerLock();
        }).catch(() => {
          if (attempt === captureAttempt.current) setCaptureNotice("Mouse capture was unavailable. You can still control inside the picture.");
        });
      } catch { setCaptureNotice("Mouse capture was unavailable. You can still control inside the picture."); }
    }
  }
  function positionVirtualCursor() {
    const bounds = picture.current?.getBoundingClientRect();
    if (!bounds || !virtualCursor.current) return;
    const width = video.current?.videoWidth || frameCanvas.current?.width || activeHand.width;
    const height = video.current?.videoHeight || frameCanvas.current?.height || activeHand.height;
    const scale = Math.min(bounds.width / width, bounds.height / height);
    virtualCursor.current.style.transform = `translate(${(bounds.width - width * scale) / 2 + pointerPosition.current.x * width * scale}px, ${(bounds.height - height * scale) / 2 + pointerPosition.current.y * height * scale}px)`;
  }
  function lockedMouseMove(event: MouseEvent<HTMLDivElement>) {
    if (!state.controlling || document.pointerLockElement !== picture.current) return;
    const deltaX = Math.max(-4096, Math.min(4096, event.movementX));
    const deltaY = Math.max(-4096, Math.min(4096, event.movementY));
    if (!deltaX && !deltaY) return;
    if (state.relativePointer) session.current?.input({ kind: "relativeMove", deltaX, deltaY });
    else {
      const bounds = picture.current!.getBoundingClientRect();
      const width = video.current?.videoWidth || activeHand.width, height = video.current?.videoHeight || activeHand.height;
      const scale = Math.min(bounds.width / width, bounds.height / height);
      pointerPosition.current = { x: Math.max(0, Math.min(1, pointerPosition.current.x + deltaX / (width * scale))),
        y: Math.max(0, Math.min(1, pointerPosition.current.y + deltaY / (height * scale))) };
      positionVirtualCursor(); session.current?.input({ kind: "move", ...pointerPosition.current });
    }
  }
  function lockedMouseButton(event: MouseEvent<HTMLDivElement>, down: boolean) {
    if (!state.controlling || document.pointerLockElement !== picture.current) return;
    event.preventDefault();
    const button = event.button === 2 ? 1 : event.button === 1 ? 2 : 0;
    session.current?.input({ kind: "button", button, down, ...(state.relativePointer ? {} : pointerPosition.current) });
  }

  function point(clientX: number, clientY: number, clamp = false) {
    const frames = activeHand.transport === "frames-v1";
    const element = frames ? frameCanvas.current : video.current; if (!element) return;
    const bounds = element.getBoundingClientRect();
    const width = (frames ? frameCanvas.current?.width : video.current?.videoWidth) || activeHand.width;
    const height = (frames ? frameCanvas.current?.height : video.current?.videoHeight) || activeHand.height;
    const scale = Math.min(bounds.width / width, bounds.height / height);
    const left = bounds.left + (bounds.width - width * scale) / 2, top = bounds.top + (bounds.height - height * scale) / 2;
    const x = (clientX - left) / (width * scale), y = (clientY - top) / (height * scale);
    if (!Number.isFinite(x) || !Number.isFinite(y) || (!clamp && (x < 0 || x > 1 || y < 0 || y > 1))) return;
    return { x: Math.min(1, Math.max(0, x)), y: Math.min(1, Math.max(0, y)) };
  }
  function pointerDown(event: PointerEvent<HTMLDivElement>) {
    if (document.pointerLockElement === picture.current) return;
    if (!state.controlling) { void video.current?.play().catch(() => {}); return; }
    const position = point(event.clientX, event.clientY); if (!position) return;
    event.preventDefault(); keyboardInput.current?.focus({ preventScroll: true }); event.currentTarget.setPointerCapture(event.pointerId);
    const button = event.button === 2 ? 1 : event.button === 1 ? 2 : 0;
    const touch = event.pointerType === "touch";
    pointers.current.set(event.pointerId, { ...position, originX: position.x, originY: position.y, pressed: !touch, button, touch });
    if (pointers.current.size > 1) {
      session.current?.input({ kind: "releaseAll" });
      for (const pointer of pointers.current.values()) pointer.pressed = false;
    } else if (!touch) session.current?.input({ kind: "button", ...position, button, down: true });
  }
  function pointerMove(event: PointerEvent<HTMLDivElement>) {
    if (document.pointerLockElement === picture.current) return;
    if (!state.controlling) return;
    const pointer = pointers.current.get(event.pointerId), position = point(event.clientX, event.clientY, Boolean(pointer));
    if (!position) return;
    pointerPosition.current = position;
    if (pointers.current.size > 1 && pointer) {
      session.current?.input({ kind: "scroll", ...position,
        deltaX: Math.max(-4096, Math.min(4096, (position.x - pointer.x) * activeHand.width)),
        deltaY: Math.max(-4096, Math.min(4096, (position.y - pointer.y) * activeHand.height)) });
    } else {
      if (pointer?.touch && !pointer.pressed) {
        if (Math.hypot(position.x - pointer.originX, position.y - pointer.originY) < 0.008) return;
        pointer.pressed = true;
        session.current?.input({ kind: "button", x: pointer.originX, y: pointer.originY, button: 0, down: true });
      }
      session.current?.input({ kind: "move", ...position });
    }
    if (pointer) { pointer.x = position.x; pointer.y = position.y; }
  }
  function pointerUp(event: PointerEvent<HTMLDivElement>) {
    if (document.pointerLockElement === picture.current) return;
    const pointer = pointers.current.get(event.pointerId); if (!pointer) return;
    const position = point(event.clientX, event.clientY, true)!;
    if (pointers.current.size === 1) {
      if (pointer.touch && !pointer.pressed) session.current?.input({ kind: "button", ...position, button: 0, down: true });
      session.current?.input({ kind: "button", ...position, button: pointer.button, down: false });
    }
    pointers.current.delete(event.pointerId);
    // A two-finger gesture must not become a click when the remaining finger lifts.
    for (const remaining of pointers.current.values()) remaining.pressed = true;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
  }
  function releaseInput() { pointers.current.clear(); keys.current.clear(); lastEscape.current = 0; session.current?.input({ kind: "releaseAll" }); }
  function keyboard(event: KeyboardEvent<HTMLDivElement>, down: boolean) {
    if (!state.controlling || event.nativeEvent.isComposing) return;
    if (down && event.code === "Escape" && document.pointerLockElement === picture.current) {
      event.preventDefault(); event.stopPropagation(); releaseControl(); return;
    }
    if (down && event.code === "Escape" && !event.repeat) {
      const now = performance.now(), previous = lastEscape.current; lastEscape.current = now;
      if ((previous > 0 && now - previous <= 500) || (event.metaKey && event.shiftKey)) {
        event.preventDefault(); event.stopPropagation(); releaseInput(); session.current?.releaseControl(); return;
      }
    } else if (down && event.code !== "Escape") lastEscape.current = 0;
    const key = remoteKeys[event.code]; if (key === undefined) return;
    event.preventDefault(); event.stopPropagation();
    if (down && activeHand.kind === "phone" && event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) session.current?.input({ kind: "text", text: event.key });
    else if (down) { keys.current.add(key); session.current?.input({ kind: "key", key, down: true }); }
    else if (keys.current.delete(key)) session.current?.input({ kind: "key", key, down: false });
  }
  return <div ref={view} className={`remote-screen-view${expanded ? " remote-screen-expanded" : ""}`} data-pointer-locked={pointerLocked}>
    <div className="remote-screen-toolbar"><button type="button" onClick={onBack}>All screens</button><span role="status">{state.status}</span>
      {!state.connected && <button type="button" disabled={state.connecting} onClick={() => session.current?.reconnect()}>Reconnect</button>}
      {state.audioAvailable && <button type="button" aria-pressed={Boolean(state.audioEnabled)}
        onClick={() => { void session.current?.setAudioEnabled(!state.audioEnabled); }}>
        {state.audioEnabled ? "Mute sound" : "Enable sound"}</button>}
      {activeHand.broadcast && <button type="button" aria-expanded={streamOpen} onClick={() => setStreamOpen(!streamOpen)}>Stream RTMP</button>}
      <button type="button" onClick={toggleFullscreen}>{fullscreen || expanded ? "Exit fullscreen" : "Fullscreen"}</button>
      <button type="button" disabled={!state.connected || !activeHand.controllable} onClick={takeControl}>
        {state.controlling ? "Release control" : state.controlPending ? "Cancel control" : "Take control"}</button></div>
    {streamOpen && activeHand.broadcast && <form className="remote-screen-broadcast" onSubmit={event => {
      event.preventDefault();
      const input = streamEndpoint.current;
      if (input) { const endpoint = input.value; input.value = ""; session.current?.broadcast("start", endpoint, streamPreset); }
    }}>
      <label>RTMP(S) endpoint<input ref={streamEndpoint} type="password" required maxLength={4096} autoComplete="off" spellCheck={false} data-1p-ignore
        placeholder="rtmps://server/app/stream-key" aria-label="RTMP stream endpoint" /></label>
      <label>Quality<select value={streamPreset} onChange={event => setStreamPreset(event.target.value as BroadcastPreset)}>
        <option value="twitch">Twitch · up to 1080p60 · 6 Mbps</option><option value="x">X · up to 1080p30 · 9 Mbps</option>
        <option value="source">Source quality</option><option value="1080p">1080p</option><option value="720p">720p</option>
      </select></label>
      <button type="submit" disabled={!canStartBroadcast(state)}>Start stream</button>
      <button type="button" disabled={!state.connected || state.broadcastPending} onClick={() => session.current?.broadcast("stop")}>Stop stream</button>
      <button type="button" disabled={!state.connected || state.broadcastPending} onClick={() => session.current?.broadcast("status")}>Check status</button>
      <span role="status">{state.broadcastPending ? "Updating stream…" : `Stream: ${state.broadcastStatus ?? "checking…"}`}</span>
      {state.broadcastAudio !== undefined && <span>{state.broadcastAudio ? "Stream audio available" : "Stream audio unavailable"}</span>}
      {state.broadcastError && <span role="alert">{state.broadcastError}</span>}
      <small>Closing this preview keeps the stream running. Use Stop stream to end it.</small>
    </form>}
    {captureNotice && <p className="remote-screen-notice" role="status">{captureNotice}</p>}
    <div ref={picture} className="remote-screen-canvas" tabIndex={0} role="application" aria-label="Remote screen" data-testid="remote-screen"
      onFocus={event => { if (event.target === event.currentTarget && state.controlling) keyboardInput.current?.focus({ preventScroll: true }); }}
      onPointerDown={pointerDown} onPointerMove={pointerMove} onPointerUp={pointerUp}
      onMouseMove={lockedMouseMove} onMouseDown={event => lockedMouseButton(event, true)} onMouseUp={event => lockedMouseButton(event, false)}
      onPointerCancel={releaseInput} onLostPointerCapture={event => { if (pointers.current.has(event.pointerId)) releaseInput(); }}
      onKeyDown={event => keyboard(event, true)} onKeyUp={event => keyboard(event, false)}
      onBlur={event => { if (!event.currentTarget.contains(event.relatedTarget)) releaseInput(); }}
      onContextMenu={event => event.preventDefault()} onWheel={event => {
        if (!state.controlling) return;
        const position = pointerLocked ? (state.relativePointer ? {} : pointerPosition.current) : point(event.clientX, event.clientY); if (!position) return;
        const scale = event.deltaMode === 1 ? 20 : event.deltaMode === 2 ? activeHand.height : 1;
        session.current?.input({ kind: "scroll", ...position, deltaX: Math.min(4096, Math.max(-4096, -event.deltaX * scale)), deltaY: Math.min(4096, Math.max(-4096, -event.deltaY * scale)) });
      }}><video ref={video} autoPlay playsInline muted={!state.audioEnabled} data-testid="remote-video" style={{ visibility: state.connected && activeHand.transport !== "frames-v1" ? "visible" : "hidden" }} />
      <canvas ref={frameCanvas} className="remote-screen-frame" data-testid="remote-frame" aria-label="Remote desktop picture"
        style={{ visibility: state.connected && activeHand.transport === "frames-v1" ? "visible" : "hidden" }} />
      {pointerLocked && !state.relativePointer && <svg ref={virtualCursor} className="remote-virtual-cursor" width="16" height="22" viewBox="0 0 16 22" aria-hidden="true"><path d="M1 1v17l4-4 3 7 3-1-3-7h6Z" fill="white" stroke="black" /></svg>}
      {pointerLocked && <span className="remote-capture-hint">Esc releases mouse and keyboard</span>}
      <textarea ref={keyboardInput} className="remote-keyboard-input" aria-label="Remote keyboard" tabIndex={-1}
        autoComplete="off" autoCapitalize="off" spellCheck={false} inputMode="none" data-1p-ignore
        onCompositionEnd={event => {
          if (event.data && new TextEncoder().encode(event.data).length <= 4096) session.current?.input({ kind: "text", text: event.data });
          event.currentTarget.value = "";
        }}
        onPaste={event => {
          event.preventDefault(); const value = event.clipboardData.getData("text/plain");
          if (value && new TextEncoder().encode(value).length <= 4096) session.current?.input({ kind: "text", text: value });
        }} />
    </div>
    {state.controlling && <form className="remote-screen-text" onSubmit={event => { event.preventDefault(); if (text && new TextEncoder().encode(text).length <= 4096) { session.current?.input({ kind: "text", text }); setText(""); } }}>
      <input aria-label="Type on remote screen" placeholder="Type on remote screen" value={text} onChange={event => setText(event.target.value)} />
      <button type="submit" disabled={!text || new TextEncoder().encode(text).length > 4096}>Send</button>
      <button type="button" onClick={() => { for (const down of [true, false]) session.current?.input({ kind: "key", key: 40, down }); }}>Return</button>
      {activeHand.kind === "phone" && <button type="button" onClick={() => session.current?.input({ kind: "key", key: 74, down: true })}>Home</button>}
      <small>{pointerLocked ? "Esc releases mouse and keyboard" : "Esc twice releases control"}</small>
    </form>}
  </div>;
}
