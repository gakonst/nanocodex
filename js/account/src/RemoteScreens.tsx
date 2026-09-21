import { useQuery } from "@tanstack/react-query";
import { useAccountSession } from "./AccountSession";
import { accountQueryKey } from "./queryClient";
import { useEffect, useRef, useState, type PointerEvent, type KeyboardEvent, type MouseEvent } from "react";
import { createPortal } from "react-dom";
import { Monitor, X } from "lucide-react";
import { canStartBroadcast, listRemoteHands, RemoteBrowserSession, remoteKeys, type RemoteHand, type BroadcastPreset, type RemoteState, type RemoteInput } from "./handRemote";
import { RemoteMotionBuffer, RemoteMouseButtons } from "./handRemoteInput";
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
export function Screen({ hand, onBack }: { hand: RemoteHand; onBack(): void }) {
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
  const mouse = useRef(new RemoteMouseButtons());
  const mousePointer = useRef<number | undefined>(undefined);
  const motion = useRef<RemoteMotionBuffer | null>(null);
  motion.current ??= new RemoteMotionBuffer(event => session.current?.input(event));
  function sendInput(event: RemoteInput) { motion.current!.input(event); }
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
      motion.current?.clear(); mouse.current.reset(); mousePointer.current = undefined;
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
      mounted = false; motion.current?.clear(); mouse.current.reset(); mousePointer.current = undefined;
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
      // Some browsers return void from requestPointerLock. A late completion
      // after release must not capture the pointer for an inactive lease.
      if (locked && !session.current?.state.controlling) { document.exitPointerLock(); return; }
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
      sendInput(state.relativePointer ? { kind: "relativeMove", deltaX: 0, deltaY: 0 }
        : { kind: "move", ...pointerPosition.current });
      keyboardInput.current?.focus({ preventScroll: true });
      positionVirtualCursor();
    }
  }, [state.controlling, state.controlPending, state.relativePointer, pointerLocked, fullscreen]);

  useEffect(() => {
    const canvas = picture.current;
    if (!canvas) return;
    // React's delegated wheel listener is passive. Consume scroll only over an
    // active remote picture so it cannot also scroll the surrounding web app.
    const wheel = (event: WheelEvent) => {
      if (!state.controlling) return;
      const locked = document.pointerLockElement === canvas;
      const position = locked ? (state.relativePointer ? {} : pointerPosition.current) : point(event.clientX, event.clientY);
      if (!position) return;
      event.preventDefault();
      const scale = event.deltaMode === 1 ? 20 : event.deltaMode === 2 ? activeHand.height : 1;
      sendInput({ kind: "scroll", ...position, deltaX: Math.min(4096, Math.max(-4096, -event.deltaX * scale)), deltaY: Math.min(4096, Math.max(-4096, -event.deltaY * scale)) });
    };
    canvas.addEventListener("wheel", wheel, { passive: false });
    return () => canvas.removeEventListener("wheel", wheel);
  }, [state.controlling, state.relativePointer, activeHand.height, activeHand.width, activeHand.transport]);

  useEffect(() => {
    // Chromium may lose pointer capture after the first up in a chord. Retain
    // only this explicitly started mouse gesture until all buttons are up.
    // Canvas events still use React; this listener handles the outside portion.
    const outside = (event: globalThis.PointerEvent) => {
      const canvas = picture.current;
      if (!canvas || !session.current?.state.controlling || !mouse.current.held
        || event.pointerId !== mousePointer.current || document.pointerLockElement === canvas
        || canvas.contains(event.target as Node | null)) return;
      if (event.type === "pointercancel") { releaseInput(); return; }
      const position = point(event.clientX, event.clientY, true);
      mouse.current.update(event.buttons, position, sendInput);
      if (position && event.type === "pointermove") sendInput({ kind: "move", ...position });
      if (!mouse.current.held) mousePointer.current = undefined;
    };
    document.addEventListener("pointermove", outside, true);
    document.addEventListener("pointerup", outside, true);
    document.addEventListener("pointercancel", outside, true);
    return () => {
      document.removeEventListener("pointermove", outside, true);
      document.removeEventListener("pointerup", outside, true);
      document.removeEventListener("pointercancel", outside, true);
    };
  }, [activeHand.width, activeHand.height, activeHand.transport]);

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
  }
  function lockMouse() {
    if (!state.controlling) return;
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
    if (mouse.current.held) mouse.current.update(event.buttons, state.relativePointer ? undefined : pointerPosition.current, sendInput);
    const deltaX = Math.max(-4096, Math.min(4096, event.movementX));
    const deltaY = Math.max(-4096, Math.min(4096, event.movementY));
    if (!deltaX && !deltaY) return;
    if (state.relativePointer) sendInput({ kind: "relativeMove", deltaX, deltaY });
    else {
      const bounds = picture.current!.getBoundingClientRect();
      const width = video.current?.videoWidth || activeHand.width, height = video.current?.videoHeight || activeHand.height;
      const scale = Math.min(bounds.width / width, bounds.height / height);
      pointerPosition.current = { x: Math.max(0, Math.min(1, pointerPosition.current.x + deltaX / (width * scale))),
        y: Math.max(0, Math.min(1, pointerPosition.current.y + deltaY / (height * scale))) };
      positionVirtualCursor(); sendInput({ kind: "move", ...pointerPosition.current });
    }
  }
  function lockedMouseButton(event: MouseEvent<HTMLDivElement>) {
    if (!state.controlling || document.pointerLockElement !== picture.current) return;
    event.preventDefault();
    mouse.current.update(event.buttons, state.relativePointer ? undefined : pointerPosition.current, sendInput);
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
    if (event.pointerType !== "touch") {
      mousePointer.current = event.pointerId;
      mouse.current.update(event.buttons, position, sendInput); return;
    }
    const button = 0;
    const touch = event.pointerType === "touch";
    pointers.current.set(event.pointerId, { ...position, originX: position.x, originY: position.y, pressed: !touch, button, touch });
    if (pointers.current.size > 1) {
      sendInput({ kind: "releaseAll" });
      for (const pointer of pointers.current.values()) pointer.pressed = false;
    } else if (!touch) sendInput({ kind: "button", ...position, button, down: true });
  }
  function pointerMove(event: PointerEvent<HTMLDivElement>) {
    if (document.pointerLockElement === picture.current) return;
    if (!state.controlling) return;
    const pointer = pointers.current.get(event.pointerId), position = point(event.clientX, event.clientY, Boolean(pointer) || mouse.current.held);
    if (!position) return;
    pointerPosition.current = position;
    if (event.pointerType !== "touch") {
      if (mouse.current.held) mouse.current.update(event.buttons, position, sendInput);
      sendInput({ kind: "move", ...position }); return;
    }
    if (pointers.current.size > 1 && pointer) {
      sendInput({ kind: "scroll", ...position,
        deltaX: Math.max(-4096, Math.min(4096, (position.x - pointer.x) * activeHand.width)),
        deltaY: Math.max(-4096, Math.min(4096, (position.y - pointer.y) * activeHand.height)) });
    } else {
      if (pointer?.touch && !pointer.pressed) {
        if (Math.hypot(position.x - pointer.originX, position.y - pointer.originY) < 0.008) return;
        pointer.pressed = true;
        sendInput({ kind: "button", x: pointer.originX, y: pointer.originY, button: 0, down: true });
      }
      sendInput({ kind: "move", ...position });
    }
    if (pointer) { pointer.x = position.x; pointer.y = position.y; }
  }
  function pointerUp(event: PointerEvent<HTMLDivElement>) {
    if (document.pointerLockElement === picture.current) return;
    if (event.pointerType !== "touch") {
      mouse.current.update(event.buttons, point(event.clientX, event.clientY, true), sendInput);
      if (!mouse.current.held) mousePointer.current = undefined;
      if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
      return;
    }
    const pointer = pointers.current.get(event.pointerId); if (!pointer) return;
    const position = point(event.clientX, event.clientY, true) ?? { x: pointer.x, y: pointer.y };
    if (pointers.current.size === 1) {
      if (pointer.touch && !pointer.pressed) sendInput({ kind: "button", ...position, button: 0, down: true });
      sendInput({ kind: "button", ...position, button: pointer.button, down: false });
    }
    pointers.current.delete(event.pointerId);
    // A two-finger gesture must not become a click when the remaining finger lifts.
    for (const remaining of pointers.current.values()) remaining.pressed = true;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
  }
  function releaseInput() { mouse.current.reset(); mousePointer.current = undefined; pointers.current.clear(); keys.current.clear(); lastEscape.current = 0; sendInput({ kind: "releaseAll" }); }
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
    if (down && activeHand.kind === "phone" && event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) sendInput({ kind: "text", text: event.key });
    else if (down) { keys.current.add(key); sendInput({ kind: "key", key, down: true }); }
    else if (keys.current.delete(key)) sendInput({ kind: "key", key, down: false });
  }
  return <div ref={view} className={`remote-screen-view${expanded ? " remote-screen-expanded" : ""}`} data-pointer-locked={pointerLocked}
    onKeyDownCapture={event => {
      if (event.repeat) return;
      if (event.code === "KeyF" && event.ctrlKey && event.metaKey) {
        event.preventDefault(); event.stopPropagation(); releaseInput(); toggleFullscreen();
      } else if (event.code === "Escape" && ((event.metaKey && event.shiftKey) || pointerLocked)) {
        event.preventDefault(); event.stopPropagation(); releaseControl();
      }
    }}>
    <div className="remote-screen-toolbar"><button type="button" onClick={onBack}>All screens</button><span role="status">{state.status}</span>
      {!state.connected && <button type="button" disabled={state.connecting} onClick={() => session.current?.reconnect()}>Reconnect</button>}
      {state.audioAvailable && <button type="button" aria-pressed={Boolean(state.audioEnabled)}
        onClick={() => { void session.current?.setAudioEnabled(!state.audioEnabled); }}>
        {state.audioEnabled ? "Mute sound" : "Enable sound"}</button>}
      {state.microphoneAvailable && <button type="button" disabled={!state.controlling}
        aria-pressed={Boolean(state.microphoneEnabled)} onClick={() => { void session.current?.setMicrophoneEnabled(!state.microphoneEnabled && !state.microphonePending); }}>
        {state.microphonePending ? "Cancel microphone" : state.microphoneEnabled ? "Mute microphone" : "Enable microphone"}</button>}
      {activeHand.broadcast && <button type="button" aria-expanded={streamOpen} onClick={() => setStreamOpen(!streamOpen)}>Stream RTMP</button>}
      {state.controlling && !pointerLocked && <button type="button" onClick={lockMouse}>Lock mouse</button>}
      <button type="button" title="Control–Command–F" onClick={toggleFullscreen}>{fullscreen || expanded ? "Exit fullscreen" : "Fullscreen"}</button>
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
    {state.microphoneError && <p className="remote-screen-notice" role="alert">{state.microphoneError}</p>}
    {captureNotice && <p className="remote-screen-notice" role="status">{captureNotice}</p>}
    <div ref={picture} className="remote-screen-canvas" tabIndex={0} role="application" aria-label="Remote screen" data-testid="remote-screen"
      onFocus={event => { if (event.target === event.currentTarget && state.controlling) keyboardInput.current?.focus({ preventScroll: true }); }}
      onPointerDown={pointerDown} onPointerMove={pointerMove} onPointerUp={pointerUp}
      onMouseMove={lockedMouseMove} onMouseDown={event => lockedMouseButton(event)} onMouseUp={event => lockedMouseButton(event)}
      onPointerCancel={releaseInput} onLostPointerCapture={event => { if (pointers.current.has(event.pointerId) || (event.pointerType !== "touch" && mouse.current.held && event.buttons === 0)) releaseInput(); }}
      onKeyDown={event => keyboard(event, true)} onKeyUp={event => keyboard(event, false)}
      onBlur={event => { if (!event.currentTarget.contains(event.relatedTarget)) releaseInput(); }}
      onContextMenu={event => event.preventDefault()} onAuxClick={event => event.preventDefault()}><video ref={video} autoPlay playsInline muted={!state.audioEnabled} data-testid="remote-video" style={{ visibility: state.connected && activeHand.transport !== "frames-v1" ? "visible" : "hidden" }} />
      <canvas ref={frameCanvas} className="remote-screen-frame" data-testid="remote-frame" aria-label="Remote desktop picture"
        style={{ visibility: state.connected && activeHand.transport === "frames-v1" ? "visible" : "hidden" }} />
      {pointerLocked && !state.relativePointer && <svg ref={virtualCursor} className="remote-virtual-cursor" width="16" height="22" viewBox="0 0 16 22" aria-hidden="true"><path d="M1 1v17l4-4 3 7 3-1-3-7h6Z" fill="white" stroke="black" /></svg>}
      {pointerLocked && <span className="remote-capture-hint">Esc releases mouse and keyboard</span>}
      <textarea ref={keyboardInput} className="remote-keyboard-input" aria-label="Remote keyboard" tabIndex={-1}
        autoComplete="off" autoCapitalize="off" spellCheck={false} inputMode="none" data-1p-ignore
        onCompositionEnd={event => {
          if (event.data && new TextEncoder().encode(event.data).length <= 4096) sendInput({ kind: "text", text: event.data });
          event.currentTarget.value = "";
        }}
        onPaste={event => {
          event.preventDefault(); const value = event.clipboardData.getData("text/plain");
          if (value && new TextEncoder().encode(value).length <= 4096) sendInput({ kind: "text", text: value });
        }} />
    </div>
    {state.controlling && <form className="remote-screen-text" onSubmit={event => { event.preventDefault(); if (text && new TextEncoder().encode(text).length <= 4096) { sendInput({ kind: "text", text }); setText(""); } }}>
      <input aria-label="Type on remote screen" placeholder="Type on remote screen" value={text} onChange={event => setText(event.target.value)} />
      <button type="submit" disabled={!text || new TextEncoder().encode(text).length > 4096}>Send</button>
      <button type="button" onClick={() => { for (const down of [true, false]) sendInput({ kind: "key", key: 40, down }); }}>Return</button>
      {activeHand.kind === "phone" && <button type="button" onClick={() => sendInput({ kind: "key", key: 74, down: true })}>Home</button>}
      <small>{pointerLocked ? "Esc releases mouse and keyboard" : "Esc twice releases control"}</small>
    </form>}
  </div>;
}
