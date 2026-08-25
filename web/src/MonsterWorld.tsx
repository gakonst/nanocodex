import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import type { AgentStatus } from "./agentTerminalTypes";
import {
  AgentSessionBar,
  type ModelSessionStatus,
  type CredentialSource,
} from "./modelSession";
import {
  RESIDENT_IDS,
  VOICE_LEVELS,
  VOICE_RADIUS,
  WORLD_PROTOCOL,
  isResidentId,
  isWorldAgentMessage,
  worldObservationCallId,
  type Direction,
  type ResidentId,
  type VoiceLevel,
  type WorldAgentCommand,
  type WorldAgentMessage,
  type WorldUsage,
} from "./monsterWorldProtocol";
import {
  WORLD_FORMATION_PROMPTS,
  type WorldFormationPrompt,
} from "./monsterWorldFormations";
import {
  drawMonsterWorld,
  loadWorldAssets,
  worldAssetSource,
  type WorldAssets,
} from "./monsterWorldRenderer";
import {
  WORLD_PIXEL_HEIGHT,
  WORLD_PIXEL_WIDTH,
  sceneLabel,
  viewportToWorld,
} from "./monsterWorldMap";
import {
  WORLD_SAVE_KEY,
  MAX_RESIDENT_COUNT,
  activeResidentCount,
  actorWorldPosition,
  applyWorldToolAction,
  completeResidentInstruction,
  createWorldState,
  formatWorldTime,
  hasUnansweredGuildCall,
  hasUnansweredPlayerOrder,
  isGuildRelayActive,
  liveAgentIdsInWorld,
  movePlayer,
  observationFor,
  playerInteract,
  playerSpeak,
  requestResidentExit,
  rejectedWorldToolResult,
  residentAtWorldPoint,
  serializeWorldState,
  setPopulationTarget,
  setWorldAgentsOnline,
  updateWorld,
  worldToolResultAtDecisionBoundary,
  worldCameraForState,
  type WorldActor,
  type WorldState,
  type WorldToolAction,
} from "./monsterWorldSimulation";
import "./MonsterWorld.css";

const WORLD_RENDER_INTERVAL_MS = 50;

type RuntimeStatus = "offline" | "starting" | "ready" | "stopping" | "error";

type UsageTotals = {
  modelTurns: number;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  estimatedUsd: number;
};

type PendingResidentRequest = {
  requestId: string;
  agentId: ResidentId;
  residentIds: readonly ResidentId[];
  callId?: number;
  cancelled: boolean;
};

const emptyUsage: UsageTotals = {
  modelTurns: 0,
  inputTokens: 0,
  outputTokens: 0,
  totalTokens: 0,
  estimatedUsd: 0,
};

export function MonsterWorld() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const assetsRef = useRef<WorldAssets | undefined>(undefined);
  const worldRef = useRef<WorldState | undefined>(undefined);
  const workerRef = useRef<Worker | undefined>(undefined);
  const renderRequestRef = useRef<(() => void) | undefined>(undefined);
  const heldDirections = useRef(new Set<Direction>());
  const pendingRequests = useRef(new Map<string, PendingResidentRequest>());
  const pendingWorldActions = useRef(new Map<string, WorldToolAction>());
  const nextCoordinationAt = useRef(0);
  const usageRef = useRef<UsageTotals>(emptyUsage);
  const runtimeStatusRef = useRef<RuntimeStatus>("offline");
  const [revision, setRevision] = useState(0);
  const [paused, setPaused] = useState(false);
  const [reducedMotion, setReducedMotion] = useState(false);
  const [draft, setDraft] = useState("");
  const [voiceLevel, setVoiceLevel] = useState<VoiceLevel>("call");
  const [selectedResident, setSelectedResident] = useState<ResidentId>("cinder");
  const [speechNotice, setSpeechNotice] = useState<string>();
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus>("offline");
  const [agentError, setAgentError] = useState<string>();
  const [, setAuthStatus] = useState<ModelSessionStatus>();
  const [credentialSource, setCredentialSource] = useState<CredentialSource>();
  const [usage, setUsage] = useState<UsageTotals>(emptyUsage);
  const [assetError, setAssetError] = useState<Error>();
  const capabilityError = useMemo(worldCapabilityError, []);

  if (!worldRef.current) {
    worldRef.current = createWorldState(readSavedWorld());
  }
  const world = worldRef.current;

  const requestWorldRender = useCallback(() => {
    renderRequestRef.current?.();
  }, []);

  const invalidateWorld = useCallback(() => {
    requestWorldRender();
    setRevision((value) => value + 1);
  }, [requestWorldRender]);

  useEffect(() => {
    runtimeStatusRef.current = runtimeStatus;
  }, [runtimeStatus]);

  useEffect(() => {
    const query = window.matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => setReducedMotion(query.matches);
    update();
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);

  useEffect(() => {
    let current = true;
    void loadWorldAssets().then(
      (assets) => {
        if (current) {
          assetsRef.current = assets;
          requestWorldRender();
        }
      },
      (error: unknown) => {
        if (!current) return;
        setAssetError(
          error instanceof Error
            ? error
            : new Error("World assets could not be loaded"),
        );
      },
    );
    return () => {
      current = false;
    };
  }, [requestWorldRender]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    if (!canvas || !context) return;
    context.imageSmoothingEnabled = false;
    let disposed = false;
    let dirty = true;
    let frame: number | undefined;
    let previous = performance.now();
    let nextCanvasDraw = previous;
    let nextUiUpdate = previous + 240;
    let nextSave = previous + 4_000;

    const cancelFrame = () => {
      if (frame === undefined) return;
      cancelAnimationFrame(frame);
      frame = undefined;
    };

    const scheduleFrame = () => {
      if (
        disposed
        || frame !== undefined
        || document.visibilityState !== "visible"
      ) return;
      frame = requestAnimationFrame(render);
    };

    const requestRender = () => {
      dirty = true;
      scheduleFrame();
    };

    const render = (now: number) => {
      frame = undefined;
      if (disposed || document.visibilityState !== "visible") return;
      const delta = now - previous;
      previous = now;
      const activeWorld = worldRef.current;
      if (activeWorld && !paused) {
        const direction = heldDirections.current.values().next().value;
        if (direction) movePlayer(activeWorld, direction);
        updateWorld(activeWorld, delta);
        const worker = workerRef.current;
        if (worker) {
          for (const [actionId, pending] of pendingWorldActions.current) {
            const result = worldToolResultAtDecisionBoundary(activeWorld, pending);
            if (!result) continue;
            pendingWorldActions.current.delete(actionId);
            worker.postMessage({
              protocol: WORLD_PROTOCOL,
              type: "action_result",
              actionId,
              requestId: pending.requestId,
              agentId: pending.agentId,
              result,
            } satisfies WorldAgentCommand);
          }
        }
        if (now >= nextCanvasDraw) dirty = true;
      }
      if (activeWorld && dirty) {
        drawMonsterWorld(context, activeWorld, assetsRef.current, {
          reducedMotion,
          pixelRatio: window.devicePixelRatio,
        });
        dirty = false;
        nextCanvasDraw = now + WORLD_RENDER_INTERVAL_MS;
      }
      if (activeWorld && !paused) {
        if (now >= nextUiUpdate) {
          nextUiUpdate = now + 240;
          setRevision((value) => value + 1);
        }
        if (now >= nextSave) {
          nextSave = now + 4_000;
          saveWorld(activeWorld);
        }
      }
      if (activeWorld && !paused) scheduleFrame();
    };

    const handleVisibility = () => {
      previous = performance.now();
      if (document.visibilityState !== "visible") {
        cancelFrame();
        return;
      }
      requestRender();
    };
    const resizeObserver = new ResizeObserver(requestRender);

    renderRequestRef.current = requestRender;
    document.addEventListener("visibilitychange", handleVisibility);
    resizeObserver.observe(canvas);
    scheduleFrame();

    return () => {
      disposed = true;
      if (renderRequestRef.current === requestRender) {
        renderRequestRef.current = undefined;
      }
      document.removeEventListener("visibilitychange", handleVisibility);
      resizeObserver.disconnect();
      cancelFrame();
      if (worldRef.current) saveWorld(worldRef.current);
    };
  }, [paused, reducedMotion]);

  const stopAgents = useCallback(() => {
    const worker = workerRef.current;
    if (worker && runtimeStatusRef.current !== "stopping") {
      worker.postMessage({ protocol: WORLD_PROTOCOL, type: "shutdown" });
      setRuntimeStatus("stopping");
      runtimeStatusRef.current = "stopping";
      window.setTimeout(() => {
        if (workerRef.current !== worker) return;
        workerRef.current = undefined;
        worker.terminate();
        setRuntimeStatus("error");
        runtimeStatusRef.current = "error";
        setAgentError("The World task tree did not finish shutting down. Retry the agents.");
        invalidateWorld();
      }, 35_000);
    }
    pendingRequests.current.clear();
    pendingWorldActions.current.clear();
    if (worldRef.current) setWorldAgentsOnline(worldRef.current, false);
    invalidateWorld();
  }, [invalidateWorld]);

  const receiveAgentMessage = useCallback((worker: Worker, message: WorldAgentMessage) => {
    if (workerRef.current !== worker) return;
    if (message.type === "status") {
      if (message.status === "ready") {
        setRuntimeStatus("ready");
        runtimeStatusRef.current = "ready";
        setAgentError(undefined);
        if (worldRef.current) setWorldAgentsOnline(worldRef.current, true);
      } else if (message.status === "error") {
        const failureMessage = message.message ?? "The world agents could not connect.";
        setRuntimeStatus("error");
        runtimeStatusRef.current = "error";
        setAgentError(failureMessage);
        if (worldRef.current) setWorldAgentsOnline(worldRef.current, false);
      } else if (message.status === "stopped") {
        workerRef.current = undefined;
        worker.terminate();
        pendingRequests.current.clear();
        setRuntimeStatus("offline");
        runtimeStatusRef.current = "offline";
        if (worldRef.current) setWorldAgentsOnline(worldRef.current, false);
      }
      invalidateWorld();
      return;
    }
    if (message.type === "action") {
      const request = pendingRequests.current.get(message.requestId);
      const activeWorld = worldRef.current;
      if (!activeWorld) return;
      if (!request || !request.residentIds.includes(message.agentId) || request.cancelled) {
        worker.postMessage({
          protocol: WORLD_PROTOCOL,
          type: "action_result",
          actionId: message.actionId,
          requestId: message.requestId,
          agentId: message.agentId,
          result: rejectedWorldToolResult(
            activeWorld,
            message.agentId,
            message.action,
            "superseded",
            "A newer World call replaced this action.",
          ),
        } satisfies WorldAgentCommand);
        return;
      }
      const application = applyWorldToolAction(activeWorld, {
        actionId: message.actionId,
        requestId: message.requestId,
        agentId: message.agentId,
        ...(message.heardCallId === undefined ? {} : { heardCallId: message.heardCallId }),
        action: message.action,
      });
      if (application.accepted) {
        pendingWorldActions.current.set(message.actionId, application.pending);
      } else {
        worker.postMessage({
          protocol: WORLD_PROTOCOL,
          type: "action_result",
          actionId: message.actionId,
          requestId: message.requestId,
          agentId: message.agentId,
          result: rejectedWorldToolResult(
            activeWorld,
            message.agentId,
            message.action,
            application.reason === "superseded" ? "superseded" : "rejected",
            `The reducer rejected this action: ${application.reason}.`,
          ),
        } satisfies WorldAgentCommand);
      }
      invalidateWorld();
      return;
    }
    if (message.type !== "settled") return;
    const request = pendingRequests.current.get(message.requestId);
    if (!request || request.agentId !== message.agentId) return;
    pendingRequests.current.delete(request.requestId);
    for (const [actionId, pending] of pendingWorldActions.current) {
      if (pending.requestId === request.requestId) pendingWorldActions.current.delete(actionId);
    }
    if (message.usage) commitModelUsage(message.usage, usageRef, setUsage);

    const activeWorld = worldRef.current;
    if (activeWorld && message.outcome === "completed" && !request.cancelled) {
      for (const residentId of request.residentIds) {
        completeResidentInstruction(activeWorld, residentId, request.callId);
      }
      setAgentError(undefined);
      nextCoordinationAt.current = 0;
    } else if (message.outcome === "failed") {
      setAgentError(message.message ?? "The World task tree could not complete this call.");
      nextCoordinationAt.current = performance.now() + 5_000;
    } else {
      nextCoordinationAt.current = 0;
    }
    invalidateWorld();
  }, [invalidateWorld]);

  const startAgents = useCallback(() => {
    if (
      capabilityError
      || credentialSource !== "brokered"
      || runtimeStatusRef.current === "starting"
      || runtimeStatusRef.current === "stopping"
      || runtimeStatusRef.current === "ready"
    ) return;
    workerRef.current?.terminate();
    const worker = new Worker(new URL("./monsterWorldAgent.worker.ts", import.meta.url), {
      type: "module",
      name: "nanocodex-monster-world",
    });
    workerRef.current = worker;
    pendingRequests.current.clear();
    pendingWorldActions.current.clear();
    setAgentError(undefined);
    setRuntimeStatus("starting");
    runtimeStatusRef.current = "starting";
    worker.addEventListener("message", ({ data }: MessageEvent<unknown>) => {
      if (!isWorldAgentMessage(data)) return;
      receiveAgentMessage(worker, data);
    });
    worker.addEventListener("error", (event) => {
      if (workerRef.current !== worker) return;
      workerRef.current = undefined;
      setRuntimeStatus("error");
      runtimeStatusRef.current = "error";
      setAgentError(event.message || "The browser Worker could not start the world agents.");
      pendingRequests.current.clear();
      pendingWorldActions.current.clear();
      if (worldRef.current) setWorldAgentsOnline(worldRef.current, false);
      invalidateWorld();
    });
    worker.postMessage({ protocol: WORLD_PROTOCOL, type: "connect" });
  }, [capabilityError, credentialSource, invalidateWorld, receiveAgentMessage]);

  useEffect(() => {
    if (runtimeStatus !== "ready") return;
    const schedule = () => {
      const activeWorld = worldRef.current;
      const worker = workerRef.current;
      if (
        !activeWorld
        || !worker
        || document.visibilityState !== "visible"
        || paused
      ) return;
      const now = performance.now();
      if (now < nextCoordinationAt.current) return;
      const residentIds = liveAgentIdsInWorld(activeWorld).filter((agentId) =>
        hasUnansweredPlayerOrder(activeWorld, agentId)
        || hasUnansweredGuildCall(activeWorld, agentId));
      const agentId = residentIds[0];
      if (!agentId) return;
      const observation = observationFor(activeWorld, agentId);
      const callId = worldObservationCallId(observation);
      if (callId === undefined) return;
      if ([...pendingRequests.current.values()].some((request) =>
        !request.cancelled && request.callId === callId)) return;
      const request: PendingResidentRequest = {
        requestId: `world-request-${crypto.randomUUID()}`,
        agentId,
        residentIds: Object.freeze(residentIds),
        callId,
        cancelled: false,
      };
      pendingRequests.current.set(request.requestId, request);
      try {
        worker.postMessage({
          protocol: WORLD_PROTOCOL,
          type: "call",
          requestId: request.requestId,
          agentId,
          residentIds: request.residentIds,
          observation,
        } satisfies WorldAgentCommand);
        nextCoordinationAt.current = Number.POSITIVE_INFINITY;
      } catch {
        pendingRequests.current.delete(request.requestId);
        nextCoordinationAt.current = now + 5_000;
      }
      invalidateWorld();
    };
    schedule();
    const timer = window.setInterval(schedule, 500);
    return () => window.clearInterval(timer);
  }, [invalidateWorld, paused, runtimeStatus]);

  useEffect(() => {
    const handleVisibility = () => {
      if (document.visibilityState === "hidden") {
        heldDirections.current.clear();
        stopAgents();
      }
    };
    document.addEventListener("visibilitychange", handleVisibility);
    return () => document.removeEventListener("visibilitychange", handleVisibility);
  }, [stopAgents]);

  useEffect(() => {
    if (credentialSource === null && workerRef.current) stopAgents();
  }, [credentialSource, stopAgents]);

  useEffect(() => () => {
    const worker = workerRef.current;
    workerRef.current = undefined;
    if (worker) {
      worker.postMessage({ protocol: WORLD_PROTOCOL, type: "shutdown" });
      window.setTimeout(() => worker.terminate(), 35_000);
    }
    pendingRequests.current.clear();
    pendingWorldActions.current.clear();
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey || isTypingTarget(event.target)) return;
      if (document.activeElement !== canvasRef.current) return;
      const direction = directionForKey(event.key);
      if (direction) {
        event.preventDefault();
        heldDirections.current.delete(direction);
        heldDirections.current.add(direction);
        requestWorldRender();
        return;
      }
      if (event.repeat) return;
      if (event.code === "Space") {
        event.preventDefault();
        playerInteract(worldRef.current as WorldState);
        invalidateWorld();
        return;
      }
      const voice = voiceLevelForKey(event.key);
      if (voice) {
        event.preventDefault();
        setVoiceLevel(voice);
        return;
      }
      if (event.key.toLowerCase() === "q") {
        event.preventDefault();
        setVoiceLevel((current) => nextVoiceLevel(current));
      }
    };
    const onKeyUp = (event: KeyboardEvent) => {
      const direction = directionForKey(event.key);
      if (direction) heldDirections.current.delete(direction);
    };
    const onWindowBlur = () => heldDirections.current.clear();
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", onWindowBlur);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", onWindowBlur);
    };
  }, [invalidateWorld, requestWorldRender]);

  const supersedeResidentTurns = (ids: readonly ResidentId[]): number => {
    const residents = new Set(ids);
    const matchingRequests = [...pendingRequests.current.values()]
      .filter(({ residentIds }) => residentIds.some((agentId) => residents.has(agentId)));
    for (const request of matchingRequests) request.cancelled = true;
    for (const [actionId, pending] of pendingWorldActions.current) {
      if (residents.has(pending.agentId)) pendingWorldActions.current.delete(actionId);
    }
    return matchingRequests.length;
  };

  const issueDialogue = (input: string) => {
    if (!worldRef.current) return;
    const speech = playerSpeak(worldRef.current, input, voiceLevel);
    if (!speech) return;
    const mindsToWake = speech.liveAddressed;
    supersedeResidentTurns(mindsToWake);
    nextCoordinationAt.current = 0;
    setPaused(false);
    setSpeechNotice(
      `Raw order delivered to ${mindsToWake.length} resident mind${mindsToWake.length === 1 ? "" : "s"}. Each Luna resident will interpret it from their own identity and world state; no destination was assumed by the page.`,
    );
    if (mindsToWake.length > 0) startAgents();
    setDraft("");
    invalidateWorld();
  };

  const submitDialogue = (event: FormEvent) => {
    event.preventDefault();
    issueDialogue(draft);
  };

  const sendFormationPrompt = (preset: WorldFormationPrompt) => {
    issueDialogue(preset.prompt);
  };

  const nudgePlayer = (direction: Direction) => {
    if (worldRef.current) movePlayer(worldRef.current, direction);
    requestWorldRender();
    canvasRef.current?.focus();
  };

  const departResident = (residentId: ResidentId): boolean => {
    const activeWorld = worldRef.current;
    if (!activeWorld || !requestResidentExit(activeWorld, residentId)) return false;
    setSpeechNotice(`${activeWorld.actors[residentId].name} decided to walk out through the nearest map edge.`);
    invalidateWorld();
    return true;
  };

  const handleCanvasPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    event.currentTarget.focus();
    const activeWorld = worldRef.current;
    const rect = event.currentTarget.getBoundingClientRect();
    if (!activeWorld || rect.width <= 0 || rect.height <= 0) return;
    const viewportX = ((event.clientX - rect.left) / rect.width) * WORLD_PIXEL_WIDTH;
    const viewportY = ((event.clientY - rect.top) / rect.height) * WORLD_PIXEL_HEIGHT;
    const point = viewportToWorld(worldCameraForState(activeWorld), viewportX, viewportY);
    const residentId = residentAtWorldPoint(activeWorld, point.scene, point.x, point.y);
    if (residentId) departResident(residentId);
  };

  const changePopulation = (requested: number) => {
    const activeWorld = worldRef.current;
    if (!activeWorld) return;
    const change = setPopulationTarget(activeWorld, requested);
    setSpeechNotice(
      change.entering.length > 0
        ? `${change.entering.length} resident${change.entering.length === 1 ? " is" : "s are"} joining from map edges or turning back into town.`
        : change.exiting.length > 0
          ? `${change.exiting.length} resident${change.exiting.length === 1 ? " is" : "s are"} walking out under their own power.`
          : undefined,
    );
    invalidateWorld();
  };

  const resetTown = () => {
    stopAgents();
    localStorage.removeItem(WORLD_SAVE_KEY);
    worldRef.current = createWorldState();
    nextCoordinationAt.current = 0;
    usageRef.current = emptyUsage;
    setUsage(emptyUsage);
    setAgentError(undefined);
    setSpeechNotice(undefined);
    setSelectedResident("cinder");
    setPaused(false);
    invalidateWorld();
  };

  const hasCredential = credentialSource === "brokered";
  const agentStatus: AgentStatus = capabilityError || !hasCredential
    ? "idle"
    : runtimeStatus === "ready"
      ? "ready"
      : runtimeStatus === "error"
        ? "error"
        : "stopped";
  const pendingByAgent = new Set(
    [...pendingRequests.current.values()].flatMap(({ residentIds }) => residentIds),
  );
  const relayActive = isGuildRelayActive(world);
  const residentsOnMap = activeResidentCount(world);
  const onMapMindIds = RESIDENT_IDS.filter((id) => world.actors[id].presence !== "absent");
  const scout = world.actors.player;
  const scoutPosition = actorWorldPosition(scout);
  const currentSceneLabel = sceneLabel(scoutPosition.scene);
  const residentsInScene = onMapMindIds.filter((id) => (
    actorWorldPosition(world.actors[id]).scene === scoutPosition.scene
  )).length;
  const carriedItem = carriedItemLabel(scout.carrying);
  const selectableResidents = RESIDENT_IDS.filter((id) => {
    const presence = world.actors[id].presence;
    return presence === "active" || presence === "entering";
  });
  const selectedResidentId = selectableResidents.includes(selectedResident)
    ? selectedResident
    : selectableResidents[0];
  const lunaStatus = runtimeStatus === "ready"
    ? "Luna online"
    : runtimeStatus === "stopping"
      ? "Luna closing"
    : runtimeStatus === "error"
        ? "Luna error"
        : "Luna offline";
  const worldStatus = `${currentSceneLabel} · ${lunaStatus}`;
  const currentSpeechNotice = speechNotice;
  void revision;

  if (assetError) throw assetError;

  return (
    <section className="monster-world" aria-labelledby="monster-world-title">
      <header className="monster-world-heading">
        <div>
          <p>Nanocodex field study 001 · 64×48 connected district · browser local</p>
          <h1 id="monster-world-title">Springleaf Rescue District</h1>
        </div>
        <span className={runtimeStatus === "ready" ? "is-live" : ""}>
          <i aria-hidden="true" /> {worldStatus}
        </span>
      </header>

      <div className="monster-world-layout">
        <div className="monster-world-play-column">
          <section className="monster-world-stage-shell" aria-label={`Playable Springleaf district, current scene ${currentSceneLabel}`}>
            <div className="monster-world-stage-head">
              <span>{residentsInScene} here · {residentsOnMap} district-wide · target {world.populationTarget}/{MAX_RESIDENT_COUNT}</span>
              <span className="monster-world-scene-plaque">{currentSceneLabel}</span>
              <span>{formatWorldTime(world.minuteOfDay)} · {world.weather}</span>
            </div>
            <div className="monster-world-stage">
              <canvas
                ref={canvasRef}
                width={WORLD_PIXEL_WIDTH}
                height={WORLD_PIXEL_HEIGHT}
                tabIndex={0}
                onBlur={() => heldDirections.current.clear()}
                onPointerDown={handleCanvasPointerDown}
                role="img"
                aria-describedby="monster-world-description"
                aria-label={`${currentSceneLabel}, the current scene in a connected 64 by 48 tile district. ${residentsInScene} residents share this scene and can be selected on the map; ${residentsOnMap} are active district-wide. Scout has ${Math.round(scout.energy)} energy and is carrying ${carriedItem.toLowerCase()}. Enter the guild and shop through their doors, gather a sunberry in the orchard, deliver it to the shop, carry a supply pack to the guild, rest, or train. The guild journal lists residents in every scene.`}
              />
              <div className="monster-world-map-stamp" aria-hidden="true">64×48 DISTRICT · {currentSceneLabel}</div>
            </div>
            <p className="sr-only" id="monster-world-description">
              Explore a connected 64 by 48 tile district. Walk through the marked guild and shop doors to enter their interior scenes. Focus the map, then move Scout with arrow keys or WASD. Press Space or activate the on-screen A control near a resident or landmark to interact. Gather a sunberry in the orchard, deliver it to the shop, take a supply pack to the guild, rest to recover energy, and train at the meadow. Click a resident in Scout’s current scene to ask them to walk out; residents in other scenes remain available in the cast. Press Q to cycle voice loudness or 1 through 4 to select it directly. Calls reach only residents inside the audible radius, except at the guild relay where everyone hears you. The population slider adds or removes residents with visible entrances and departures.
            </p>
            <dl className="monster-world-hud" aria-label="Scout and district supplies">
              <div className="monster-world-hud-scene"><dt>Current scene</dt><dd>{currentSceneLabel}</dd></div>
              <div><dt>Scout energy</dt><dd>{Math.round(scout.energy)}%</dd></div>
              <div><dt>Satchel</dt><dd>{carriedItem}</dd></div>
              <div><dt>Orchard berries</dt><dd>{world.supplies.orchardBerries}</dd></div>
              <div><dt>Shop stock</dt><dd>{world.supplies.shopStock}</dd></div>
              <div><dt>Guild supplies</dt><dd>{world.supplies.guildSupplies}</dd></div>
              <div><dt>Training marks</dt><dd>{world.supplies.trainingMarks}</dd></div>
            </dl>
            <p className="monster-world-route-guide">
              Enter the guild and shop doors · orchard sunberry → shop → supply pack → guild · rest to recover · train at the meadow · <kbd>A</kbd>/<kbd>Space</kbd> interacts
            </p>
            <div className="monster-world-command-row">
              <div className="monster-world-dpad" role="group" aria-label="Movement controls">
                <button type="button" aria-label="Move up" onClick={() => nudgePlayer("up")}>↑</button>
                <button type="button" aria-label="Move left" onClick={() => nudgePlayer("left")}>←</button>
                <button type="button" aria-label="Interact" onClick={() => {
                  playerInteract(world);
                  invalidateWorld();
                }}>A</button>
                <button type="button" aria-label="Move right" onClick={() => nudgePlayer("right")}>→</button>
                <button type="button" aria-label="Move down" onClick={() => nudgePlayer("down")}>↓</button>
              </div>
              <form className="monster-world-talk" onSubmit={submitDialogue}>
                <label htmlFor="world-dialogue">Orchestrate by voice</label>
                <div>
                  <input
                    id="world-dialogue"
                    aria-describedby="monster-world-hearing"
                    value={draft}
                    maxLength={140}
                    placeholder="Cinder, check the bridge…"
                    onChange={(event) => setDraft(event.target.value)}
                  />
                  <button type="submit" disabled={!draft.trim()}>call</button>
                </div>
                <div className="monster-world-voice" role="group" aria-label="Voice loudness">
                  {VOICE_LEVELS.map((level, index) => (
                    <button
                      key={level}
                      type="button"
                      aria-pressed={voiceLevel === level}
                      onClick={() => setVoiceLevel(level)}
                    ><kbd>{index + 1}</kbd>{level}</button>
                  ))}
                </div>
                <p id="monster-world-hearing" className="monster-world-hearing" aria-live="polite">
                  {relayActive
                    ? `Guild relay active: every on-map resident hears you across all scenes.`
                    : `${voiceLevel} reaches ${VOICE_RADIUS[voiceLevel]} tiles · Q cycles loudness.`}
                </p>
                {currentSpeechNotice ? <p className="monster-world-speech-notice" role="status">{currentSpeechNotice}</p> : null}
                <section className="monster-world-formation-lab" aria-labelledby="world-formation-title">
                  <header>
                    <span id="world-formation-title">Formation lab</span>
                    <b>prompt helpers</b>
                  </header>
                  <div className="monster-world-formation-ladder">
                    {WORLD_FORMATION_PROMPTS.map((preset, index) => (
                      <button
                        key={preset.id}
                        type="button"
                        aria-label={`Ask residents to form a ${preset.label}`}
                        title={preset.prompt}
                        onClick={() => sendFormationPrompt(preset)}
                      >
                        <small>{index + 1}</small>
                        {preset.label}
                      </button>
                    ))}
                  </div>
                  <p className="monster-world-formation-empty">These buttons only send example prompts. Residents interpret the same words independently; the reducer supplies no slots, target points, or answer key.</p>
                </section>
              </form>
            </div>
            <div className="monster-world-population">
              <label htmlFor="world-population">
                Town population
                <output htmlFor="world-population">{residentsOnMap} on map · target {world.populationTarget}</output>
              </label>
              <input
                id="world-population"
                type="range"
                min="0"
                max={MAX_RESIDENT_COUNT}
                step="1"
                value={world.populationTarget}
                onChange={(event) => changePopulation(Number(event.currentTarget.value))}
              />
              <p>Raise the slider to bring characters in from random points just outside the 64×48 district. Click a visible resident in Scout’s current scene to make them walk out; use the cast to choose anyone elsewhere.</p>
              <div className="monster-world-resident-exit">
                <label htmlFor="world-resident-exit">Choose a resident</label>
                <div>
                  <select
                    id="world-resident-exit"
                    value={selectedResidentId ?? ""}
                    disabled={!selectedResidentId}
                    onChange={(event) => {
                      if (isResidentId(event.currentTarget.value)) {
                        setSelectedResident(event.currentTarget.value);
                      }
                    }}
                  >
                    {selectableResidents.map((id) => (
                      <option key={id} value={id}>{world.actors[id].name} · {world.actors[id].role}</option>
                    ))}
                  </select>
                  <button
                    type="button"
                    disabled={!selectedResidentId}
                    onClick={() => {
                      if (selectedResidentId) departResident(selectedResidentId);
                    }}
                  >ask to leave</button>
                </div>
              </div>
            </div>
          </section>

          <section className="monster-world-mission" aria-labelledby="world-mission-title">
            <div>
              <p>Active expedition · clue {Math.min(world.mission.stage + 1, 3)}/3</p>
              <h2 id="world-mission-title">{world.mission.title}</h2>
            </div>
            <p>{world.mission.detail}</p>
          </section>
        </div>

        <aside className="monster-world-journal" aria-label="Guild controls and activity">
          <div className="monster-world-agent-panel">
            <AgentSessionBar
              agentStatus={agentStatus}
              agentError={agentError}
              source={credentialSource}
              capabilityError={capabilityError}
              onAuthStatusChange={setAuthStatus}
              onRetryAgent={startAgents}
              onSourceChange={setCredentialSource}
            />
            <div className="monster-world-agent-actions">
              {runtimeStatus === "ready" || runtimeStatus === "stopping" ? (
                <button type="button" disabled={runtimeStatus === "stopping"} onClick={stopAgents}>
                  {runtimeStatus === "stopping" ? "closing minds" : "stop agents"}
                </button>
              ) : (
                <button
                  type="button"
                  disabled={
                    !hasCredential
                    || Boolean(capabilityError)
                    || runtimeStatus === "starting"
                    || onMapMindIds.length === 0
                  }
                  onClick={startAgents}
                >{`${runtimeStatus === "error" ? "retry" : "wake"} ${onMapMindIds.length} minds`}</button>
              )}
              <button type="button" aria-pressed={paused} onClick={() => setPaused((value) => !value)}>
                {paused ? "resume town" : "pause town"}
              </button>
              <button type="button" onClick={resetTown}>reset</button>
            </div>
          </div>

              <section className="monster-world-cast" aria-labelledby="world-cast-title">
                <header>
                  <div><p>One task tree · six squads · retained resident sessions</p><h2 id="world-cast-title">Autonomous minds</h2></div>
                  <span>{pendingRequests.current.size > 0 ? `${pendingByAgent.size} resident minds coordinating` : "task tree idle"}</span>
                </header>
                <div className="monster-world-scale" aria-label="World population architecture">
                  <span><strong>{residentsOnMap}</strong> on map</span>
                  <span><strong>1</strong> root · <strong>6</strong> leaders · resident descendants</span>
            </div>
            <ol>
              {onMapMindIds.map((id) => {
                const actor = world.actors[id];
                const live = runtimeStatus === "ready" && actor.presence === "active";
                const listening = hasUnansweredGuildCall(world, id);
                return (
                  <li key={id}>
                    <AgentPortrait actor={actor} />
                    <span>
                      <strong>{actor.name}<small>{actor.role} · {sceneLabel(actorWorldPosition(actor).scene)}</small></strong>
                      <em>
                        <i>{actor.intent ?? actor.activity}</i>
                        {actor.intent && actor.activity !== actor.intent
                          ? <small>now · {actor.activity}</small>
                          : null}
                      </em>
                    </span>
                    <b className={live ? "is-agent" : ""}>
                      {actor.presence === "absent"
                        ? "away"
                        : actor.presence === "entering"
                          ? "entering"
                          : actor.presence === "exiting"
                            ? "leaving"
                            : actor.activeOrderId !== undefined
                              ? "obeying"
                              : pendingByAgent.has(id)
                                ? "thinking"
                                : listening
                                  ? "heard call"
                                  : actor.lastOrigin === "nanocodex"
                                    ? "nanocodex"
                                    : live
                                      ? "own session · live"
                                      : runtimeStatus === "error"
                                          ? "Luna error"
                                          : "Luna offline"}
                    </b>
                  </li>
                );
              })}
            </ol>
          </section>

          {world.orders.length > 0 ? (
            <section className="monster-world-orders" aria-labelledby="world-orders-title">
              <header>
                <div><p>Reducer-owned execution receipts</p><h2 id="world-orders-title">Scout orders</h2></div>
                <span aria-live="polite">{world.orders.length} retained</span>
              </header>
              <ol>
                {world.orders.slice(0, 3).map((order) => {
                  const completed = order.assignments.filter(({ status }) => status === "completed").length;
                  const active = order.assignments.filter(({ status }) => status === "assigned" || status === "moving").length;
                  const executable = order.assignments.filter(({ status }) => status !== "rejected").length;
                  return (
                    <li key={order.id} data-active={active > 0 ? "true" : "false"}>
                      <div>
                        <strong>Order {order.id}</strong>
                        <span>{completed}/{executable} complete · {active} moving</span>
                      </div>
                      <p>“{order.text}”</p>
                      <progress
                        max={Math.max(executable, 1)}
                        value={completed}
                        aria-label={`Order ${order.id}: ${completed} of ${executable} residents completed`}
                      />
                      <ul>
                        {order.assignments.map((assignment) => (
                          <li key={assignment.actorId} data-status={assignment.status}>
                            <span>{world.actors[assignment.actorId].name}</span>
                            <small>
                              {assignment.interaction
                                ? `${assignment.interaction} · ${assignment.target.replaceAll("_", " ")}`
                                : assignment.target.replaceAll("_", " ")}
                            </small>
                            <b>{assignment.status}</b>
                          </li>
                        ))}
                      </ul>
                    </li>
                  );
                })}
              </ol>
            </section>
          ) : null}

              <section className="monster-world-board" aria-labelledby="world-board-title">
                <header>
                  <div><p>Embodied local speech · not task-tree coordination</p><h2 id="world-board-title">Scene dialogue</h2></div>
              <span aria-live="polite">{world.guildMessages.length} posts</span>
            </header>
            <ol>
              {world.guildMessages.slice(0, 7).map((message) => {
                const from = world.actors[message.fromId];
                const to = message.toId === undefined ? undefined : world.actors[message.toId];
                const audience = message.audience?.map((id) => world.actors[id].name).join(", ");
                const destination = to
                  ? to.name
                  : message.scope === "spatial"
                    ? `heard by ${audience || "nearby listeners"}`
                    : "room";
                return (
                  <li key={message.id} data-origin={message.origin}>
                    <div>
                      <strong>{from.name} → {destination}</strong>
                      <span>{message.origin} · <time>{formatWorldTime(message.minuteOfDay)}</time></span>
                    </div>
                    <p>“{message.text}”</p>
                  </li>
                );
              })}
            </ol>
          </section>

          <section className="monster-world-activity" aria-labelledby="world-activity-title">
            <header>
              <div><p>Semantic event stream</p><h2 id="world-activity-title">Guild journal</h2></div>
              <span aria-live="polite">{world.activities.length} events</span>
            </header>
            <ol>
              {world.activities.slice(0, 9).map((entry) => (
                <li key={entry.id} data-origin={entry.origin}>
                  <time>{formatWorldTime(entry.minuteOfDay)}</time>
                  <p>{entry.text}</p>
                  <span>{entry.origin}</span>
                </li>
              ))}
            </ol>
          </section>

          <footer className="monster-world-telemetry">
                <div>
                  <span>reported root activity</span>
                  <strong>{usage.modelTurns} coordinated calls</strong>
                </div>
                <p>{usage.totalTokens.toLocaleString()} observed tokens · {usage.estimatedUsd > 0 ? `$${usage.estimatedUsd.toFixed(4)} estimated` : "cost appears when reported"}</p>
                <p>GPT-5.6 Luna · thinking none · one Guild Dispatch root and one persistent task-tree session per resident.</p>
                <p>Squads coordinate through directed subagent messages. The reducer only applies physical tool actions and local scene dialogue. Every entry marked <b>nanocodex</b> came from a live resident tool loop.</p>
          </footer>
        </aside>
      </div>
    </section>
  );
}

function AgentPortrait({ actor }: Readonly<{ actor: WorldActor }>) {
  if (actor.kind === "monster") {
    return (
      <img
        className="monster-world-portrait"
        src={worldAssetSource(`menu-sprites/menusprite${actor.sprite}.png`)}
        alt=""
        width="32"
        height="32"
      />
    );
  }
  return (
    <span
      className="monster-world-portrait is-human"
      aria-hidden="true"
      style={{
        backgroundImage: `url(${worldAssetSource(`character-overworld/ow${actor.sprite}.png`)})`,
      }}
    />
  );
}

function carriedItemLabel(item: WorldActor["carrying"]): string {
  if (item === "sunberry") return "Sunberry";
  if (item === "supply_pack") return "Supply pack";
  return "Empty satchel";
}

function commitModelUsage(
  turn: WorldUsage | undefined,
  usageRef: { current: UsageTotals },
  publish: (usage: UsageTotals) => void,
): UsageTotals {
  const next = {
    modelTurns: usageRef.current.modelTurns + (turn?.modelTurns ?? 0),
    inputTokens: usageRef.current.inputTokens + (turn?.inputTokens ?? 0),
    outputTokens: usageRef.current.outputTokens + (turn?.outputTokens ?? 0),
    totalTokens: usageRef.current.totalTokens + (turn?.totalTokens ?? 0),
    estimatedUsd: usageRef.current.estimatedUsd + (Number(turn?.estimatedUsd) || 0),
  };
  usageRef.current = next;
  publish(next);
  return next;
}

function readSavedWorld(): string | null {
  try {
    return localStorage.getItem(WORLD_SAVE_KEY);
  } catch {
    return null;
  }
}

function saveWorld(world: WorldState): void {
  try {
    localStorage.setItem(WORLD_SAVE_KEY, serializeWorldState(world));
  } catch {
    // Local persistence is optional; the complete in-tab simulation keeps running.
  }
}

function worldCapabilityError(): string | undefined {
  if (typeof Worker !== "function") return "This browser does not support module Workers.";
  if (typeof WebAssembly !== "object") return "This browser does not support WebAssembly.";
  if (typeof WebSocket !== "function") return "This browser does not support WebSockets.";
  if (typeof crypto?.randomUUID !== "function") return "This browser cannot create secure world turn identifiers.";
  return undefined;
}

function directionForKey(key: string): Direction | undefined {
  const normalized = key.toLowerCase();
  if (normalized === "arrowup" || normalized === "w") return "up";
  if (normalized === "arrowdown" || normalized === "s") return "down";
  if (normalized === "arrowleft" || normalized === "a") return "left";
  if (normalized === "arrowright" || normalized === "d") return "right";
  return undefined;
}

function isTypingTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLElement
    && target.matches("input, textarea, button, [contenteditable='true']");
}

function voiceLevelForKey(key: string): VoiceLevel | undefined {
  const index = Number(key) - 1;
  return Number.isInteger(index) && index >= 0 && index < VOICE_LEVELS.length
    ? VOICE_LEVELS[index]
    : undefined;
}

function nextVoiceLevel(current: VoiceLevel): VoiceLevel {
  const index = VOICE_LEVELS.indexOf(current);
  return VOICE_LEVELS[(index + 1) % VOICE_LEVELS.length] ?? "call";
}
