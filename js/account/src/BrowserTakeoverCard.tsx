import { useEffect, useRef, useState, type HTMLAttributes } from 'react';
import { createPortal } from 'react-dom';
import { browserTakeover, type BrowserTakeoverAction, type BrowserTakeoverFrame, type BrowserKeyboard, type VaultIntake } from './vaultIntake';
import { enqueueTakeover, imagePoint, textEdits } from './browserTakeoverInput';
import './browserTakeover.css';

export function BrowserTakeoverCard({ intake, authenticated, onReceipt }: { intake: VaultIntake; authenticated: boolean; onReceipt(receipt: string): void }) {
  const login = intake.operation === "browser_login";
  const [reviewed, setReviewed] = useState(!login);
  const [frame, setFrame] = useState<BrowserTakeoverFrame>();
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(authenticated);
  const [hint, setHint] = useState<BrowserKeyboard>({ type: login ? 'password' : 'text', multiline: false });
  const [dot, setDot] = useState<{ x: number; y: number }>();
  const input = useRef<HTMLTextAreaElement | HTMLInputElement | null>(null), textInput = useRef<HTMLTextAreaElement>(null), passwordInput = useRef<HTMLInputElement>(null), screen = useRef<HTMLDivElement>(null), dialog = useRef<HTMLDialogElement>(null);
  const state = useRef({ alive: false, blocked: false, running: false, finishing: false, finished: false, epoch: 0, queue: [] as BrowserTakeoverAction[], controller: undefined as AbortController | undefined, text: '', composing: false, pointer: undefined as number | undefined, region: undefined as BrowserKeyboard | undefined, lastInput: 0, recovering: false });
  // Capture before the native keyboard changes the visual viewport; never resize on keyboard events.
  const viewport = useRef({ width: Math.max(240, Math.min(1920, Math.round(window.innerWidth))), height: Math.max(240, Math.min(1920, Math.round(window.innerHeight - 56))), mobile: matchMedia('(pointer: coarse)').matches });
  const receipt = useRef(onReceipt); receipt.current = onReceipt;
  const clear = () => {
    const s = state.current; s.epoch++; s.controller?.abort(); s.controller = undefined; s.queue.length = 0; s.running = false; s.blocked = true; s.finishing = false; s.text = ''; s.composing = false; s.pointer = undefined;
    if (input.current) { input.current.value = ''; input.current.blur(); }
    setFrame(undefined); setDot(undefined); setBusy(false);
  };
  const pump = async () => {
    const s = state.current; if (s.running || !s.alive || s.blocked || s.finished) return;
    s.running = true; setBusy(true); const epoch = s.epoch; let retry = false;
    try {
      while (s.queue.length && s.alive && epoch === s.epoch) {
        const action = s.queue.shift()!; s.controller = new AbortController();
        const next = await browserTakeover(intake, action, fetch, s.controller.signal);
        if (!s.alive || epoch !== s.epoch) return;
        if (next.status === 'approved') { setReviewed(true); enqueueTakeover(s.queue, { action: 'observe', viewport: viewport.current, image_format: 'jpeg' }); continue; }
        if (next.status === 'finished' && action.action !== 'finish') throw new Error('Unexpected completion');
        setFrame(next); if (s.recovering) { s.recovering = false; setNotice(''); }
        if (next.status === 'active' && next.keyboard) setHint(next.keyboard);
        if ((next.status === 'finished' || next.status === 'cancelled') && !s.finished) {
          s.finished = true; s.queue.length = 0; s.text = ''; if (input.current) input.current.value = ''; setOpen(false);
          receipt.current(JSON.stringify(login ? { type: 'browser_login_receipt', status: next.status, request_id: intake.request_id } : { type: 'browser_vault_takeover_receipt', status: 'finished', challenge_id: intake.challenge_id }));
        }
      }
    } catch {
      if (!s.alive || epoch !== s.epoch) return;
      // One automatic recovery frame. Observation never replays input; queued
      // input after an unconfirmed action is dropped rather than applied blind.
      if (!s.recovering && !s.finishing) { s.recovering = true; s.queue.length = 0; s.pointer = undefined; setDot(undefined); setNotice('Reconnecting…'); enqueueTakeover(s.queue, { action: 'observe', image_format: 'jpeg' }); retry = true; }
      else { s.recovering = false; setNotice(''); clear(); setError('Action could not be confirmed. Refresh to continue.'); }
    } finally { if (s.alive && epoch === s.epoch) { s.running = false; setBusy(false); if (retry) void pump(); } }
  };
  const send = (action: BrowserTakeoverAction) => {
    const s = state.current; if (!authenticated || s.finished || s.finishing || (s.blocked && !['finish', 'cancel', 'approve'].includes(action.action))) return;
    if (action.action === 'finish' || action.action === 'cancel') { s.blocked = false; s.finishing = true; setError(''); }
    if (action.action === 'approve') s.blocked = false;
    if (action.action !== 'observe') s.lastInput = Date.now();
    enqueueTakeover(s.queue, action); void pump();
  };
  const refresh = () => {
    if (!authenticated || state.current.finished || state.current.running) return;
    state.current.blocked = false; setError(''); send({ action: 'observe', viewport: viewport.current, image_format: 'jpeg' });
  };
  useEffect(() => {
    const s = state.current; s.alive = true; s.finished = false; s.finishing = false;
    setOpen(authenticated);
    setReviewed(!login);
    if (authenticated && !login) refresh(); else s.blocked = true;
    const hide = () => { if (document.visibilityState !== 'visible') { clear(); setError('Private view paused. Refresh to continue.'); } };
    const pagehide = () => { clear(); setError('Private view paused. Refresh to continue.'); };
    document.addEventListener('visibilitychange', hide); window.addEventListener('pagehide', pagehide);
    const interval = window.setInterval(() => { if (!s.blocked && !s.running && !s.queue.length && !s.finished && s.pointer === undefined && Date.now() - s.lastInput > 2000 && document.visibilityState === 'visible') send({ action: 'observe', image_format: 'jpeg' }); }, 1500);
    return () => { s.alive = false; clear(); document.removeEventListener('visibilitychange', hide); window.removeEventListener('pagehide', pagehide); clearInterval(interval); };
  }, [authenticated, intake.challenge_id]);
  useEffect(() => { if (open && authenticated && !dialog.current?.open) dialog.current?.showModal(); }, [open, authenticated]);
  const focusKeyboard = (keyboard = hint) => {
    if (state.current.blocked) return;
    setHint(keyboard);
    // Must happen inside the user's tap for iOS Safari; do not await a remote frame.
    input.current = keyboard.type === 'password' ? passwordInput.current : textInput.current;
    if (input.current) { input.current.inputMode = keyboard.type === 'password' ? 'text' : keyboard.type === 'number' ? 'numeric' : keyboard.type; input.current.focus({ preventScroll: true }); }
  };
  const commit = () => {
    const s = state.current; if (!input.current || s.composing) return;
    const next = input.current.value; for (const action of textEdits(s.text, next)) send(action);
    s.text = next;
    // Ephemeral composition buffer only; no private text retained after commit.
    s.text = ''; input.current.value = '';
  };
  const keyboardEvents: HTMLAttributes<HTMLTextAreaElement | HTMLInputElement> = {
    onCompositionStart: () => { state.current.composing = true; },
    onCompositionEnd: () => { state.current.composing = false; commit(); },
    onInput: commit,
    onBeforeInput: event => { const native = event.nativeEvent as InputEvent; if (native.inputType === 'deleteContentBackward' && !state.current.composing && !input.current?.value) { event.preventDefault(); send({ action: 'edit', delete_backward: 1, text: '' }); } },
    onKeyDown: event => { if (event.nativeEvent.isComposing || state.current.composing) return; if (['Enter', 'Tab', 'Escape', 'Backspace'].includes(event.key)) { if (event.key === 'Enter' && hint.multiline) return; event.preventDefault(); send({ action: 'key', key: event.key as 'Enter' | 'Tab' | 'Escape' | 'Backspace' }); } },
  };
  const point = (x: number, y: number) => imagePoint(screen.current!.getBoundingClientRect(), x, y);
  return <section className="vault-intake-card" aria-label="Private browser control">
    <strong>{frame?.status === 'cancelled' ? 'Private sign-in cancelled' : frame?.status === 'finished' ? (login ? 'Private sign-in finished; verification pending' : 'Browser control finished') : login ? 'Sign in privately on phone' : 'Private browser'}</strong>
    {!authenticated ? <p>Sign in to control this browser.</p> : !open && !state.current.finished ? <button onClick={() => { setOpen(true); }}>Open private browser</button> : null}
    {open && authenticated ? createPortal(<dialog ref={dialog} className="private-browser-dialog" aria-label="Private browser" onCancel={event => event.preventDefault()}>
      {login && !reviewed ? <section className="private-browser-review">
        <h2>Sign in privately on phone</h2><p>Starting site: <strong>{intake.origin}</strong></p>
        <p>Review the sites allowed for this login:</p><ul>{intake.allowed_origins?.map(origin => <li key={origin}>{origin}</li>)}</ul>
        <p>The screen, password and verification codes stay out of chat. Credentials are not saved to Vault. Finishing returns control to the agent to check this browser session.</p>
        {error ? <p role="alert">{error}</p> : null}
        <button disabled={busy} onClick={() => { setError(''); send({ action: 'approve' }); }}>Continue to private login</button>
        <button disabled={state.current.finishing} onClick={() => send({ action: 'cancel' })}>Cancel</button>
      </section> : <>
      <header className="private-browser-header"><span title={frame?.status === "active" ? frame.origin ?? intake.origin : intake.origin}>🔒 {frame?.status === "active" ? frame.origin ?? intake.origin : intake.origin}</span><button aria-label="Show keyboard" disabled={!frame || state.current.blocked} onClick={() => focusKeyboard()}>Keyboard</button><button disabled={busy} onClick={refresh}>Refresh</button>{login ? <button disabled={state.current.finishing} onClick={() => { input.current?.blur(); send({ action: 'cancel' }); }}>Cancel</button> : null}<button disabled={state.current.finishing || state.current.finished} onClick={() => { commit(); input.current?.blur(); send({ action: 'finish' }); }}>Done</button></header>
      {error ? <p className="private-browser-status" role="alert">{error}</p> : null}
      {!frame && !error ? <p className="private-browser-status" role="status">Opening private browser…</p> : null}
      {notice && !error ? <p className="private-browser-status" role="status">{notice}</p> : null}
      <div className="private-browser-stage">
        {frame?.status === 'active' ? <div ref={screen} className="private-browser-screen" style={{ aspectRatio: `${frame.width}/${frame.height}`, maxWidth: `min(100%, calc((var(--private-browser-height) - 60px) * ${frame.width / frame.height}))` }}
          onPointerDown={event => {
            if (state.current.blocked || state.current.pointer !== undefined) return;
            event.preventDefault(); const p = point(event.clientX, event.clientY); state.current.pointer = event.pointerId; event.currentTarget.setPointerCapture(event.pointerId); setDot(p);
            const region = frame.inputs?.find(r => p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height);
            state.current.region = region ? { type: region.type, multiline: region.multiline } : undefined;
            send({ action: 'touch', phase: 'start', ...p });
            if (region) focusKeyboard(state.current.region); else input.current?.blur();
          }}
          onPointerMove={event => { if (state.current.pointer !== event.pointerId) return; const p = point(event.clientX, event.clientY); setDot(p); send({ action: 'touch', phase: 'move', ...p }); }}
          onPointerUp={event => { if (state.current.pointer !== event.pointerId) return; send({ action: 'touch', phase: 'end', ...point(event.clientX, event.clientY) }); state.current.pointer = undefined; setDot(undefined);
            // iOS only raises the keyboard for focus inside touchend/click activation.
            const region = state.current.region; state.current.region = undefined; if (region) focusKeyboard(region); }}
          onPointerCancel={event => { if (state.current.pointer !== event.pointerId) return; send({ action: 'touch', phase: 'cancel' }); state.current.pointer = undefined; setDot(undefined); }}
          onLostPointerCapture={() => { if (state.current.pointer !== undefined) { send({ action: 'touch', phase: 'cancel' }); state.current.pointer = undefined; setDot(undefined); } }}
          onWheel={event => { send({ action: 'scroll', delta_y: Math.max(-2000, Math.min(2000, Math.round(event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? frame.height : 1)))) }); }}>
          <img src={frame.image} alt="Private browser screen" draggable={false} />
          {dot ? <i className="private-browser-touch" style={{ left: `${dot.x * 100}%`, top: `${dot.y * 100}%` }} /> : null}
        </div> : null}
      </div>
      <textarea ref={textInput} className="private-browser-keyboard" aria-label="Private browser keyboard" tabIndex={-1} autoComplete="off" autoCorrect="off" autoCapitalize="none" spellCheck={false} inputMode={hint.type === 'password' ? 'text' : hint.type === 'number' ? 'numeric' : hint.type} {...keyboardEvents} />
      <input ref={passwordInput} type="password" className="private-browser-keyboard" aria-label="Private browser password keyboard" tabIndex={-1} autoComplete="off" autoCorrect="off" autoCapitalize="none" spellCheck={false} {...keyboardEvents} />
      </>}
    </dialog>, document.body) : null}
  </section>;
}
