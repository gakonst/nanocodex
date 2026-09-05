import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Clock3, X } from "lucide-react";
import type { ManagedCronTrigger as CronTrigger, ManagedAgent } from "nanocodex/managed";
import "./ManagedAgentSchedules.css";

type ScheduleAgent = Pick<ManagedAgent, "id" | "triggers">;
type Draft = { id: string; cron: string; timezone: string; input: string; enabled: boolean };
const freshDraft = (): Draft => ({
  id: crypto.randomUUID(), cron: "0 9 * * *",
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC", input: "", enabled: true,
});

export function ManagedAgentSchedules({ agent }: { agent: ScheduleAgent }) {
  const [open, setOpen] = useState(false);
  return <>
    <button type="button" className="agent-schedules-open" aria-haspopup="dialog" aria-label="Schedules" title="Schedules"
      onClick={() => setOpen(true)}><Clock3 size={17} aria-hidden="true" /></button>
    {open && createPortal(<ScheduleDialog key={agent.id} agent={agent} onClose={() => setOpen(false)} />, document.body)}
  </>;
}

function ScheduleDialog({ agent, onClose }: { agent: ScheduleAgent; onClose(): void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const active = useRef(false);
  const locked = useRef(false);
  const [rows, setRows] = useState<readonly CronTrigger[]>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState("");
  const [draft, setDraft] = useState<Draft>();
  const [editing, setEditing] = useState(false);
  const [deleting, setDeleting] = useState<string>();

  async function run(operation: () => Promise<void>) {
    if (locked.current) return;
    locked.current = true;
    setPending(true);
    setError(undefined);
    setNotice("");
    try { await operation(); }
    catch (cause) { if (active.current) setError(cause instanceof Error ? cause.message : "Request failed. Please retry."); }
    finally {
      locked.current = false;
      if (active.current) setPending(false);
    }
  }

  async function refresh() {
    const listed = await agent.triggers.list();
    if (active.current) setRows(listed);
  }

  useEffect(() => {
    active.current = true;
    dialog.current?.showModal();
    void run(refresh);
    return () => { active.current = false; dialog.current?.close(); };
  }, [agent]);

  function replace(saved: CronTrigger) {
    if (!active.current) return;
    setRows((current) => [...(current ?? []).filter((row) => row.id !== saved.id), saved].sort((a, b) => a.id.localeCompare(b.id)));
  }

  return <dialog ref={dialog} className="agent-schedules" aria-labelledby="agent-schedules-title"
    onCancel={(event) => { event.preventDefault(); onClose(); }}>
    <header>
      <div><h2 id="agent-schedules-title">Schedules</h2><p>Run a prompt in this conversation automatically.</p></div>
      <button type="button" aria-label="Close schedules" onClick={onClose}><X size={18} /></button>
    </header>
    <div className="agent-schedules-body">
      <p className="agent-schedules-note">Runs use this agent’s model and account permissions, even when you’re away, and may incur usage charges. Browser or local Hands must be online for tools that need them.</p>
      <div className="agent-schedules-toolbar">
        <button type="button" disabled={pending || rows === undefined || Boolean(draft)} onClick={() => {
          setDraft(freshDraft()); setEditing(false); setDeleting(undefined); setError(undefined); setNotice("");
        }}>New schedule</button>
        <button type="button" disabled={pending} onClick={() => void run(refresh)}>Refresh</button>
      </div>
      {error && <p role="alert" className="agent-schedules-error">{error}</p>}
      <p role="status" className="agent-schedules-status">{pending ? "Saving or loading…" : notice}</p>
      {rows === undefined && !error && <p>Loading schedules…</p>}
      {rows?.length === 0 && !draft && <p>No schedules yet.</p>}
      {draft && <form onSubmit={(event) => {
        event.preventDefault();
        // Portals still bubble React events through the chat composer. Saving a
        // schedule must never submit the user's unsent conversation draft.
        event.stopPropagation();
        const { id, ...config } = draft;
        void run(async () => {
          if (!config.input.trim()) throw new Error("Enter a prompt.");
          if (!editing && rows?.some((row) => row.id === id)) throw new Error("That schedule ID already exists. Choose another ID or edit the existing schedule.");
          const saved = await agent.triggers.put(id, config);
          if (!active.current) return;
          replace(saved); setDraft(undefined); setNotice(editing ? "Schedule updated." : "Schedule created.");
        });
      }}>
        <h3>{editing ? "Edit schedule" : "New schedule"}</h3>
        <fieldset disabled={pending}>
          <label>Schedule ID<input required pattern={"[A-Za-z0-9_\\-]{1,64}"} maxLength={64} readOnly={editing}
            value={draft.id} onChange={(event) => setDraft({ ...draft, id: event.target.value })} /></label>
          <label>Frequency<select value={["0 9 * * *", "0 9 * * MON-FRI", "0 * * * *"].includes(draft.cron) ? draft.cron : "custom"}
            onChange={(event) => setDraft({ ...draft, cron: event.target.value === "custom" ? "" : event.target.value })}>
            <option value="0 9 * * *">Every day at 09:00</option>
            <option value="0 9 * * MON-FRI">Weekdays at 09:00</option>
            <option value="0 * * * *">Every hour</option>
            <option value="custom">Custom cron</option>
          </select></label>
          <div className="agent-schedules-fields">
            <label>Cron expression<input required maxLength={256} placeholder="0 9 * * *" value={draft.cron}
              aria-describedby="agent-schedules-cron-help" onChange={(event) => setDraft({ ...draft, cron: event.target.value })} /></label>
            <label>Time zone<input required maxLength={128} placeholder="Europe/Athens" value={draft.timezone}
              onChange={(event) => setDraft({ ...draft, timezone: event.target.value })} /></label>
          </div>
          <p id="agent-schedules-cron-help" className="agent-schedules-note">Five fields: minute, hour, day of month, month, weekday. Times follow the selected IANA time zone, including daylight saving.</p>
          <label>Prompt<textarea required rows={4} value={draft.input} placeholder="Summarize what needs my attention today."
            onChange={(event) => setDraft({ ...draft, input: event.target.value })} /></label>
          <label className="agent-schedules-checkbox"><input type="checkbox" checked={draft.enabled}
            onChange={(event) => setDraft({ ...draft, enabled: event.target.checked })} />Enabled</label>
          <div className="agent-schedules-actions">
            <button type="submit">{editing ? "Save changes" : "Create schedule"}</button>
            <button type="button" onClick={() => { setDraft(undefined); setError(undefined); }}>Cancel</button>
          </div>
        </fieldset>
      </form>}
      <ul className="agent-schedules-list">
        {rows?.map((row) => <li key={row.id} aria-label={`Schedule ${row.id}`}>
          <div className="agent-schedules-row"><strong>{row.id}</strong><span>{row.enabled ? "Enabled" : "Paused"}</span></div>
          <p><code>{row.cron}</code> · {row.timezone}</p>
          <p className="agent-schedules-prompt">{row.input}</p>
          <p>Next: {row.next_run_at === null ? "Paused" : formatTime(row.next_run_at, row.timezone)}</p>
          {row.last_run_at !== null && <p title={row.last_turn_id ?? undefined}>Last dispatched: {formatTime(row.last_run_at, row.timezone)} · see conversation for result</p>}
          {row.last_skipped_at !== null && <p>Last skipped while busy: {formatTime(row.last_skipped_at, row.timezone)}</p>}
          <div className="agent-schedules-actions">
            <button type="button" disabled={pending || Boolean(draft)} onClick={() => {
              setDraft({ id: row.id, cron: row.cron, timezone: row.timezone, input: row.input, enabled: row.enabled });
              setEditing(true); setDeleting(undefined); setError(undefined); setNotice("");
            }}>Edit</button>
            <button type="button" disabled={pending || Boolean(draft)} onClick={() => void run(async () => {
              const saved = await agent.triggers.put(row.id, { cron: row.cron, timezone: row.timezone, input: row.input, enabled: !row.enabled });
              replace(saved); if (active.current) setNotice(saved.enabled ? "Schedule resumed." : "Schedule paused. Any running turn continues.");
            })}>{row.enabled ? "Pause" : "Resume"}</button>
            <button type="button" disabled={pending || Boolean(draft)} onClick={() => setDeleting(row.id)}>Delete</button>
          </div>
          {deleting === row.id && <div className="agent-schedules-delete">
            <p>Delete this schedule? Conversation history and any running turn are kept.</p>
            <div className="agent-schedules-actions">
              <button type="button" disabled={pending} onClick={() => void run(async () => {
                await agent.triggers.delete(row.id);
                if (!active.current) return;
                setRows((current) => current?.filter((item) => item.id !== row.id));
                setDeleting(undefined); setNotice("Schedule deleted.");
              })}>Confirm delete</button>
              <button type="button" disabled={pending} onClick={() => setDeleting(undefined)}>Keep schedule</button>
            </div>
          </div>}
        </li>)}
      </ul>
      <p className="agent-schedules-note">Busy agents skip occurrences; missed times are not replayed individually. Pausing or deleting a schedule does not cancel a turn already running.</p>
    </div>
  </dialog>;
}

function formatTime(value: number, timeZone: string) {
  return new Intl.DateTimeFormat(undefined, { timeZone, dateStyle: "medium", timeStyle: "short" }).format(value);
}
