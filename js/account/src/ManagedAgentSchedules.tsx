import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAccountSession } from "./AccountSession";
import { accountQueryKey } from "./queryClient";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Clock3, X } from "lucide-react";
import { Agent, type ManagedAgent } from "nanocodex/managed";
import { readScheduledAgents, scheduledAgentCandidates } from "./scheduledAgents";
import "./ManagedAgentSchedules.css";

type ScheduleAgent = Pick<ManagedAgent, "id" | "triggers">;
type Draft = { id: string; cron: string; timezone: string; input: string; enabled: boolean; session_mode: "new" | "continue" };
const freshDraft = (): Draft => ({
  id: crypto.randomUUID(), cron: "0 9 * * *",
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC", input: "", enabled: true, session_mode: "new",
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
  const accountId = useAccountSession().account?.id;
  const client = useQueryClient();
  const [allAgents, setAllAgents] = useState(false);
  const [editingAgent, setEditingAgent] = useState<ScheduleAgent>(agent);
  const schedulesKey = [...accountQueryKey(accountId), "schedules"] as const;
  const queryKey = [...schedulesKey, allAgents ? "all" : agent.id] as const;
  const query = useQuery({ queryKey, queryFn: async ({ signal }) => {
    if (!allAgents) return (await agent.triggers.list()).map(row => ({ ...row, owner: agent, ownerTitle: "This conversation" }));
    const candidates = await scheduledAgentCandidates(fetch, signal);
    const rows = await readScheduledAgents(candidates, async candidate => {
      const owner = Agent.open(candidate.id);
      const reader = Agent.open(candidate.id, { fetch: (input, init) => fetch(input, { ...init, signal }) });
      try {
        return (await reader.triggers.list()).map(row => ({ ...row, owner, ownerTitle: candidate.title }));
      } catch (error) {
        // An agent may be removed between discovery and reading its schedules.
        if (!(error instanceof Error && "status" in error && error.status === 404)) throw error;
        return [];
      }
    }, signal);
    return rows.flat();
  }, enabled: Boolean(accountId), staleTime: 15_000, refetchInterval: 30_000 });
  const rows = query.data;
  const [operationError, setError] = useState<string>();
  const mutation = useMutation({
    mutationKey: [...queryKey, "edit"],
    mutationFn: (operation: () => Promise<void>) => operation(),
    onSuccess: async () => {
      await client.cancelQueries({ queryKey, exact: true });
      await client.invalidateQueries({ queryKey: schedulesKey });
    },
  });
  const pending = mutation.isPending || query.isFetching;
  const error = operationError ?? query.error?.message;
  const [notice, setNotice] = useState("");
  const [draft, setDraft] = useState<Draft>();
  const [editing, setEditing] = useState(false);

  async function run(operation: () => Promise<void>) {
    if (mutation.isPending) return;
    setError(undefined);
    setNotice("");
    try { await mutation.mutateAsync(operation); }
    catch (cause) { if (active.current) setError(cause instanceof Error ? cause.message : "Request failed. Please retry."); }
  }

  async function refresh() {
    await query.refetch();
  }

  useEffect(() => {
    active.current = true;
    dialog.current?.showModal();
    return () => { active.current = false; dialog.current?.close(); };
  }, [agent]);

  return <dialog ref={dialog} className="agent-schedules" aria-labelledby="agent-schedules-title"
    onCancel={(event) => { event.preventDefault(); onClose(); }}>
    <header>
      <div><h2 id="agent-schedules-title">Schedules</h2><p>Run a prompt automatically in a new or existing conversation.</p></div>
      <button type="button" aria-label="Close schedules" onClick={onClose}><X size={18} /></button>
    </header>
    <div className="agent-schedules-body">
      <p className="agent-schedules-note">Runs use their owning agent’s model and account permissions, even when you’re away, and may incur usage charges. Browser or local Hands must be online for tools that need them.</p>
      <label className="agent-schedules-checkbox"><input type="checkbox" checked={allAgents} disabled={pending || Boolean(draft)}
        onChange={event => setAllAgents(event.target.checked)} />Show schedules from all my agents</label>
      <div className="agent-schedules-toolbar">
        <button type="button" disabled={pending || rows === undefined || Boolean(draft)} onClick={() => {
          setDraft(freshDraft()); setEditingAgent(agent); setEditing(false); setError(undefined); setNotice("");
        }}>New schedule</button>
        <button type="button" disabled={pending} onClick={() => void refresh()}>Refresh</button>
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
          if (!editing && rows?.some((row) => row.id === id && row.owner.id === agent.id)) throw new Error("That schedule ID already exists. Choose another ID or edit the existing schedule.");
          if (editing) await editingAgent.triggers.update(id, config);
          else await editingAgent.triggers.put(id, config);
          if (!active.current) return;
          setDraft(undefined); setNotice(editing ? "Schedule updated." : "Schedule created.");
        });
      }}>
        <h3>{editing ? "Edit schedule" : "New schedule"}</h3>
        <p className="agent-schedules-note">{editing ? "This schedule belongs to " : "New schedules belong to "}<a href={`/agent/${encodeURIComponent(editingAgent.id)}`}>{editingAgent.id === agent.id ? "this conversation" : editingAgent.id}</a>.</p>
        <fieldset disabled={pending}>
          <label>Schedule ID<input required pattern={"[A-Za-z0-9_\\-]{1,64}"} maxLength={64} readOnly={editing}
            value={draft.id} onChange={(event) => setDraft({ ...draft, id: event.target.value })} /></label>
          <label>Run in<select value={draft.session_mode}
            onChange={(event) => setDraft({ ...draft, session_mode: event.target.value as Draft["session_mode"] })}>
            <option value="new">New session each time</option>
            <option value="continue">Continue the schedule’s agent conversation</option>
          </select></label>
          <p className="agent-schedules-note">{draft.session_mode === "new"
            ? "Each run starts with empty conversation history and the owning agent’s current model settings. Results appear as separate conversations."
            : "Each run adds a turn to the owning conversation, including its existing history. Occurrences are skipped while it is busy."}</p>
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
        {rows?.map((row) => <li key={`${row.owner.id}:${row.id}`} aria-label={`Schedule ${row.id}`}>
          <div className="agent-schedules-row"><strong>{row.id}</strong><span>{row.enabled ? "Enabled" : "Paused"}</span></div>
          {allAgents && <p>Agent: <a href={`/agent/${encodeURIComponent(row.owner.id)}`}>{row.ownerTitle}</a></p>}
          <p>{row.session_mode === "new" ? "New session each time" : "Continues its agent conversation"}</p>
          <p><code>{row.cron}</code> · {row.timezone}</p>
          <p className="agent-schedules-prompt">{row.input}</p>
          <p>Next: {row.next_run_at === null ? "Paused" : formatTime(row.next_run_at, row.timezone)}</p>
          {row.last_run_at !== null && <p title={row.last_turn_id ?? undefined}>Last dispatched: {formatTime(row.last_run_at, row.timezone)} · <a href={`/agent/${encodeURIComponent(row.last_agent_id ?? row.owner.id)}`}>Open run</a></p>}
          {row.last_skipped_at !== null && <p>Last skipped while busy: {formatTime(row.last_skipped_at, row.timezone)}</p>}
          <div className="agent-schedules-actions">
            <button type="button" disabled={pending || Boolean(draft)} onClick={() => {
              setDraft({ id: row.id, cron: row.cron, timezone: row.timezone, input: row.input, enabled: row.enabled, session_mode: row.session_mode });
              setEditingAgent(row.owner); setEditing(true); setError(undefined); setNotice("");
            }}>Edit</button>
            <button type="button" disabled={pending || Boolean(draft)} onClick={() => void run(async () => {
              const saved = await row.owner.triggers.update(row.id, { enabled: !row.enabled });
              if (active.current) setNotice(saved.enabled ? "Schedule resumed." : "Schedule paused. Any running turn continues.");
            })}>{row.enabled ? "Pause" : "Resume"}</button>
            <button type="button" disabled={pending || Boolean(draft)} onClick={() => void run(async () => {
              await row.owner.triggers.delete(row.id);
              if (active.current) setNotice("Schedule canceled. Any running turn continues.");
            })}>Cancel schedule</button>
          </div>
        </li>)}
      </ul>
      <p className="agent-schedules-note">Continuing a busy conversation skips that occurrence. New sessions run independently. Missed times are not replayed individually. Pausing or deleting does not cancel a run already dispatched or being delivered.</p>
    </div>
  </dialog>;
}

function formatTime(value: number, timeZone: string) {
  return new Intl.DateTimeFormat(undefined, { timeZone, dateStyle: "medium", timeStyle: "short" }).format(value);
}
