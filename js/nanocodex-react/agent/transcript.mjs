export function initialState(status = "Ready") {
  return {
    entries: [], running: false, status, pendingTurns: 0, queuedPrompts: [],
    displayedQueuedPrompt: undefined, pendingSteers: [], appliedSteerRuns: [],
    runGeneration: 0, activeTurnId: undefined, streamedThisTurn: false,
    pendingRunError: undefined, modelCalls: 0, syntheticId: 0,
  };
}

export function queuePrompt(state, id, text, historyEntryId) {
  const displayImmediately = !state.running && state.queuedPrompts.length === 0;
  const turnId = historyTurnId(historyEntryId);
  return {
    ...state,
    entries: displayImmediately ? [...state.entries, {
      id: historyEntryId ?? `user-${id}`, kind: "user", text, promptId: id,
      ...(turnId === undefined ? {} : { turnId }),
    }] : state.entries,
    queuedPrompts: [...state.queuedPrompts, { id, text, historyEntryId, turnId }],
    displayedQueuedPrompt: displayImmediately ? id : state.displayedQueuedPrompt,
    pendingTurns: state.pendingTurns + 1,
    status: state.running ? "Prompt queued" : "Starting",
  };
}

export function queueSteer(state, id, text) {
  return {
    ...state,
    entries: [...state.entries, {
      id: `steer-${id}`, kind: "user", text,
      ...(state.activeTurnId === undefined ? {} : { turnId: state.activeTurnId }),
    }],
    pendingSteers: [...state.pendingSteers, {
      id, text, state: "submitting", runGeneration: state.runGeneration,
    }],
    status: "Submitting steer",
  };
}

export function steerAdmitted(state, id) {
  return reconcileSteers({
    ...state,
    pendingSteers: state.pendingSteers.map((steer) => (
      steer.id === id ? { ...steer, state: "admitted" } : steer
    )),
    status: state.running ? "Steer pending" : state.status,
  });
}

export function steerFailed(state, id, error) {
  return appendError({
    ...state,
    pendingSteers: state.pendingSteers.filter((steer) => steer.id !== id),
  }, error);
}

export function requeueSteerAsPrompt(state, id, text, historyEntryId) {
  const turnId = historyTurnId(historyEntryId);
  return {
    ...state,
    entries: state.entries.map((entry) => entry.id === `steer-${id}` ? {
      id: historyEntryId ?? `user-${id}`, kind: "user", text, promptId: id,
      ...(turnId === undefined ? {} : { turnId }),
    } : entry),
    pendingSteers: state.pendingSteers.filter((steer) => steer.id !== id),
    queuedPrompts: [...state.queuedPrompts, { id, text, historyEntryId, turnId }],
    displayedQueuedPrompt: id,
    pendingTurns: state.pendingTurns + 1,
    status: "Prompt queued",
  };
}

export function turnFinished(state, error, finalMessage, promptId, historyEntryId) {
  const turnId = historyTurnId(historyEntryId);
  let next = {
    ...state,
    pendingTurns: Math.max(0, state.pendingTurns - 1),
    queuedPrompts: promptId === undefined ? state.queuedPrompts
      : state.queuedPrompts.filter((prompt) => prompt.id !== promptId),
    displayedQueuedPrompt: state.displayedQueuedPrompt === promptId
      ? undefined : state.displayedQueuedPrompt,
  };
  const finishedActiveRun = state.running && (
    (turnId !== undefined && state.activeTurnId === turnId)
    || (state.activeTurnId === undefined && next.pendingTurns === 0)
  );
  if (finishedActiveRun) {
    next = {
      ...next,
      running: false,
      activeTurnId: undefined,
      pendingRunError: undefined,
      status: error ? "Turn failed" : "Ready",
    };
  }
  if (finalMessage?.trim()) {
    let userIndex = -1;
    let assistantIndex = -1;
    for (let index = next.entries.length - 1; index >= 0; index -= 1) {
      const entry = next.entries[index];
      if (entry?.kind === "user" && (
        (turnId !== undefined && entry.turnId === turnId)
        || (turnId === undefined && (promptId === undefined || entry.promptId === promptId))
      )) {
        userIndex = index;
        break;
      }
    }
    for (let index = next.entries.length - 1; index > userIndex; index -= 1) {
      const entry = next.entries[index];
      if (entry?.kind === "assistant" && (turnId === undefined || entry.turnId === turnId)) {
        assistantIndex = index;
        break;
      }
    }
    if (assistantIndex >= 0) {
      const assistant = next.entries[assistantIndex];
      if (assistant.text !== finalMessage || assistant.streaming) {
        const entries = next.entries.slice();
        entries[assistantIndex] = { ...assistant, text: finalMessage, streaming: false };
        next = { ...next, entries };
      }
    } else {
      const syntheticId = next.syntheticId + 1;
      next = {
        ...next, syntheticId,
        entries: [...next.entries, {
          id: `assistant-result-${syntheticId}`, kind: "assistant",
          text: finalMessage, streaming: false,
          ...(turnId === undefined ? {} : { turnId }),
        }],
      };
    }
  }
  if (!error || error === "the turn was cancelled") return next;
  const tail = next.entries.at(-1);
  return tail?.kind === "error" && tail.text === error ? next : appendError(next, error, turnId);
}

export function applyAgentEvents(state, events) {
  if (events.length === 0) return state;
  let next = { ...state };
  let ownsEntries = false;
  let bufferedKind;
  let bufferedId = "";
  let bufferedTurnId;
  let bufferedText = [];
  const mutableEntries = () => {
    if (!ownsEntries) {
      next = { ...next, entries: next.entries.slice() };
      ownsEntries = true;
    }
    return next.entries;
  };
  const sealTail = () => {
    const tail = next.entries.at(-1);
    if (tail && (tail.kind === "assistant" || tail.kind === "reasoning") && tail.streaming) {
      sealStreamingTail(mutableEntries());
    }
  };
  const flushDeltas = () => {
    if (!bufferedKind || bufferedText.length === 0) return;
    const text = bufferedText.join("");
    const entries = mutableEntries();
    const tail = entries.at(-1);
    if (tail?.kind === bufferedKind && tail.streaming) {
      entries[entries.length - 1] = { ...tail, text: tail.text + text };
    } else {
      sealStreamingTail(entries);
      entries.push({
        id: bufferedId, kind: bufferedKind, text, streaming: true,
        ...(bufferedTurnId === undefined ? {} : { turnId: bufferedTurnId }),
      });
    }
    next.streamedThisTurn ||= bufferedKind === "assistant";
    bufferedKind = undefined;
    bufferedId = "";
    bufferedTurnId = undefined;
    bufferedText = [];
  };

  for (const event of events) {
    const payload = event.payload ?? {};
    if (event.type === "assistant.delta" || event.type === "reasoning.summary.delta") {
      const kind = event.type === "assistant.delta" ? "assistant" : "reasoning";
      if (bufferedKind && bufferedKind !== kind) flushDeltas();
      bufferedKind = kind;
      bufferedId ||= `${kind}-${eventIdentity(event)}`;
      bufferedTurnId ??= payloadString(payload, "turn_id") ?? next.activeTurnId;
      bufferedText.push(payloadString(payload, "text") ?? "");
      continue;
    }
    flushDeltas();
    switch (event.type) {
      case "managed.prompt": {
        const turnId = payloadString(payload, "turn_id");
        const text = payloadString(payload, "text");
        const id = turnId ? `managed-user-${turnId}` : `managed-user-${event.seq}`;
        if (text && !next.entries.some((entry) => entry.id === id)) {
          mutableEntries().push({ id, kind: "user", text, ...(turnId ? { turnId } : {}) });
        }
        break;
      }
      case "managed.steer": {
        const turnId = payloadString(payload, "turn_id");
        const steerId = payloadString(payload, "steer_id") ?? eventIdentity(event);
        const text = payloadString(payload, "text");
        const id = `managed-steer-${turnId ?? "unknown"}-${steerId}`;
        if (text && !next.entries.some((entry) => entry.id === id)) {
          mutableEntries().push({ id, kind: "user", text, ...(turnId ? { turnId } : {}) });
        }
        break;
      }
      case "run.started": {
        const eventTurnId = payloadString(payload, "turn_id");
        const promptIndex = eventTurnId === undefined
          ? (next.queuedPrompts.length > 0 ? 0 : -1)
          : next.queuedPrompts.findIndex((queued) => queued.turnId === eventTurnId);
        const prompt = promptIndex < 0 ? undefined : next.queuedPrompts[promptIndex];
        const promptEntryId = prompt?.historyEntryId ?? (prompt ? `user-${prompt.id}` : undefined);
        if (prompt && next.displayedQueuedPrompt !== prompt.id
          && !next.entries.some((entry) => entry.kind === "user" && (
            prompt.turnId === undefined ? entry.id === promptEntryId : entry.turnId === prompt.turnId
          ))) {
          mutableEntries().push({
            id: promptEntryId, kind: "user", text: prompt.text, promptId: prompt.id,
            ...(prompt.turnId === undefined ? {} : { turnId: prompt.turnId }),
          });
        }
        next = {
          ...next,
          queuedPrompts: promptIndex < 0 ? next.queuedPrompts
            : next.queuedPrompts.filter((_, index) => index !== promptIndex),
          displayedQueuedPrompt: prompt && next.displayedQueuedPrompt === prompt.id
            ? undefined : next.displayedQueuedPrompt,
          running: true,
          activeTurnId: eventTurnId ?? prompt?.turnId,
          runGeneration: next.runGeneration + 1,
          streamedThisTurn: false,
          pendingRunError: undefined,
          status: "Thinking...",
        };
        break;
      }
      case "run.steered":
        next = reconcileSteers({
          ...next,
          appliedSteerRuns: [...next.appliedSteerRuns, next.runGeneration],
          status: "Steer applied",
        });
        break;
      case "model.warmup.started": next.status = "Prewarming model..."; break;
      case "model.warmup.completed": next.status = "Thinking..."; break;
      case "model.warmup.failed": next.status = "Warmup unavailable; continuing"; break;
      case "model.connection.started": next.status = "Connecting..."; break;
      case "model.call.started": next.status = "Thinking..."; break;
      case "model.attempt.retrying": next.status = "Retrying..."; break;
      case "assistant.message": {
        const text = payloadString(payload, "text") ?? "";
        const turnId = payloadString(payload, "turn_id") ?? next.activeTurnId;
        const tail = next.entries.at(-1);
        if (tail?.kind === "assistant" && tail.turnId === turnId) {
          const entries = mutableEntries();
          entries[entries.length - 1] = { ...tail, text, streaming: false };
        } else if (text) {
          mutableEntries().push({
            id: `assistant-${eventIdentity(event)}`, kind: "assistant", text, streaming: false,
            ...(turnId === undefined ? {} : { turnId }),
          });
        }
        break;
      }
      case "tool.call": {
        const tool = payloadString(payload, "tool") ?? "tool";
        if (isEmptyTerminalPoll(tool, payload.arguments)) break;
        if (tool === "update_plan") {
          const update = decodePlanUpdate(payload.arguments);
          if (update) {
            const turnId = payloadString(payload, "turn_id") ?? next.activeTurnId;
            mutableEntries().push({
              id: `plan-${eventIdentity(event)}`, kind: "plan", update,
              ...(turnId === undefined ? {} : { turnId }),
            });
            next.status = "Working";
            break;
          }
        }
        applyToolCall(mutableEntries(), event, payloadString(payload, "turn_id") ?? next.activeTurnId);
        next.status = `Running ${tool}`;
        break;
      }
      case "tool.result":
        applyToolResult(mutableEntries(), event, payloadString(payload, "turn_id") ?? next.activeTurnId);
        next.status = "Working";
        break;
      case "model.call.completed": next.modelCalls += 1; break;
      case "run.error": next.pendingRunError = payloadString(payload, "message"); break;
      case "run.completed": {
        sealTail();
        const turnId = payloadString(payload, "turn_id") ?? next.activeTurnId;
        if (next.pendingRunError && !hasProjectedError(next.entries, next.pendingRunError, turnId)) {
          next = appendError(next, next.pendingRunError, turnId);
          ownsEntries = true;
        }
        next = reconcileSteers({
          ...next, running: false, activeTurnId: undefined,
          pendingRunError: undefined, status: "Ready",
        });
        ownsEntries = true;
        break;
      }
      case "run.failed": {
        sealTail();
        const cancelled = payloadString(payload, "status") === "cancelled";
        const turnId = payloadString(payload, "turn_id") ?? next.activeTurnId;
        if (!cancelled && next.pendingRunError
          && !hasProjectedError(next.entries, next.pendingRunError, turnId)) {
          next = appendError(next, next.pendingRunError, turnId);
          ownsEntries = true;
        }
        next = reconcileSteers({
          ...next, running: false, activeTurnId: undefined,
          pendingRunError: undefined, status: cancelled ? "Cancelled" : "Turn failed",
        });
        ownsEntries = true;
        break;
      }
    }
  }
  flushDeltas();
  return next;
}

export function mergeHistoryEntries(current, historical, previouslyProjectedKeys) {
  const historicalGroups = transcriptTurnGroups(historical);
  const historicalKeys = historyGroupEntryKeys(historicalGroups);
  const currentGroups = transcriptTurnGroups(current).flatMap((group) => {
    const entries = group.entries.filter((entry) => {
      const key = historyEntryKey(group.turnId, entry);
      return !previouslyProjectedKeys.has(key) && !historicalKeys.has(key);
    });
    return entries.length === 0 ? [] : [{ ...group, entries }];
  });
  const historicalKinds = new Map();
  for (const group of historicalGroups) {
    if (group.turnId) historicalKinds.set(group.turnId, new Set(group.entries.map((entry) => entry.kind)));
  }
  const merged = [];
  const emittedCurrentTurns = new Set();
  for (const group of historicalGroups) {
    if (!group.turnId) {
      merged.push(...group.entries);
      continue;
    }
    const live = currentGroups.find((candidate) => candidate.turnId === group.turnId);
    if (!live) {
      merged.push(...group.entries);
      continue;
    }
    const replacedKinds = historicalKinds.get(group.turnId) ?? new Set();
    const liveEntries = live.entries.filter((entry) => (
      entry.kind !== "user" && !(entry.kind === "assistant" && replacedKinds.has("assistant"))
    ));
    const finalAssistant = group.entries.findIndex((entry) => entry.kind === "assistant");
    if (finalAssistant < 0) merged.push(...group.entries, ...liveEntries);
    else merged.push(
      ...group.entries.slice(0, finalAssistant), ...liveEntries,
      ...group.entries.slice(finalAssistant),
    );
    emittedCurrentTurns.add(group.turnId);
  }
  for (const group of currentGroups) {
    if (!group.turnId || !emittedCurrentTurns.has(group.turnId)) merged.push(...group.entries);
  }
  return merged;
}

export function historyEntryKeys(entries) {
  return historyGroupEntryKeys(transcriptTurnGroups(entries));
}

function appendError(state, text, turnId = state.activeTurnId) {
  const syntheticId = state.syntheticId + 1;
  return {
    ...state, syntheticId,
    entries: [...state.entries, {
      id: `error-${syntheticId}`, kind: "error", text,
      ...(turnId === undefined ? {} : { turnId }),
    }],
  };
}

function historyGroupEntryKeys(groups) {
  const keys = new Set();
  for (const group of groups) {
    for (const entry of group.entries) keys.add(historyEntryKey(group.turnId, entry));
  }
  return keys;
}

function historyEntryKey(turnId, entry) {
  return turnId === undefined ? `unowned\0${entry.id}` : `turn\0${turnId}\0${entry.id}`;
}

function transcriptTurnGroups(entries) {
  const groups = [];
  const ownedGroups = new Map();
  let inferredTurnId;
  for (const entry of entries) {
    if (entry.kind === "user") inferredTurnId = entry.turnId ?? historyTurnId(entry.id);
    const turnId = entry.turnId ?? inferredTurnId;
    if (turnId !== undefined) {
      const retained = ownedGroups.get(turnId);
      if (retained) retained.entries.push(entry);
      else {
        const group = { turnId, entries: [entry] };
        ownedGroups.set(turnId, group);
        groups.push(group);
      }
    } else {
      const tail = groups.at(-1);
      if (!tail || tail.turnId !== undefined) groups.push({ entries: [entry] });
      else tail.entries.push(entry);
    }
  }
  return groups;
}

function historyTurnId(historyEntryId) {
  const prefix = "managed-user-";
  return historyEntryId?.startsWith(prefix) ? historyEntryId.slice(prefix.length) : undefined;
}

function reconcileSteers(state) {
  const pendingSteers = state.pendingSteers.slice();
  const appliedSteerRuns = state.appliedSteerRuns.slice();
  const entries = state.entries.slice();
  let applied = 0;
  while (appliedSteerRuns.length > 0) {
    const generation = appliedSteerRuns[0];
    const index = pendingSteers.findIndex(
      (steer) => steer.runGeneration === generation && steer.state === "admitted",
    );
    if (index < 0) break;
    const [steer] = pendingSteers.splice(index, 1);
    if (!entries.some((entry) => entry.id === `steer-${steer.id}`)) {
      entries.push({
        id: `steer-${steer.id}`, kind: "user", text: steer.text,
        ...(state.activeTurnId === undefined ? {} : { turnId: state.activeTurnId }),
      });
    }
    appliedSteerRuns.shift();
    applied += 1;
  }
  if (!state.running) {
    const waiting = new Set(appliedSteerRuns);
    return {
      ...state, entries,
      pendingSteers: pendingSteers.filter((steer) => waiting.has(steer.runGeneration)),
      appliedSteerRuns,
      status: applied ? "Steer applied" : state.status,
    };
  }
  return { ...state, entries, pendingSteers, appliedSteerRuns };
}

function eventIdentity(event) {
  return payloadString(event.payload ?? {}, "managed_event_cursor") ?? String(event.seq);
}

function applyToolCall(entries, event, turnId) {
  const payload = event.payload ?? {};
  const callId = payloadString(payload, "call_id") ?? `tool-${eventIdentity(event)}`;
  if (hasToolCall(entries, callId, turnId)) return;
  const name = payloadString(payload, "tool") ?? "tool";
  const tool = {
    callId, name,
    arguments: summarizeToolArguments(name, payload.arguments),
    input: serializeToolDetail(payload.arguments),
    status: "running", children: [],
  };
  const parentId = callId.split("/code-")[0];
  if (parentId !== callId) {
    for (let index = entries.length - 1; index >= 0; index -= 1) {
      const entry = entries[index];
      if (entry?.kind === "tool" && entry.tool.callId === parentId
        && (turnId === undefined || entry.turnId === turnId)) {
        entries[index] = {
          ...entry, tool: { ...entry.tool, children: [...entry.tool.children, tool] },
        };
        return;
      }
    }
  }
  entries.push({
    id: `tool-${callId}`, kind: "tool", tool,
    ...(turnId === undefined ? {} : { turnId }),
  });
}

function hasToolCall(entries, callId, turnId) {
  return entries.some((entry) => entry.kind === "tool"
    && (turnId === undefined || entry.turnId === turnId)
    && (entry.tool.callId === callId
      || entry.tool.children.some((child) => child.callId === callId)));
}

function applyToolResult(entries, event, turnId) {
  const payload = event.payload ?? {};
  const callId = payloadString(payload, "call_id");
  if (!callId) return;
  const statusValue = payloadString(payload, "status");
  const status = statusValue === "cancelled" ? "cancelled"
    : statusValue === "completed" ? "completed" : "failed";
  for (let index = entries.length - 1; index >= 0; index -= 1) {
    const entry = entries[index];
    if (entry?.kind !== "tool" || (turnId !== undefined && entry.turnId !== turnId)) continue;
    if (entry.tool.callId === callId) {
      entries[index] = { ...entry, tool: completedTool(entry.tool, payload, status) };
      return;
    }
    const childIndex = entry.tool.children.findIndex((child) => child.callId === callId);
    if (childIndex >= 0) {
      const children = entry.tool.children.slice();
      children[childIndex] = completedTool(children[childIndex], payload, status);
      entries[index] = { ...entry, tool: { ...entry.tool, children } };
      return;
    }
  }
}

function completedTool(tool, payload, status) {
  const result = preferredToolResult(payload.structured_result, payload.result);
  const images = extractImageUrls(result);
  return {
    ...tool, status, durationNs: payloadNumber(payload, "duration_ns"),
    ...(images ? { images } : {}),
    ...(payload.metadata === undefined || payload.metadata === null
      ? {}
      : { metadata: payload.metadata }),
    result: summarizeToolResult(tool.name, result, status),
    output: serializeToolDetail(result),
  };
}

function preferredToolResult(structured, modelVisible) {
  const decoded = decodeJsonString(structured);
  if (hasUsefulToolResult(decoded)) return decoded;
  return decodeJsonString(modelVisible);
}

function hasUsefulToolResult(value) {
  if (value === undefined || value === null) return false;
  if (typeof value === "string") return value.trim().length > 0;
  if (Array.isArray(value)) return value.length > 0;
  if (isObject(value)) return Object.keys(value).length > 0;
  return true;
}

function decodePlanUpdate(value) {
  if (!isObject(value) || !Array.isArray(value.plan)) return undefined;
  const plan = value.plan.flatMap((item) => {
    if (!isObject(item) || typeof item.step !== "string") return [];
    if (!["pending", "in_progress", "completed"].includes(item.status)) return [];
    return [{ step: item.step, status: item.status }];
  });
  if (plan.length !== value.plan.length) return undefined;
  return { ...(typeof value.explanation === "string" ? { explanation: value.explanation } : {}), plan };
}

function extractImageUrls(value) {
  const decoded = decodeJsonString(value);
  if (!Array.isArray(decoded)) return undefined;
  const images = decoded.flatMap((item) => (
    isObject(item) && item.type === "input_image" && typeof item.image_url === "string"
      ? [item.image_url] : []
  ));
  return images.length ? images : undefined;
}

function sealStreamingTail(entries) {
  const tail = entries.at(-1);
  if (tail && (tail.kind === "assistant" || tail.kind === "reasoning") && tail.streaming) {
    entries[entries.length - 1] = { ...tail, streaming: false };
  }
}

function hasProjectedError(entries, text, turnId) {
  return entries.some((entry) => entry.kind === "error"
    && entry.text === text && entry.turnId === turnId);
}

function payloadString(payload, key) {
  return typeof payload[key] === "string" ? payload[key] : undefined;
}

function payloadNumber(payload, key) {
  return typeof payload[key] === "number" ? payload[key] : undefined;
}

function isEmptyTerminalPoll(tool, value) {
  return tool === "write_stdin" && isObject(value)
    && (typeof value.chars !== "string" || value.chars.length === 0);
}

function serializeToolDetail(value) {
  return boundedMultiline(formatValue(value));
}

function summarizeToolArguments(tool, value) {
  if (tool === "exec" && typeof value === "string") return boundedMultiline(value);
  if (isObject(value)) {
    if (tool === "write_stdin" && value.session_id !== undefined) return `session ${value.session_id}`;
    const preferred = tool === "exec_command" ? value.cmd
      : tool === "view_image" ? value.path
        : tool === "read_file" ? value.path ?? value.file_path
          : tool === "wait" ? value.cell_id : undefined;
    if (typeof preferred === "string") {
      return tool === "exec_command" && preferred.includes("\n")
        ? boundedMultiline(preferred) : compact(preferred);
    }
  }
  if (tool === "apply_patch" && typeof value === "string") {
    const lines = value.split("\n");
    const files = lines.flatMap((line) => {
      const prefix = ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
        .find((candidate) => line.startsWith(candidate));
      return prefix ? [line.slice(prefix.length)] : [];
    });
    if (files.length) {
      const added = lines.filter((line) => line.startsWith("+")).length;
      const removed = lines.filter((line) => line.startsWith("-")).length;
      return compact(`${files.join(", ")} (+${added} -${removed})`);
    }
  }
  return compact(formatValue(value));
}

function summarizeToolResult(tool, value, status) {
  if (tool === "exec_command") {
    const decoded = decodeJsonString(value);
    if (isObject(decoded)) {
      const parts = [];
      if (typeof decoded.exit_code === "number") parts.push(`exit ${decoded.exit_code}`);
      if (typeof decoded.output === "string") {
        const lines = decoded.output ? decoded.output.split("\n").length : 0;
        if (lines) parts.push(`${lines} line${lines === 1 ? "" : "s"}`);
      }
      if (parts.length) return parts.join(" · ");
    }
  }
  if (tool === "apply_patch" && typeof value === "string" && value.includes("Success")) return "applied";
  return status === "failed" || status === "cancelled" ? compact(formatValue(value)) : undefined;
}

function decodeJsonString(value) {
  if (typeof value !== "string") return value;
  try { return JSON.parse(value); } catch { return value; }
}

function compact(value) {
  const normalized = value.split(/\s+/).filter(Boolean).join(" ");
  return [...normalized].length <= 180 ? normalized : `${[...normalized].slice(0, 180).join("")}…`;
}

function boundedMultiline(value) {
  const lines = value.trim().split("\n");
  const output = lines.slice(0, 24).join("\n");
  const characters = [...output];
  if (characters.length > 4_000) return `${characters.slice(0, 4_000).join("")}…`;
  return lines.length > 24 ? `${output}\n…` : output;
}

function formatValue(value) {
  if (typeof value === "string") {
    const decoded = decodeJsonString(value);
    if (decoded === value) return value;
    try { return JSON.stringify(decoded); } catch { return value; }
  }
  if (value === undefined) return "";
  try { return JSON.stringify(value); } catch { return String(value); }
}

function isObject(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
