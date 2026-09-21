import { useEffect, useState } from "react";

import type { ElevenLabsVoice, ElevenLabsManager } from "nanocodex-react";
export type { ElevenLabsVoice, ElevenLabsManager } from "nanocodex-react";

/** Credentials and recordings go directly to the authenticated account service. */
export function ElevenLabsSettings({ manager, voiceId, onSelect }: {
  manager: ElevenLabsManager;
  voiceId: string;
  onSelect(voiceId: string): void;
}) {
  const [voices, setVoices] = useState<readonly ElevenLabsVoice[]>([]);
  const [apiKey, setApiKey] = useState("");
  const [name, setName] = useState("");
  const [files, setFiles] = useState<readonly File[]>([]);
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [fileInputKey, setFileInputKey] = useState(0);
  useEffect(() => {
    let disposed = false;
    setBusy(true);
    setError(undefined);
    void manager.listVoices().then((items) => { if (!disposed) setVoices(items); })
      .catch((failure: unknown) => { if (!disposed) setError(message(failure)); })
      .finally(() => { if (!disposed) setBusy(false); });
    return () => { disposed = true; };
  }, [manager]);
  async function run(action: () => Promise<void>) {
    setBusy(true); setError(undefined); setNotice(undefined);
    try { await action(); } catch (failure) { setError(message(failure)); }
    finally { setBusy(false); }
  }
  return <div className="agent-elevenlabs-settings" role="group" aria-label="ElevenLabs settings">
    <label>ElevenLabs voice<select aria-label="ElevenLabs voice" value={voiceId} disabled={busy}
      onChange={(event) => onSelect(event.target.value)}>
      <option value="">Select a voice</option>
      {voiceId && !voices.some((voice) => voice.voiceId === voiceId)
        ? <option value={voiceId}>Saved voice ({voiceId})</option> : null}
      {voices.map((voice) => <option key={voice.voiceId} value={voice.voiceId}>
        {voice.name}{voice.category === "cloned" ? " (cloned)" : ""}
      </option>)}
    </select></label>
    <button type="button" disabled={busy} onClick={() => { void run(async () => {
      setVoices(await manager.listVoices());
    }); }}>Refresh voices</button>
    <details><summary>Set up ElevenLabs API key</summary>
      <p>Your key is stored securely on your account and is never saved in browser preferences.</p>
      <label>API key<input type="password" autoComplete="off" value={apiKey}
        onChange={(event) => setApiKey(event.target.value)} /></label>
      <button type="button" disabled={busy || !apiKey.trim()} onClick={() => { void run(async () => {
        const key = apiKey.trim(); setApiKey("");
        await manager.saveApiKey(key);
        setNotice("ElevenLabs connected.");
        setVoices(await manager.listVoices());
      }); }}>Save API key</button>
    </details>
    <details><summary>Clone a voice</summary>
      <label>Clone name<input value={name} maxLength={100} onChange={(event) => setName(event.target.value)} /></label>
      <p>Upload 1–5 audio recordings, up to 10 MiB each and 20 MiB total. Some voices require verification in your ElevenLabs account.</p>
      <label>Voice recordings<input key={fileInputKey} type="file" accept="audio/*" multiple
        onChange={(event) => setFiles(Array.from(event.target.files ?? []))} /></label>
      <label className="agent-voice-consent"><input type="checkbox" checked={consent}
        onChange={(event) => setConsent(event.target.checked)} />
        I own this voice or have explicit permission to clone it, and consent to sending these recordings to ElevenLabs.
      </label>
      <button type="button" disabled={busy || !name.trim() || !files.length || !consent}
        onClick={() => { if (!consent || !name.trim() || !files.length) return; void run(async () => {
          if (files.length > 5 || files.some((file) => !file.size || file.size > 10 * 1024 * 1024)
            || files.reduce((total, file) => total + file.size, 0) > 20 * 1024 * 1024)
            throw new Error("Choose 1–5 nonempty audio files, up to 10 MiB each and 20 MiB total.");
          const cloned = await manager.cloneVoice({ name: name.trim(), files, consent: true });
          setVoices((current) => [...current.filter((voice) => voice.voiceId !== cloned.voiceId), cloned]);
          if (!cloned.requiresVerification) onSelect(cloned.voiceId);
          setName(""); setFiles([]); setConsent(false); setFileInputKey((key) => key + 1);
          setNotice(cloned.requiresVerification
            ? "Voice created. Complete verification in your ElevenLabs account before selecting it for speech."
            : "Voice cloned and selected. Save your voice settings to use it.");
        }); }}>Create voice clone</button>
    </details>
    {busy ? <span role="status">Working…</span> : null}
    {notice ? <span role="status">{notice}</span> : null}
    {error ? <span role="alert">{error}</span> : null}
  </div>;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : "ElevenLabs request failed. Please retry.";
}
