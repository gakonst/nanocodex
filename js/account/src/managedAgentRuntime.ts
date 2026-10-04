import { decodeModelCatalog } from "./modelCatalog.ts";
import { QueryObserver, queryOptions } from "@tanstack/react-query";
import { appQueryClient, accountQueryKey, sessionQueryKey } from "./queryClient.ts";
import type { BrowserSession } from "./sessionQueries.ts";
import {
  Agent,
  type ManagedAgent,
  type ManagedCreateSettings,
} from "nanocodex/managed";
import type { Agent as ControllerAgent } from "nanocodex-react/agent";
import {
  createManagedAgentSource,
  type ManagedTerminalSource,
  type RetainedManagedHistory,
} from "nanocodex-connect-embed/managed";
export type { ManagedTerminalSource } from "nanocodex-connect-embed/managed";

const managedCreates = new Map<string, Promise<ManagedConversation>>();

export type ManagedConversation = Readonly<{
  id: string;
  title: string;
  updatedAt?: number;
  lastUserMessageAt?: number;
  turnCount?: number;
  presentation?: NonNullable<ManagedAgent["summary"]>["presentation"];
}>;

export type ManagedConversationSelection = Readonly<{
  conversations: readonly ManagedConversation[];
  selectedId?: string;
  replaceRoute: boolean;
}>;

function queryFetch(signal: AbortSignal): typeof fetch {
  return (input, init) => fetch(input, {
    ...init,
    signal: init?.signal ? AbortSignal.any([signal, init.signal]) : signal,
  });
}

export const managedConversationsKey = (accountId: string) => [...accountQueryKey(accountId), "conversations"] as const;

export function managedConversationsQueryOptions(accountId: string) {
  return queryOptions({
    queryKey: managedConversationsKey(accountId),
    queryFn: async ({ signal }) => {
      const agents = await Agent.list({ fetch: queryFetch(signal) });
      signal.throwIfAborted();
      return Object.freeze(agents.map(managedConversation).sort((a, b) => (b.lastUserMessageAt ?? 0) - (a.lastUserMessageAt ?? 0) || a.id.localeCompare(b.id)));
    },
    staleTime: 15_000,
  });
}

export function managedConversationQueryOptions(accountId: string, agentId: string) {
  return queryOptions({
    queryKey: [...accountQueryKey(accountId), "conversation", agentId],
    queryFn: async ({ signal }) => {
      const state = await Agent.open(agentId, { fetch: queryFetch(signal) }).state();
      signal.throwIfAborted();
      return state;
    },
    staleTime: 30_000,
  });
}

export async function listManagedConversations(
  accountId = "default",
  options: Readonly<{ refresh?: boolean }> = {},
): Promise<readonly ManagedConversation[]> {
  const query = managedConversationsQueryOptions(accountId);
  if (options.refresh) {
    await appQueryClient.cancelQueries({ queryKey: query.queryKey, exact: true });
    await appQueryClient.invalidateQueries({ queryKey: query.queryKey, exact: true, refetchType: "none" });
  }
  return appQueryClient.fetchQuery(query);
}

export function recordManagedConversationActivity(accountId: string, agentId: string, input: string): void {
  appQueryClient.setQueryData(managedConversationsQueryOptions(accountId).queryKey, (current) => current
    ? Object.freeze(current.map((item) => item.id === agentId ? {
      ...item,
      title: (item.turnCount ?? 0) === 0 ? titleFromPrompt(input) : item.title,
      turnCount: (item.turnCount ?? 0) + 1,
      lastUserMessageAt: Date.now(),
      updatedAt: Date.now(),
    } : item).sort((a, b) => (b.lastUserMessageAt ?? 0) - (a.lastUserMessageAt ?? 0) || a.id.localeCompare(b.id)))
    : undefined);
}

export async function loadManagedConversationSelection(options: Readonly<{
  accountId?: string;
  routeAgentId?: string;
  retainedAgentId?: string;
  hasCredential: boolean;
  createSettings?: ManagedCreateSettings;
  refresh?: boolean;
}>): Promise<ManagedConversationSelection> {
  const accountId = options.accountId ?? "default";
  const listing = listManagedConversations(accountId, { refresh: options.refresh });
  if (options.routeAgentId) {
    // Exact-route verification may fail while the parallel list is still in flight.
    void listing.catch(() => undefined);
    // Verify the exact route without gating its terminal on a possibly slow list.
    // The list is still started above, so navigation and sidebar load in parallel.
    await appQueryClient.fetchQuery(managedConversationQueryOptions(accountId, options.routeAgentId));
    const agentId = options.routeAgentId;
    const cached = appQueryClient.getQueryData<readonly ManagedConversation[]>(managedConversationsKey(accountId)) ?? [];
    const exact = cached.find(({ id }) => id === agentId) ?? Object.freeze({
      id: agentId,
      title: `Conversation ${agentId.slice(0, 8)}`,
    });
    const conversations = cached.some(({ id }) => id === agentId)
      ? cached
      : Object.freeze([exact, ...cached]);
    // A stale list response may arrive after the exact state and omit this id.
    // Reinsert it *after* listing settles, retaining the list's original freshness.
    void listing.then(() => {
      const key = managedConversationsKey(accountId);
      const listState = appQueryClient.getQueryState(key);
      if (listState?.data === undefined) return;
      appQueryClient.setQueryData<readonly ManagedConversation[]>(key,
        (current) => current && !current.some(({ id }) => id === agentId)
          ? Object.freeze([exact, ...current]) : current,
        { updatedAt: listState.dataUpdatedAt });
    }).catch(() => undefined);
    return Object.freeze({ conversations, selectedId: exact.id, replaceRoute: false });
  }
  const listed = await listing;
  const conversations = listed.length || !options.hasCredential
    ? listed
    : Object.freeze([await createManagedConversation(accountId, options.createSettings)]);
  const selectedId = conversations.find(({ id }) => id === options.retainedAgentId)?.id
    ?? conversations[0]?.id;
  return Object.freeze({
    conversations,
    ...(selectedId === undefined ? {} : { selectedId }),
    replaceRoute: selectedId !== undefined,
  });
}

/** A local-only id gives the new tab an identity before the server acknowledges it.
 * Never pass this id to Agent.open or put it in the URL. */
export function beginManagedConversationCreation(accountId: string): Readonly<{
  provisional: ManagedConversation;
  receipt: Promise<ManagedConversation>;
}> {
  const provisional = Object.freeze({
    id: `pending:${crypto.randomUUID()}`,
    title: "New agent",
    updatedAt: Date.now(),
    turnCount: 0,
  });
  return { provisional, receipt: createManagedConversation(accountId) };
}

/** A late create receipt must not take focus back from a tab chosen since creation. */
export function reconcileManagedCreateSelection(
  selectedId: string | undefined,
  provisionalId: string,
  actualId: string,
): string | undefined {
  return selectedId === provisionalId ? actualId : selectedId;
}

export function createManagedConversation(
  accountId = "default",
  settings?: ManagedCreateSettings,
): Promise<ManagedConversation> {
  const creationKey = `${accountId}:${JSON.stringify(settings)}`;
  const retained = managedCreates.get(creationKey);
  if (retained) return retained;
  const creating = (async () => {
    // Availability is account-owned. Never choose a hardcoded OpenAI model on a Claude-only account.
    const response = await fetch(new URL("/v1/models", location.origin), { credentials: "same-origin", cache: "no-store", headers: { accept: "application/json" } });
    if (!response.ok) { await response.body?.cancel(); throw new Error("Couldn’t check available models. Refresh your connections."); }
    const catalog = decodeModelCatalog(await response.json());
    const choice = catalog.models.find(model => model.id === (settings?.model ?? catalog.defaultModel));
    if (!choice) throw new Error("Connect a model subscription in account settings before starting a chat.");
    const selected = settings ?? { model: choice.id, thinking: choice.thinking.includes("low") ? "low" : choice.thinking[0], reasoningMode: "standard", fastMode: false };
    if (!choice.thinking.includes(selected.thinking) || !choice.reasoningModes.includes(selected.reasoningMode) || (selected.fastMode && !choice.fastMode)) {
      throw new Error("These model settings are unavailable. Choose settings from the current model catalog.");
    }
    const currentSession = appQueryClient.getQueryData<BrowserSession>(sessionQueryKey);
    if (currentSession && currentSession.account?.id !== accountId) {
      throw new Error("The account changed before creation. Start a new chat from the current account.");
    }
    return Agent.create({ settings: selected });
  })().then((agent) => {
    const conversation = Object.freeze({
      id: agent.id,
      title: "New conversation",
      updatedAt: Date.now(),
      turnCount: 0,
    });
    const queryKey = managedConversationsKey(accountId);
    const activeAccount = appQueryClient.getQueryData<BrowserSession>(sessionQueryKey)?.account?.id;
    if (activeAccount === accountId || appQueryClient.getQueryState(queryKey)) {
      appQueryClient.setQueryData<readonly ManagedConversation[]>(queryKey, (current) =>
        Object.freeze([conversation, ...(current ?? []).filter(({ id }) => id !== conversation.id)]));
      void appQueryClient.invalidateQueries({ queryKey, exact: true });
    }
    return conversation;
  }).finally(() => {
    if (managedCreates.get(creationKey) === creating) managedCreates.delete(creationKey);
  });
  managedCreates.set(creationKey, creating);
  return creating;
}

export function openManagedTerminalAgent(agentId: string): ControllerAgent {
  return managedTerminalAgent(openManagedAgent(agentId));
}

export function openManagedAgent(agentId: string): ManagedAgent {
  return Agent.open(agentId);
}

function managedConversation(agent: ManagedAgent): ManagedConversation {
  return Object.freeze({
    id: agent.id,
    title: titleFromPrompt(agent.summary?.title ?? "") || `Conversation ${agent.id.slice(0, 8)}`,
    ...(agent.summary === undefined ? {} : {
      updatedAt: agent.summary.updatedAt,
      lastUserMessageAt: agent.summary.lastUserMessageAt ?? 0,
      turnCount: agent.summary.turnCount,
      ...(agent.summary.presentation ? { presentation: agent.summary.presentation } : {}),
    }),
  });
}

/** Keep account cache lifetime and invalidation policy outside the shared source. */
export function managedTerminalAgent(
  managed: ManagedTerminalSource,
  options: Readonly<{ history?: boolean; accountId?: string }> = {},
): ControllerAgent {
  const accountId = options.accountId;
  return createManagedAgentSource(managed, {
    history: options.history,
    ...(accountId ? {
      cache: {
        attach(agentId) {
          const cacheKey = [...accountQueryKey(accountId), "conversation-history", agentId] as const;
          const snapshot = appQueryClient.getQueryData<RetainedManagedHistory>(cacheKey);
          const observer = new QueryObserver<RetainedManagedHistory>(appQueryClient, {
            queryKey: cacheKey, enabled: false, staleTime: Infinity, structuralSharing: false,
          });
          const release = observer.subscribe(() => {});
          const query = appQueryClient.getQueryCache().find({ queryKey: cacheKey, exact: true });
          return {
            snapshot,
            retain(history) {
              // Sign-out/account switching removes the query. Never restore it
              // from a watcher that detaches after that removal.
              if (query === appQueryClient.getQueryCache().find({ queryKey: cacheKey, exact: true })) {
                appQueryClient.setQueryData<RetainedManagedHistory>(cacheKey, history);
              }
            },
            release,
          };
        },
      },
      onActivity() {
        void appQueryClient.invalidateQueries({ queryKey: managedConversationsKey(accountId), exact: true });
        void appQueryClient.invalidateQueries({ queryKey: managedConversationQueryOptions(accountId, managed.id).queryKey, exact: true });
      },
    } : {}),
  });
}

function titleFromPrompt(input: string): string {
  const text = input.replace(/\s+/g, " ").trim();
  if (!text) return "";
  return text.length > 56 ? `${text.slice(0, 55).trimEnd()}…` : text;
}
