import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Folder, MessageCircle } from "lucide-react";
import { listProjects, mainThread } from "./mainThreadApi";
import { accountQueryKey } from "./queryClient";

/** Only the server assigns the global thread and project coordinators. */
export function MainThreadNavigation({ accountId, selectedId, onSelect, onConnect }: {
  accountId?: string;
  selectedId?: string;
  onSelect(id: string): void;
  onConnect(): void;
}) {
  const queryClient = useQueryClient();
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState<string>();
  const inFlight = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const key = [...accountQueryKey(accountId ?? ""), "main-thread"];
  const lookup = useQuery({ queryKey: key, queryFn: ({ signal }) => mainThread("GET", fetch, signal), enabled: Boolean(accountId), retry: false });
  const projects = useQuery({ queryKey: [...accountQueryKey(accountId ?? ""), "projects"], queryFn: ({ signal }) => listProjects(fetch, signal), enabled: Boolean(accountId), refetchInterval: 30_000 });
  async function openMain() {
    if (!accountId) { onConnect(); return; }
    if (inFlight.current) return;
    inFlight.current = true;
    setOpening(true);
    setError(undefined);
    try {
      const id = await mainThread("PUT");
      if (!id) throw new Error("Main Thread is unavailable.");
      await queryClient.cancelQueries({ queryKey: key, exact: true });
      if (!mounted.current) return;
      queryClient.setQueryData(key, id);
      onSelect(id);
    } catch (cause) {
      if (!mounted.current) return;
      setError(cause instanceof Error ? cause.message : "Couldn’t open Main Thread.");
    } finally { inFlight.current = false; setOpening(false); }
  }
  return <>
    <nav className="agent-navigation-primary" aria-label="Main Thread">
      <button type="button" disabled={opening} aria-busy={opening} aria-current={selectedId && lookup.data === selectedId ? "page" : undefined} onClick={() => void openMain()}>
        <MessageCircle aria-hidden="true" /><span>{opening ? "Opening Main Thread…" : "Main Thread"}</span>
      </button>
    </nav>
    {error && <div className="agent-navigation-error"><p role="alert">{error}</p><button type="button" onClick={() => void openMain()}>Try again</button></div>}
    {accountId && <section aria-label="Projects" className="agent-project-navigation">
      <div className="agent-navigation-heading"><span>Projects</span></div>
      {projects.isPending ? <p className="agent-navigation-empty" role="status">Loading projects…</p> : projects.isError ? <div className="agent-navigation-error"><p role="alert">{projects.error.message}</p><button type="button" onClick={() => void projects.refetch()}>Retry projects</button></div> : projects.data?.length ? projects.data.map(project => <button
        key={project.id} type="button" className="agent-navigation-thread" disabled={!project.coordinator_agent_id}
        title={project.coordinator_agent_id ? project.name : `${project.name} has no coordinator yet`}
        aria-current={selectedId && selectedId === project.coordinator_agent_id ? "location" : undefined}
        onClick={() => { if (project.coordinator_agent_id) onSelect(project.coordinator_agent_id); }}
      ><Folder aria-hidden="true" /><span>{project.name}</span></button>) : <p className="agent-navigation-empty">No projects yet.</p>}
    </section>}
  </>;
}
