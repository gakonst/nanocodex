import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useAccountSession } from "./AccountSession";
import { accountQueryKey } from "./queryClient";
import type { RemoteHand } from "./handRemote";
import { activePlayback, createPlaybackLink, listPlaybackLinks, PlaybackError, playbackDurations, revokePlaybackLink,
  type PlaybackPreset, type PlaybackRequest } from "./remotePlayback";

const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
const errorText = (error: unknown) => error instanceof Error ? error.message : "The playback request failed.";

/** View-only HLS links. The bearer URL lives only in this component's state. */
export function PlaybackLinks({ hand }: { hand: RemoteHand }) {
  const accountId = useAccountSession().account?.id;
  const client = useQueryClient();
  const key = [...accountQueryKey(accountId), "playback-links"];
  const links = useQuery({ queryKey: key, queryFn: ({ signal }) => listPlaybackLinks(signal), enabled: Boolean(accountId), refetchInterval: 10_000 });
  const [expiresInSeconds, setExpires] = useState(3600);
  const [preset, setPreset] = useState<PlaybackPreset>("720p");
  // An unconfirmed request keeps its operation ID; only an explicit Retry resends it.
  const [unconfirmed, setUnconfirmed] = useState<PlaybackRequest>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [created, setCreated] = useState<{ id: string; url?: string; expires_at: number }>();
  const [copied, setCopied] = useState(false);
  const [stopping, setStopping] = useState<string>();
  const refresh = () => client.invalidateQueries({ queryKey: key });

  async function create() {
    if (pending) return;
    const request = unconfirmed ?? { operationId: crypto.randomUUID(), expiresInSeconds, preset };
    setPending(true); setError(""); setCopied(false); setCreated(undefined);
    try {
      const receipt = await createPlaybackLink(hand, request);
      setUnconfirmed(undefined);
      setCreated({ id: receipt.link.id, url: receipt.url, expires_at: receipt.link.expires_at });
    } catch (failure) {
      setUnconfirmed(failure instanceof PlaybackError && failure.uncertain ? request : undefined);
      setError(errorText(failure));
    } finally { setPending(false); void refresh(); }
  }
  async function stop(id: string) {
    setStopping(id); setError("");
    try { await revokePlaybackLink(id); if (created?.id === id) setCreated(undefined); }
    catch (failure) { setError(errorText(failure)); }
    finally { setStopping(undefined); void refresh(); }
  }
  async function copy(url: string) {
    try { await navigator.clipboard.writeText(url); setCopied(true); } catch { setError("Copy failed. Select the link and copy it manually."); }
  }
  const mine = (links.data ?? []).filter(link => link.machine_id === hand.machine_id && link.surface_id === hand.id);
  const active = mine.filter(activePlayback);
  return <section className="remote-screen-playback" aria-label="Playback links">
    <form onSubmit={event => { event.preventDefault(); void create(); }}>
      <label>Expires after<select value={unconfirmed?.expiresInSeconds ?? expiresInSeconds} disabled={pending || !!unconfirmed}
        onChange={event => setExpires(Number(event.target.value))}>
        {playbackDurations.map(([seconds, label]) => <option key={seconds} value={seconds}>{label}</option>)}</select></label>
      <label>Quality<select value={unconfirmed?.preset ?? preset} disabled={pending || !!unconfirmed}
        onChange={event => setPreset(event.target.value as PlaybackPreset)}>
        <option value="720p">720p</option><option value="1080p">1080p</option></select></label>
      <button type="submit" disabled={pending}>{pending ? "Creating…" : unconfirmed ? "Retry request" : "Create playback link"}</button>
      {unconfirmed && !pending && <button type="button" onClick={() => { setUnconfirmed(undefined); setError(""); }}>Discard request</button>}
    </form>
    <small>Anyone with the link can watch this screen (view only, no control or sound) until it expires or you stop it.</small>
    {error && <p role="alert">{error}{unconfirmed ? " Check active links below first; Retry reuses the same request and cannot create a duplicate." : ""}</p>}
    {created && (created.url ? <div className="remote-screen-playback-url">
      <label>Playback link (shown once)<input readOnly value={created.url} spellCheck={false} onFocus={event => event.currentTarget.select()} /></label>
      <button type="button" onClick={() => void copy(created.url!)}>{copied ? "Copied" : "Copy link"}</button>
      <small>Anyone with this link can watch until {when(created.expires_at)}. It won't be shown again.</small>
    </div> : <p role="status">This link was already created, but its URL can't be shown again. Stop it and create a new one.</p>)}
    <h3>Active links</h3>
    {links.isPending ? <p role="status">Loading…</p> : links.isError ? <p role="alert">{errorText(links.error)} <button type="button" onClick={() => void refresh()}>Retry</button></p>
      : active.length === 0 ? <p>No active playback links for this screen.</p>
      : <ul>{active.map(link => <li key={link.id}>
        <span>{link.preset} · {link.state === "live" ? "Live" : "Starting"} · expires {when(link.expires_at)}</span>
        <button type="button" disabled={stopping === link.id} onClick={() => void stop(link.id)}>{stopping === link.id ? "Stopping…" : "Stop"}</button>
      </li>)}</ul>}
    {mine.filter(link => link.state === "failed" && link.error).slice(0, 1).map(link => <p key={link.id} role="status">Last link failed: {link.error}</p>)}
  </section>;
}
