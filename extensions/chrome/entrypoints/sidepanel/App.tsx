import { useEffect, useMemo, useRef, useState } from "react";
import type { Agent, AgentControllerEvent, AgentTurn, AgentTurnResult } from "nanocodex-react/agent";
import {
  AgentTerminalView,
  ConversationHistoryRail,
  type AgentStatus,
  type AgentTerminalState,
  type ConversationSummary,
} from "nanocodex-terminal";
import type { ToolContext } from "nanocodex/host";
import { createPageAgent, type PageAgentSession } from "../../lib/agent";
import {
  connectNanocodex,
  cookieSyncTransport,
  createConversationId,
  disconnectNanocodex,
  isManagedAgentId,
  isConversationId,
  LEGACY_CONVERSATION_ID,
  reconnectNanocodex,
  type NanocodexConnection,
} from "../../lib/connect";
import type {
  CleanupInput,
  PageInterrupted,
  PageLease,
  PageSelectionSnapshot,
  PreviewInfo,
  TabClaim,
} from "../../lib/extension";
import { acquireCleanupHost, type CleanupHostLock } from "../../lib/host-lock";
import type { StoredSiteRecipe } from "../../lib/recipe";
import type {
  BrowserCookieJarV1,
  CookieCaptureHandle,
  CookieRestoreConfirmation,
  SyncedCookieJarReference,
} from "../../lib/cookie-sync";

interface ActiveOperation {
  cancelled: boolean;
  catalogTabRefs?: Set<string>;
  controller: AbortController;
  lease?: PageLease;
  ready?: Promise<PageLease>;
  selection?: Promise<PageSelectionSnapshot>;
  snapshots?: Promise<PageSelectionSnapshot>[];
  tabCursors?: Map<string, { catalogId: string; offset: number }>;
  targetTabRef?: string;
  turn?: AgentTurn;
  windowId?: Promise<number>;
}

export function App() {
  const [conversations, setConversations] = useState<readonly ConversationSummary[]>(loadConversations);
  const [conversationId, setConversationId] = useState(loadSelectedConversation);
  const [conversationPending, setConversationPending] = useState(false);
  const [railOpen, setRailOpen] = useState(false);
  const [connection, setConnection] = useState<NanocodexConnection>();
  const [agentSource, setAgentSource] = useState<Agent>();
  const [agentError, setAgentError] = useState<string>();
  const [agentOpening, setAgentOpening] = useState(false);
  const [operationActive, setOperationActive] = useState(false);
  const [restoring, setRestoring] = useState(true);
  const [connecting, setConnecting] = useState(false);
  const [activity, setActivity] = useState<string>();
  const [error, setError] = useState("");
  const [tab, setTab] = useState<TabClaim>();
  const [preview, setPreview] = useState<PreviewInfo>();
  const [kept, setKept] = useState("");
  const [saved, setSaved] = useState<StoredSiteRecipe[]>([]);
  const [cookieCapture, setCookieCapture] = useState<SyncedCookieJarReference>();
  const [cookieBusy, setCookieBusy] = useState(false);
  const [cookieNotice, setCookieNotice] = useState("");
  const connectionRef = useRef<NanocodexConnection | undefined>(undefined);
  const sessionRef = useRef<PageAgentSession | undefined>(undefined);
  const sessionOpeningRef = useRef<Promise<PageAgentSession> | undefined>(undefined);
  const sessionOpeningControllerRef = useRef<AbortController | undefined>(undefined);
  const hostLockRef = useRef<CleanupHostLock | undefined>(undefined);
  const operationRef = useRef<ActiveOperation | undefined>(undefined);
  const leaseRef = useRef<PageLease | undefined>(undefined);
  const closedRef = useRef(false);
  const closingRef = useRef<Promise<void> | undefined>(undefined);
  const conversationIdRef = useRef(conversationId);
  const transitionRef = useRef(true);
  const transitionVersionRef = useRef(0);
  connectionRef.current = connection;
  conversationIdRef.current = conversationId;

  useEffect(() => {
    let mounted = true;
    const restoringId = conversationIdRef.current;
    const version = ++transitionVersionRef.current;
    void reconnectNanocodex(restoringId, conversationIdentity(restoringId) ?? undefined)
      .then((restored) => {
        if (!mounted || transitionVersionRef.current !== version
          || conversationIdRef.current !== restoringId) return;
        if (restored) retainConversation(restoringId, restored);
        setConnection(restored);
      })
      .catch((cause) => {
        if (mounted) setError(errorMessage(cause));
      })
      .finally(() => {
        if (mounted && transitionVersionRef.current === version) {
          transitionRef.current = false;
          setRestoring(false);
        }
      });
    void refreshSaved().catch((cause) => setError(errorMessage(cause)));
    const listener = (value: unknown) => {
      const message = value as Partial<PageInterrupted>;
      if (
        message.type !== "page.interrupted"
        || typeof message.lease_id !== "string"
        || message.lease_id !== leaseRef.current?.lease_id
      ) return;
      const operation = operationRef.current;
      if (operation?.lease?.lease_id === message.lease_id) {
        operation.cancelled = true;
        operation.controller.abort(new Error("The selected page changed."));
        delete operation.lease;
        void operation.turn?.cancel().catch(() => {});
      }
      leaseRef.current = undefined;
      setCookieCapture(undefined);
      setTab(undefined);
      setPreview(undefined);
      setActivity(undefined);
      setError(typeof message.reason === "string" ? message.reason : "The selected page changed.");
    };
    const close = () => {
      closedRef.current = true;
      fencePanelRuntime();
    };
    chrome.runtime.onMessage.addListener(listener);
    window.addEventListener("pagehide", close);
    return () => {
      mounted = false;
      transitionVersionRef.current += 1;
      chrome.runtime.onMessage.removeListener(listener);
      window.removeEventListener("pagehide", close);
    };
  }, []);

  useEffect(() => {
    if (connection) void ensurePageAgent(connection).catch(() => {});
  }, [connection]);

  async function ensurePageAgent(activeConnection: NanocodexConnection): Promise<PageAgentSession> {
    if (sessionRef.current) return sessionRef.current;
    if (sessionOpeningRef.current) return sessionOpeningRef.current;
    setAgentError(undefined);
    setAgentOpening(true);
    const controller = new AbortController();
    let timedOut = false;
    const timeout = window.setTimeout(() => {
      timedOut = true;
      controller.abort(new Error("The page agent could not attach. Retry in a moment."));
    }, 15_000);
    sessionOpeningControllerRef.current = controller;
    const opening = (async () => {
      const hostLock = await acquireCleanupHost();
      if (!hostLock) {
        throw new Error("Another Nanocodex panel is using the cleanup agent. Close that panel before running here.");
      }
      if (closedRef.current || controller.signal.aborted || connectionRef.current !== activeConnection) {
        await hostLock.release();
        throw new Error("The page agent closed before it finished connecting.");
      }
      hostLockRef.current = hostLock;
      try {
        const session = await createPageAgent({
          connection: activeConnection,
          dispatch: dispatchCleanup,
          signal: controller.signal,
        });
        if (closedRef.current || controller.signal.aborted || connectionRef.current !== activeConnection) {
          await session.close();
          throw new Error("The page agent closed before it finished connecting.");
        }
        sessionRef.current = session;
        setAgentSource(session.source);
        return session;
      } catch (cause) {
        if (hostLockRef.current === hostLock) hostLockRef.current = undefined;
        await hostLock.release();
        throw cause;
      }
    })();
    sessionOpeningRef.current = opening;
    try {
      return await opening;
    } catch (cause) {
      if (timedOut) setAgentError("The page agent could not attach. Retry in a moment.");
      else if (!controller.signal.aborted) setAgentError(errorMessage(cause));
      throw cause;
    } finally {
      window.clearTimeout(timeout);
      setAgentOpening(false);
      if (sessionOpeningRef.current === opening) sessionOpeningRef.current = undefined;
      if (sessionOpeningControllerRef.current === controller) sessionOpeningControllerRef.current = undefined;
    }
  }

  async function closePanelRuntime(): Promise<void> {
    if (closingRef.current) return closingRef.current;
    const closing = (async () => {
      const operation = operationRef.current;
      if (operation) {
        operation.cancelled = true;
        operation.controller.abort(new Error("The side panel closed."));
        if (operation.turn) {
          await operation.turn.cancel().catch(() => {});
          await operation.turn.result().catch(() => {});
        }
      }
      sessionOpeningControllerRef.current?.abort(new Error("The side panel closed."));
      await sessionOpeningRef.current?.catch(() => {});
      const current = leaseRef.current;
      leaseRef.current = undefined;
      if (current) await sendMessage({ type: "lease.release", lease_id: current.lease_id }).catch(() => {});
      setCookieCapture(undefined);
      setCookieNotice("");
      const session = sessionRef.current;
      sessionRef.current = undefined;
      await session?.close().catch(() => {});
      const hostLock = hostLockRef.current;
      hostLockRef.current = undefined;
      await hostLock?.release().catch(() => {});
      setAgentSource(undefined);
      setAgentOpening(false);
      setActivity(undefined);
    })();
    closingRef.current = closing;
    return closing;
  }

  function fencePanelRuntime(): void {
    const operation = operationRef.current;
    if (operation) {
      operation.cancelled = true;
      operation.controller.abort(new Error("The side panel closed."));
      delete operation.lease;
      void releasePageSelections(operation);
      void operation.turn?.cancel().catch(() => {});
    }
    sessionOpeningControllerRef.current?.abort(new Error("The side panel closed."));
    const current = leaseRef.current;
    leaseRef.current = undefined;
    if (current) {
      void chrome.runtime.sendMessage({ type: "lease.release", lease_id: current.lease_id }).catch(() => {});
    }
    const session = sessionRef.current;
    sessionRef.current = undefined;
    void session?.close().catch(() => {});
    const hostLock = hostLockRef.current;
    hostLockRef.current = undefined;
    void hostLock?.release().catch(() => {});
  }

  async function refreshSaved(): Promise<void> {
    setSaved(await sendMessage<StoredSiteRecipe[]>({ type: "recipe.list" }));
  }

  async function claimSelectedPage(operation: ActiveOperation, requestedTabRef?: string): Promise<PageLease> {
    const windowId = await (operation.windowId ??= currentPanelWindowId());
    const selection = requestedTabRef
      ? undefined
      : await (operation.selection ??= selectedPageSelection(windowId));
    if (operation.cancelled || operationRef.current !== operation || closedRef.current) {
      throw new Error("The turn was cancelled before the selected tab was needed.");
    }
    const selectionId = requestedTabRef ?? selection?.default_tab_ref;
    const selectionAvailable = requestedTabRef
      ? operation.catalogTabRefs?.has(requestedTabRef) === true
      : Boolean(selectionId && selection?.tabs.some(({ tab_ref }) => tab_ref === selectionId));
    if (!selectionId || !selectionAvailable) {
      throw new Error(requestedTabRef
        ? "That open-tab reference is unavailable. List tabs again and choose one exact match."
        : "No active HTTP or HTTPS tab is available in this side panel's window.");
    }
    operation.targetTabRef = selectionId;
    const previous = leaseRef.current;
    const claimed = await sendMessage<PageLease>({
      type: "page.claim",
      selection_id: selectionId,
      ...(leaseRef.current ? { previous_lease_id: leaseRef.current.lease_id } : {}),
    });
    if (previous?.lease_id === leaseRef.current?.lease_id) {
      leaseRef.current = undefined;
      setPreview(undefined);
      setTab(undefined);
    }
    if (operation.cancelled || operationRef.current !== operation || closedRef.current) {
      await sendMessage({ type: "lease.release", lease_id: claimed.lease_id }).catch(() => {});
      throw new Error("The cleanup was cancelled before the selected tab was ready.");
    }
    operation.lease = claimed;
    leaseRef.current = claimed;
    setPreview(undefined);
    setTab(claimed.tab);
    return claimed;
  }

  async function dispatchCleanup(input: CleanupInput, context: ToolContext): Promise<unknown> {
    if (context.signal.aborted) throw context.signal.reason;
    const operation = operationRef.current;
    if (!operation || operation.cancelled) {
      throw new Error("The current turn is no longer active.");
    }
    if (input.action === "list_tabs") {
      setActivity("Checking open tabs");
      const windowId = await (operation.windowId ??= currentPanelWindowId());
      const continuation = input.cursor === undefined ? undefined : operation.tabCursors?.get(input.cursor);
      if (input.cursor !== undefined && !continuation) {
        throw new Error("That open-tab cursor expired. List tabs again from the beginning.");
      }
      if (input.cursor === undefined) {
        operation.catalogTabRefs = new Set();
        operation.tabCursors = new Map();
      }
      operation.catalogTabRefs ??= new Set();
      const tabCursors = operation.tabCursors ??= new Map();
      const pending = listOpenPageTabs(windowId, continuation?.offset ?? 0, continuation?.catalogId);
      (operation.snapshots ??= []).push(pending);
      const selection = await pending;
      if (context.signal.aborted) throw context.signal.reason;
      if (operation.cancelled || operationRef.current !== operation) {
        throw new Error("The current turn is cancelling.");
      }
      for (const { tab_ref: tabRef } of selection.tabs) operation.catalogTabRefs.add(tabRef);
      let nextCursor: string | undefined;
      if (selection.next_offset !== undefined) {
        if (!selection.snapshot_id) throw new Error("The open-tab catalog did not return an identity.");
        nextCursor = crypto.randomUUID();
        tabCursors.set(nextCursor, {
          catalogId: selection.snapshot_id,
          offset: selection.next_offset,
        });
      }
      setActivity("Thinking");
      return {
        ...(selection.default_tab_ref ? { default_tab_ref: selection.default_tab_ref } : {}),
        ...(nextCursor ? { next_cursor: nextCursor } : {}),
        tabs: selection.tabs,
      };
    }
    const requestedTabRef = input.action === "inspect" ? input.tab_ref : undefined;
    if (operation.ready && requestedTabRef && requestedTabRef !== operation.targetTabRef) {
      throw new Error("This turn is already attached to another exact tab. Start a new request to switch tabs.");
    }
    operation.ready ??= claimSelectedPage(operation, requestedTabRef);
    const current = await operation.ready;
    if (context.signal.aborted) throw context.signal.reason;
    if (operation.cancelled || operationRef.current !== operation) {
      throw new Error("The current turn is cancelling.");
    }
    setActivity(cleanupActivity(input));
    const requestId = crypto.randomUUID();
    const cancel = () => {
      void chrome.runtime.sendMessage({ type: "page.cancel", request_id: requestId });
    };
    context.signal.addEventListener("abort", cancel, { once: true });
    try {
      const response = await sendMessage({
        type: "page.cleanup",
        lease_id: current.lease_id,
        request_id: requestId,
        input,
      });
      if (context.signal.aborted) {
        const result = asRecord(response);
        if (result.previewed === true) {
          await sendMessage({ type: "preview.revert", lease_id: current.lease_id }).catch(() => {});
        }
        throw context.signal.reason;
      }
      setActivity("Thinking");
      return response;
    } finally {
      context.signal.removeEventListener("abort", cancel);
    }
  }

  function startPanelTurn(source: Agent, input: string): AgentTurn {
    if (transitionRef.current) {
      throw new Error("Wait for the current conversation to finish opening.");
    }
    if (operationRef.current) {
      throw new Error("The current turn is still finishing. Stop it before starting another.");
    }
    setError("");
    setKept("");
    setActivity("Thinking");
    const operation: ActiveOperation = {
      cancelled: false,
      controller: new AbortController(),
    };
    operationRef.current = operation;
    setOperationActive(true);
    let inner: AgentTurn;
    try {
      inner = source.turn.prompt({ input });
      recordConversationActivity(input);
    } catch (cause) {
      operation.cancelled = true;
      operation.controller.abort(cause);
      if (operationRef.current === operation) operationRef.current = undefined;
      void releasePageSelections(operation);
      setOperationActive(false);
      setActivity(undefined);
      throw cause;
    }
    let resultPromise: Promise<AgentTurnResult> | undefined;
    const wrapped: AgentTurn = Object.freeze({
      ...(inner.historyEntryId ? { historyEntryId: inner.historyEntryId } : {}),
      steer: (options) => inner.steer(options),
      cancel: async () => {
        operation.cancelled = true;
        operation.controller.abort(new Error("The cleanup was cancelled."));
        setActivity("Stopping");
        return inner.cancel();
      },
      result: () => {
        resultPromise ??= finishPanelTurn(operation, inner);
        return resultPromise;
      },
      dispose: () => inner.dispose(),
    });
    operation.turn = wrapped;
    return wrapped;
  }

  async function finishPanelTurn(operation: ActiveOperation, turn: AgentTurn): Promise<AgentTurnResult> {
    let lease: PageLease | undefined;
    try {
      const result = await turn.result();
      lease = operation.lease;
      if (lease && (operation.cancelled || operationRef.current !== operation)) {
        await revertFailedTurnPreview(lease);
      } else if (lease) {
        try {
          setPreview(await sendMessage<PreviewInfo | undefined>({
            type: "preview.info",
            lease_id: lease.lease_id,
          }));
        } catch (cause) {
          setError(errorMessage(cause));
        }
      }
      return result;
    } catch (cause) {
      lease ??= operation.lease;
      if (lease) await revertFailedTurnPreview(lease);
      throw cause;
    } finally {
      await releasePageSelections(operation).catch(() => {});
      if (operationRef.current === operation) {
        operationRef.current = undefined;
        setOperationActive(false);
        setActivity(undefined);
      }
    }
  }

  async function revertFailedTurnPreview(lease: PageLease): Promise<void> {
    try {
      await sendMessage({ type: "preview.revert", lease_id: lease.lease_id });
      setPreview(undefined);
    } catch (revertCause) {
      try {
        setPreview(await sendMessage<PreviewInfo | undefined>({
          type: "preview.info",
          lease_id: lease.lease_id,
        }));
      } catch {
        setPreview(undefined);
      }
      setError(`The cleanup stopped, but its preview could not be reverted. ${errorMessage(revertCause)}`);
    }
  }

  async function connect(): Promise<void> {
    if (transitionRef.current || operationRef.current) return;
    const targetId = conversationIdRef.current;
    const version = ++transitionVersionRef.current;
    transitionRef.current = true;
    setConnecting(true);
    setError("");
    try {
      const connected = await connectNanocodex(targetId, conversationIdentity(targetId) ?? undefined);
      if (transitionVersionRef.current !== version || conversationIdRef.current !== targetId) return;
      retainConversation(targetId, connected);
      setConnection(connected);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      if (transitionVersionRef.current === version) {
        transitionRef.current = false;
        setConnecting(false);
      }
    }
  }

  async function disconnect(): Promise<void> {
    if (operationRef.current) {
      setError("Stop the active turn and wait for it to finish before disconnecting.");
      return;
    }
    if (transitionRef.current) return;
    const version = ++transitionVersionRef.current;
    transitionRef.current = true;
    setError("");
    try {
      connectionRef.current = undefined;
      setConnection(undefined);
      await closePanelRuntime();
      closingRef.current = undefined;
      setAgentError(undefined);
      setAgentOpening(false);
      setTab(undefined);
      setPreview(undefined);
      setCookieCapture(undefined);
      setCookieNotice("");
      await disconnectNanocodex(conversationIdRef.current);
    } catch (cause) {
      setError(`Disconnected locally. ${errorMessage(cause)}`);
    } finally {
      if (transitionVersionRef.current === version) transitionRef.current = false;
    }
  }

  async function revert(): Promise<void> {
    const current = leaseRef.current;
    if (!current) return;
    try {
      await sendMessage({ type: "preview.revert", lease_id: current.lease_id });
      setPreview(undefined);
      setKept("");
    } catch (cause) {
      setError(errorMessage(cause));
    }
  }

  async function keep(): Promise<void> {
    const current = leaseRef.current;
    if (!preview || !current) return;
    setError("");
    try {
      const granted = await chrome.permissions.contains({ origins: [preview.permission] })
        || await chrome.permissions.request({ origins: [preview.permission] });
      if (!granted) {
        setError(`Site access was not granted for ${preview.origin}.`);
        return;
      }
      const response = await sendMessage<{ name?: string }>({
        type: "recipe.keep",
        lease_id: current.lease_id,
        origin: preview.origin,
      });
      setKept(response.name ?? preview.recipe.name);
      setPreview(undefined);
      await refreshSaved();
    } catch (cause) {
      setError(errorMessage(cause));
    }
  }

  async function forget(origin: string): Promise<void> {
    setError("");
    try {
      await sendMessage({ type: "recipe.forget", origin });
      setSaved((current) => current.filter((entry) => entry.origin !== origin));
      setKept("");
    } catch (cause) {
      setError(errorMessage(cause));
    }
  }

  async function claimActivePageForCookieSync(): Promise<PageLease> {
    const windowId = await currentPanelWindowId();
    const selection = await selectedPageSelection(windowId);
    try {
      const selectionId = selection.default_tab_ref;
      if (!selectionId || !selection.tabs.some(({ tab_ref }) => tab_ref === selectionId)) {
        throw new Error("No active HTTP or HTTPS tab is available in this side panel's window.");
      }
      const previous = leaseRef.current;
      const claimed = await sendMessage<PageLease>({
        type: "page.claim",
        selection_id: selectionId,
        ...(previous ? { previous_lease_id: previous.lease_id } : {}),
      });
      leaseRef.current = claimed;
      setTab(claimed.tab);
      setPreview(undefined);
      setCookieCapture(undefined);
      return claimed;
    } finally {
      if (selection.snapshot_id) {
        await sendMessage({
          type: "page.selection.release",
          snapshot_id: selection.snapshot_id,
        }).catch(() => {});
      }
    }
  }

  async function captureCurrentSiteCookies(): Promise<void> {
    const activeConnection = connectionRef.current;
    if (!activeConnection || operationRef.current || cookieBusy || preview) return;
    setCookieBusy(true);
    setCookieNotice("");
    setError("");
    let captureToRelease: Readonly<{ capture_id: string; lease_id: string }> | undefined;
    try {
      const granted = await chrome.permissions.request({ permissions: ["cookies"] });
      if (!granted) throw new Error("Cookie access was not granted.");
      const current = await claimActivePageForCookieSync();
      const captured = await sendMessage<CookieCaptureHandle>({
        type: "cookie.capture",
        lease_id: current.lease_id,
      });
      captureToRelease = captured;
      const exported = await sendMessage<{ jar_id: string; jar: BrowserCookieJarV1 }>({
        type: "cookie.sync.export",
        capture_id: captured.capture_id,
        lease_id: current.lease_id,
      });
      const transport = cookieSyncTransport(activeConnection);
      const existing = (await transport.list({
        origin: captured.origin,
        profile_id: captured.profile_id,
        store_id: captured.store_id,
      }))[0];
      const metadata = await transport.replace(existing?.id ?? exported.jar_id, {
        ...exported.jar,
        revision: existing?.revision ?? 0,
      });
      setCookieCapture({
        jar_id: metadata.id,
        lease_id: current.lease_id,
        origin: metadata.origin,
        profile_id: metadata.profile_id,
        store_id: metadata.store_id,
        cookie_count: metadata.cookie_count,
        revision: metadata.revision,
      });
      setCookieNotice(`Synced ${metadata.cookie_count} cookie${metadata.cookie_count === 1 ? "" : "s"} for ${metadata.origin}. Values were sent only through authenticated Connect.`);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      if (captureToRelease) {
        await sendMessage({
          type: "cookie.capture.release",
          capture_id: captureToRelease.capture_id,
          lease_id: captureToRelease.lease_id,
        }).catch(() => {});
      }
      setCookieBusy(false);
    }
  }

  async function restoreCurrentSiteCookies(): Promise<void> {
    const activeConnection = connectionRef.current;
    if (!activeConnection || cookieBusy) return;
    setCookieBusy(true);
    setCookieNotice("");
    setError("");
    let confirmation: CookieRestoreConfirmation | undefined;
    try {
      const granted = await chrome.permissions.request({ permissions: ["cookies"] });
      if (!granted) throw new Error("Cookie access was not granted.");
      let current = leaseRef.current;
      let captured = cookieCapture;
      if (!current || !captured || captured.lease_id !== current.lease_id) {
        current = await claimActivePageForCookieSync();
        const probe = await sendMessage<CookieCaptureHandle>({
          type: "cookie.capture",
          lease_id: current.lease_id,
        });
        await sendMessage({
          type: "cookie.capture.release",
          capture_id: probe.capture_id,
          lease_id: current.lease_id,
        }).catch(() => {});
        const saved = (await cookieSyncTransport(activeConnection).list({
          origin: probe.origin,
          profile_id: probe.profile_id,
          store_id: probe.store_id,
        }))[0];
        if (!saved) throw new Error(`No saved cookies exist for ${probe.origin} in this browser profile.`);
        captured = {
          jar_id: saved.id,
          lease_id: current.lease_id,
          origin: saved.origin,
          profile_id: saved.profile_id,
          store_id: saved.store_id,
          cookie_count: saved.cookie_count,
          revision: saved.revision,
        };
        setCookieCapture(captured);
      }
      const jar = await cookieSyncTransport(activeConnection).materialize(captured.jar_id, {
        origin: captured.origin,
        profile_id: captured.profile_id,
        store_id: captured.store_id,
      });
      const staged = await sendMessage<CookieCaptureHandle>({
        type: "cookie.restore.stage",
        lease_id: current.lease_id,
        jar,
      });
      confirmation = await sendMessage<CookieRestoreConfirmation>({
        type: "cookie.restore.prepare",
        capture_id: staged.capture_id,
        lease_id: current.lease_id,
      });
      const confirmed = window.confirm(
        `Replace cookies currently applicable to ${confirmation.origin} with the ${confirmation.cookie_count} in-memory captured cookie${confirmation.cookie_count === 1 ? "" : "s"}? This can sign you out or replace newer site state.`,
      );
      if (!confirmed) {
        await sendMessage({
          type: "cookie.restore.cancel",
          confirmation_id: confirmation.confirmation_id,
        }).catch(() => {});
        return;
      }
      const restored = await sendMessage<{ origin: string; cookie_count: number }>({
        type: "cookie.restore.apply",
        confirmation_id: confirmation.confirmation_id,
        confirmed: true,
      });
      setCookieNotice(`Restored ${restored.cookie_count} cookie${restored.cookie_count === 1 ? "" : "s"} for ${restored.origin}.`);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setCookieBusy(false);
    }
  }

  function recordConversationActivity(input: string): void {
    const now = Date.now();
    setConversations((current) => persistConversations(current.map((conversation) => (
      conversation.id === conversationId
        ? {
          ...conversation,
          title: (conversation.turnCount ?? 0) === 0 ? conversationTitle(input) : conversation.title,
          turnCount: (conversation.turnCount ?? 0) + 1,
          updatedAt: now,
        }
        : conversation
    )).sort((left, right) => (right.updatedAt ?? 0) - (left.updatedAt ?? 0))));
  }

  function retainConversation(id: string, connected: NanocodexConnection): void {
    setConversations((current) => {
      const existing = current.find((conversation) => conversation.id === id);
      const next = existing
        ? current
        : [{ id, title: "New conversation", updatedAt: Date.now(), turnCount: 0 }, ...current];
      persistConversationIdentity(id, connected);
      return persistConversations(next);
    });
  }

  async function activateConversation(id: string, connected?: NanocodexConnection): Promise<void> {
    if (operationRef.current || transitionRef.current
      || (id === conversationId && connection)) return;
    const version = ++transitionVersionRef.current;
    transitionRef.current = true;
    setConversationPending(true);
    setError("");
    try {
      const next = connected ?? await reconnectNanocodex(id, conversationIdentity(id) ?? undefined);
      if (transitionVersionRef.current !== version) return;
      if (!next) throw new Error("Reconnect this conversation to continue.");
      const expected = conversationIdentity(id);
      if (expected && (expected.agentId !== next.agentId
        || (expected.accountAddress && expected.accountAddress.toLowerCase() !== next.accountAddress.toLowerCase()))) {
        throw new Error("The retained conversation authorization no longer matches this thread.");
      }
      connectionRef.current = undefined;
      setConnection(undefined);
      await closePanelRuntime();
      closingRef.current = undefined;
      setAgentError(undefined);
      setTab(undefined);
      setPreview(undefined);
      conversationIdRef.current = id;
      setConversationId(id);
      persistSelectedConversation(id);
      persistConversationIdentity(id, next);
      setConnection(next);
      setRailOpen(false);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      if (transitionVersionRef.current === version) {
        transitionRef.current = false;
        setConversationPending(false);
      }
    }
  }

  async function createConversation(): Promise<void> {
    if (operationRef.current || transitionRef.current) return;
    const id = createConversationId();
    const version = ++transitionVersionRef.current;
    transitionRef.current = true;
    setConversationPending(true);
    setError("");
    try {
      const connected = await connectNanocodex(id);
      if (transitionVersionRef.current !== version) return;
      retainConversation(id, connected);
      connectionRef.current = undefined;
      setConnection(undefined);
      await closePanelRuntime();
      closingRef.current = undefined;
      setAgentError(undefined);
      setTab(undefined);
      setPreview(undefined);
      conversationIdRef.current = id;
      setConversationId(id);
      persistSelectedConversation(id);
      persistConversationIdentity(id, connected);
      setConnection(connected);
      setRailOpen(false);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      if (transitionVersionRef.current === version) {
        transitionRef.current = false;
        setConversationPending(false);
      }
    }
  }

  const panelAgent = useMemo<Agent | undefined>(() => {
    if (!agentSource) return undefined;
    return Object.freeze({
      sessionId: agentSource.sessionId,
      events: agentSource.events,
      turn: Object.freeze({
        prompt: ({ input }: Readonly<{ input: string }>) => startPanelTurn(agentSource, input),
      }),
    });
  }, [agentSource]);

  const status = activity
    ?? (agentError
      ? "Agent unavailable"
      : agentSource
        ? "Ready"
        : agentOpening
          ? "Connecting agent"
          : connection
            ? "Connected"
            : "Not connected");
  const agentStatus: AgentStatus = agentError
    ? "error"
    : agentSource
      ? "ready"
      : agentOpening || restoring || conversationPending
        ? "starting"
        : "idle";

  return (
    <main className="app-shell">
      <header className="app-header">
        <div className="identity">
          <span className="mark" aria-hidden="true">N</span>
          <div><h1>Nanocodex</h1><p>Shape this tab. Keep only what you approve.</p></div>
        </div>
        <span className={`agent-state${activity ? " is-active" : ""}`} role="status">
          <span aria-hidden="true" />{status}
        </span>
      </header>

      <section className="connection-bar" aria-label="Nanocodex account">
        {connection ? <>
          <div><strong>Nanocodex Connect</strong><code title={connection.accountAddress}>{shortAddress(connection.accountAddress)}</code></div>
          <div className="connection-actions">
            {agentError ? <button type="button" onClick={() => void ensurePageAgent(connection).catch(() => {})}>Retry agent</button> : null}
            <button type="button" disabled={operationActive || connecting} onClick={() => void disconnect()}>Disconnect</button>
          </div>
        </> : <>
          <p>Connect your passkey account to chat with your durable page agent.</p>
          <button className="primary" type="button" disabled={connecting || restoring} onClick={() => void connect()}>Connect Nanocodex</button>
        </>}
      </section>

      {tab ? <div className="site" title={tab.url}><span aria-hidden="true">●</span>{tab.title} · {tab.origin}</div> : null}

      <div className="conversation-workspace">
        <ConversationHistoryRail
          agentStatus={agentStatus}
          conversations={conversations}
          mobileOpen={railOpen}
          pending={conversationPending || connecting || restoring}
          runtime="managed"
          selectedId={conversationId}
          onClose={() => setRailOpen(false)}
          onCreate={() => void createConversation()}
          onOpen={() => setRailOpen(true)}
          onRetry={() => void activateConversation(conversationId)}
          onSelect={(id) => void activateConversation(id)}
        />
        <div className="conversation-main">
          <section className="chat" aria-label="Durable agent chat">
            <AgentTerminalView
              key={conversationId}
              agent={panelAgent}
              agentError={agentError}
              inactiveMessage={({ agentError: currentError }) => currentError ?? (!connection ? "Connect Nanocodex to start." : "")}
              maxEntries={160}
              mode="full"
              onConversationActivity={() => {}}
              onTerminalEvent={(event) => observeTerminalEvent(event, setActivity)}
              onStateChange={observeTerminalState}
              promptIntent="steer"
              retryAgent={() => {
                if (connection) void ensurePageAgent(connection).catch(() => {});
              }}
              showToolCalls
              welcome="Chat with your durable Nanocodex agent. Ask it anything, or ask it to inspect and reshape the selected tab."
            />
          </section>
        </div>
      </div>

      {preview ? (
        <section className="preview" aria-label="Active preview">
          <div><span className="eyebrow">Preview ready</span><h2>{preview.recipe.name}</h2><p>Only this tab has changed. Keep it to reapply on {preview.origin}.</p></div>
          <div className="actions"><button className="primary" type="button" onClick={() => void keep()}>Keep for this site</button><button type="button" onClick={() => void revert()}>Revert</button></div>
        </section>
      ) : null}

      <section className="cookie-sync" aria-label="Current-site cookie sync">
        <div>
          <span className="eyebrow">Current site only</span>
          <h2>Cookie sync</h2>
          <p>Capture explicitly syncs only this leased site through authenticated Connect. Cookie values never enter chat, React state, extension storage, logs, transcripts, or the model.</p>
        </div>
        <div className="actions">
          <button
            type="button"
            disabled={!connection || operationActive || cookieBusy || Boolean(preview)}
            onClick={() => void captureCurrentSiteCookies()}
          >{cookieBusy ? "Working…" : "Capture this site"}</button>
          <button
            className="danger"
            type="button"
            disabled={!connection || operationActive || cookieBusy || Boolean(preview)}
            onClick={() => void restoreCurrentSiteCookies()}
          >Restore saved cookies…</button>
        </div>
      </section>

      {kept ? <p className="notice" role="status">Saved “{kept}” for this site.</p> : null}
      {cookieNotice ? <p className="notice" role="status">{cookieNotice}</p> : null}
      {error ? <p className="error" role="alert">{error}</p> : null}

      <div className="panel-details">
        {saved.length > 0 ? (
          <details>
            <summary>Saved sites <span>{saved.length}</span></summary>
            <div className="saved-list">
              {saved.map((entry) => (
                <div className="saved-site" key={entry.origin}>
                  <div><strong>{entry.recipe.name}</strong><p>{entry.origin}</p></div>
                  <button type="button" onClick={() => void forget(entry.origin)}>Forget</button>
                </div>
              ))}
            </div>
          </details>
        ) : null}
        <details>
          <summary>Privacy and tab access</summary>
          <p>The agent can list safe titles and origins for loaded HTTP(S) tabs when you name another tab. Without a target, it uses the active web tab when the page tool runs and never falls back to an older tab. Ordinary chat does not read tabs. Page contents are inspected only when the cleanup tool runs, and every change stays bound to one exact document. Cookie access is optional and requested only when you press Capture this site; values remain isolated in background memory and are never shown to the agent. Your signed grant allows replies, actions, conversation history, full run traces, and explicit browser-cookie sync, but never spending or contracts.</p>
        </details>
      </div>
    </main>
  );
}

async function sendMessage<Result = unknown>(message: unknown): Promise<Result> {
  const response = await chrome.runtime.sendMessage(message) as Result & { error?: string };
  if (response && typeof response === "object" && typeof response.error === "string") throw new Error(response.error);
  return response;
}

async function selectedPageSelection(windowId: number): Promise<PageSelectionSnapshot> {
  try {
    return await sendMessage<PageSelectionSnapshot>({ type: "page.selection", window_id: windowId });
  } catch {
    return { tabs: [] };
  }
}

function listOpenPageTabs(
  windowId: number,
  offset: number,
  catalogId?: string,
): Promise<PageSelectionSnapshot> {
  return sendMessage<PageSelectionSnapshot>({
    type: "page.tabs",
    window_id: windowId,
    offset,
    ...(catalogId ? { catalog_id: catalogId } : {}),
  });
}

async function currentPanelWindowId(): Promise<number> {
  const current = await chrome.windows.getCurrent();
  if (!Number.isInteger(current.id)) throw new Error("The side panel's browser window is unavailable.");
  return current.id as number;
}

async function releasePageSelections(operation: ActiveOperation): Promise<void> {
  const snapshots = await Promise.all([
    operation.selection?.catch(() => undefined),
    ...(operation.snapshots ?? []).map((snapshot) => snapshot.catch(() => undefined)),
  ]);
  const ids = new Set(snapshots.flatMap((snapshot) => snapshot?.snapshot_id ? [snapshot.snapshot_id] : []));
  await Promise.all([...ids].map((snapshotId) => sendMessage({
    type: "page.selection.release",
    snapshot_id: snapshotId,
  }).catch(() => {})));
}

function observeTerminalEvent(event: AgentControllerEvent, setActivity: (activity: string | undefined) => void): void {
  if (event.type === "prompt.accepted") setActivity("Thinking");
  else if (event.type === "prompt.completed" || event.type === "prompt.failed" || event.type === "prompt.cancelled") setActivity(undefined);
  else if (event.type === "agent.event" && event.event && typeof event.event === "object") {
    const type = (event.event as { type?: unknown }).type;
    if (type === "assistant.delta") setActivity("Writing");
    else if (type === "tool.call") setActivity("Working on this tab");
    else if (type === "run.started") setActivity("Thinking");
    else if (type === "assistant.message" || type === "run.completed" || type === "run.failed" || type === "run.cancelled") setActivity(undefined);
  }
}

function observeTerminalState(_state: AgentTerminalState): void {}

function cleanupActivity(input: CleanupInput): string {
  if (input.action === "list_tabs") return "Checking open tabs";
  if (input.action === "inspect") return "Reading this tab";
  if (input.action === "preview") return "Applying preview";
  return "Reverting preview";
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function asRecord(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
}

function shortAddress(address: string): string {
  return `${address.slice(0, 6)}…${address.slice(-4)}`;
}

const CONVERSATIONS_KEY = "nanocodex.chrome.conversations.v1";
const SELECTED_CONVERSATION_KEY = "nanocodex.chrome.selected-conversation.v1";

function loadConversations(): readonly ConversationSummary[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(CONVERSATIONS_KEY) ?? "null") as unknown;
    if (!Array.isArray(parsed)) return [legacyConversation()];
    const conversations = parsed.flatMap((item): ConversationSummary[] => {
      if (!item || typeof item !== "object" || Array.isArray(item)) return [];
      const value = item as Partial<ConversationSummary>;
      if (typeof value.id !== "string" || !isConversationId(value.id)
        || typeof value.title !== "string" || value.title.length === 0 || value.title.length > 80
        || (value.updatedAt !== undefined && !Number.isSafeInteger(value.updatedAt))
        || (value.turnCount !== undefined && (!Number.isSafeInteger(value.turnCount) || value.turnCount < 0))) {
        return [];
      }
      return [{
        id: value.id,
        title: value.title,
        ...(value.updatedAt === undefined ? {} : { updatedAt: value.updatedAt }),
        ...(value.turnCount === undefined ? {} : { turnCount: value.turnCount }),
      }];
    });
    return conversations.length > 0 ? conversations : [legacyConversation()];
  } catch {
    return [legacyConversation()];
  }
}

function loadSelectedConversation(): string {
  try {
    const selected = localStorage.getItem(SELECTED_CONVERSATION_KEY);
    if (selected && loadConversations().some(({ id }) => id === selected)) return selected;
  } catch {}
  return loadConversations()[0]?.id ?? LEGACY_CONVERSATION_ID;
}

function persistConversations(conversations: readonly ConversationSummary[]): readonly ConversationSummary[] {
  const frozen = Object.freeze([...conversations]);
  try { localStorage.setItem(CONVERSATIONS_KEY, JSON.stringify(frozen)); } catch {}
  return frozen;
}

function persistSelectedConversation(id: string): void {
  try { localStorage.setItem(SELECTED_CONVERSATION_KEY, id); } catch {}
}

function persistConversationIdentity(id: string, connection: NanocodexConnection): void {
  try {
    localStorage.setItem(`${CONVERSATIONS_KEY}:identity:${id}`, JSON.stringify({
      accountAddress: connection.accountAddress,
      agentId: connection.agentId,
    }));
    localStorage.setItem(`${CONVERSATIONS_KEY}:agent:${id}`, connection.agentId);
  } catch {}
}

function conversationIdentity(id: string): Readonly<{ accountAddress?: string; agentId: string }> | null {
  try {
    const retained = localStorage.getItem(`${CONVERSATIONS_KEY}:identity:${id}`);
    if (retained) {
      const parsed = JSON.parse(retained) as unknown;
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        const value = parsed as { accountAddress?: unknown; agentId?: unknown };
        if (typeof value.agentId === "string" && isManagedAgentId(value.agentId)
          && typeof value.accountAddress === "string" && /^0x[0-9a-f]{40}$/i.test(value.accountAddress)) {
          return { accountAddress: value.accountAddress, agentId: value.agentId };
        }
      }
    }
    const legacyAgent = localStorage.getItem(`${CONVERSATIONS_KEY}:agent:${id}`);
    return legacyAgent && isManagedAgentId(legacyAgent) ? { agentId: legacyAgent } : null;
  } catch {
    return null;
  }
}

function legacyConversation(): ConversationSummary {
  return Object.freeze({ id: LEGACY_CONVERSATION_ID, title: "New conversation" });
}

function conversationTitle(input: string): string {
  const text = input.replace(/\s+/g, " ").trim();
  if (!text) return "New conversation";
  return text.length > 56 ? `${text.slice(0, 55).trimEnd()}…` : text;
}
