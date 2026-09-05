import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import {
  MULTIPLAYER_MAX_MESSAGE_BYTES,
  MULTIPLAYER_ROOM_ENDED_CLOSE_CODE,
  MultiplayerProtocolError,
  clearMultiplayerCreateAttempt,
  clearMultiplayerJoinAttempt,
  clearMultiplayerPendingSend,
  createMultiplayerCreateAttempt,
  createMultiplayerJoinAttempt,
  createMultiplayerPendingSend,
  createMultiplayerRoomState,
  decodeMultiplayerMessage,
  multiplayerInvitation,
  multiplayerInviteUrl,
  multiplayerPendingSendSettled,
  multiplayerRoomPath,
  multiplayerSocketUrl,
  readMultiplayerCreateAttempt,
  readMultiplayerJoinAttempt,
  readMultiplayerPendingSend,
  reduceMultiplayerMessage,
  writeMultiplayerCreateAttempt,
  writeMultiplayerJoinAttempt,
  writeMultiplayerPendingSend,
  type MultiplayerCreateAttempt,
  type MultiplayerJoinAttempt,
  type MultiplayerPendingSend,
  type MultiplayerRoomState,
  type MultiplayerTarget,
} from "./multiplayerProtocol";
import "./Multiplayer.css";

type LobbyState =
  | { kind: "create"; error?: string }
  | { kind: "join"; roomId: string; invite: string; error?: string }
  | { kind: "resume"; roomId: string }
  | { kind: "ended"; roomId: string }
  | { kind: "blocked"; roomId: string; error: string };

type RoomReceipt = {
  roomId: string;
  memberId: string;
  invite?: string;
};

type PendingRoom = Omit<RoomReceipt, "memberId"> & { memberId?: string; inviteUrl?: string };

const encoder = new TextEncoder();
const RECONNECT_DELAYS = [500, 1_000, 2_000, 4_000, 8_000];
const TRANSCRIPT_FOLLOW_THRESHOLD_PX = 80;

export function Multiplayer() {
  const initial = useRef(multiplayerInvitation(new URL(window.location.href))).current;
  const initialCreateAttempt = useRef(!initial.roomId
    ? readMultiplayerCreateAttempt(window.sessionStorage)
    : undefined).current;
  const initialJoinAttempt = useRef(initial.roomId && initial.invite
    ? readMultiplayerJoinAttempt(window.sessionStorage, initial.roomId, initial.invite)
    : undefined).current;
  const initialPendingSend = useRef(initial.roomId
    ? readMultiplayerPendingSend(window.sessionStorage, initial.roomId)
    : undefined).current;
  const [lobby, setLobby] = useState<LobbyState>(() => initial.roomId && initial.invite
    ? { kind: "join", roomId: initial.roomId, invite: initial.invite }
    : initial.roomId
      ? { kind: "resume", roomId: initial.roomId }
      : { kind: "create" });
  const [displayName, setDisplayName] = useState(() => (
    initialJoinAttempt?.displayName
    ?? initialCreateAttempt?.displayName
    ?? readDisplayName()
  ));
  const [pending, setPending] = useState(false);
  const [room, setRoom] = useState<MultiplayerRoomState>();
  const [connected, setConnected] = useState(false);
  const [roomEnded, setRoomEnded] = useState(false);
  const [draft, setDraft] = useState("");
  const [target, setTarget] = useState<MultiplayerTarget>("room");
  const [roomError, setRoomError] = useState<string>();
  const [inviteCopied, setInviteCopied] = useState(false);
  const [endingRoom, setEndingRoom] = useState(false);
  const [unreadCount, setUnreadCount] = useState(0);
  const [pendingSend, setPendingSend] = useState<MultiplayerPendingSend | undefined>(initialPendingSend);
  const roomRef = useRef<MultiplayerRoomState | undefined>(undefined);
  const socketRef = useRef<WebSocket | undefined>(undefined);
  const socketGeneration = useRef(0);
  const reconnectTimer = useRef<number | undefined>(undefined);
  const reconnectAttempt = useRef(0);
  const pendingRoomRef = useRef<PendingRoom | undefined>(undefined);
  const createAttemptRef = useRef<MultiplayerCreateAttempt | undefined>(initialCreateAttempt);
  const joinAttemptRef = useRef<MultiplayerJoinAttempt | undefined>(initialJoinAttempt);
  const pendingSendRef = useRef<MultiplayerPendingSend | undefined>(initialPendingSend);
  const mounted = useRef(true);
  const lifecycleAbort = useRef(new AbortController());
  const transcriptRef = useRef<HTMLOListElement>(null);
  const followTranscript = useRef(true);
  const transcriptSnapshot = useRef<{ roomId?: string; timelineLength: number }>({ timelineLength: 0 });

  const commitRoom = useCallback((next: MultiplayerRoomState) => {
    const transcript = transcriptRef.current;
    if (transcript) followTranscript.current = isNearTranscriptEnd(transcript);
    roomRef.current = next;
    setRoom(next);
  }, []);

  const forgetPendingSend = useCallback((pendingCommand: MultiplayerPendingSend) => {
    clearMultiplayerPendingSend(window.sessionStorage, pendingCommand);
    const current = pendingSendRef.current;
    if (current?.roomId === pendingCommand.roomId
      && current.memberId === pendingCommand.memberId
      && current.id === pendingCommand.id
      && current.encoded === pendingCommand.encoded) {
      pendingSendRef.current = undefined;
      setPendingSend(undefined);
    }
  }, []);

  const markRoomEnded = useCallback((roomId: string) => {
    socketGeneration.current++;
    window.clearTimeout(reconnectTimer.current);
    socketRef.current = undefined;
    setConnected(false);
    setPending(false);
    setRoomEnded(true);
    setRoomError(undefined);
    setLobby({ kind: "ended", roomId });
  }, []);

  const resendPendingSend = useCallback((
    socket: WebSocket,
    roomId: string,
    memberId: string,
  ) => {
    let pendingCommand = pendingSendRef.current;
    if (!pendingCommand || pendingCommand.roomId !== roomId) {
      pendingCommand = readMultiplayerPendingSend(window.sessionStorage, roomId);
    }
    if (!pendingCommand) return;
    pendingSendRef.current = pendingCommand;
    setPendingSend(pendingCommand);
    if (pendingCommand.memberId !== memberId) {
      forgetPendingSend(pendingCommand);
      setRoomError("A pending command belonged to a different room membership and was not resent.");
      return;
    }
    try {
      socket.send(pendingCommand.encoded);
    } catch {
      setRoomError("The pending command remains saved and will be retried after reconnecting.");
    }
  }, [forgetPendingSend]);

  const connect = useCallback((receipt: PendingRoom, isReconnect = false) => {
    if (!mounted.current) return;
    window.clearTimeout(reconnectTimer.current);
    setRoomEnded(false);
    const generation = ++socketGeneration.current;
    socketRef.current?.close(1000, "replaced connection");
    const retained = roomRef.current?.roomId === receipt.roomId ? roomRef.current : undefined;
    const cursor = retained?.cursor ?? "0";
    const socket = new WebSocket(multiplayerSocketUrl(window.location.origin, receipt.roomId, cursor));
    socketRef.current = socket;
    pendingRoomRef.current = receipt;

    socket.addEventListener("message", (event) => {
      if (!mounted.current
        || generation !== socketGeneration.current
        || typeof event.data !== "string") return;
      try {
        const message = decodeMultiplayerMessage(event.data);
        if (message.type === "room_ended") {
          markRoomEnded(receipt.roomId);
          return;
        }
        if (message.type === "ready") {
          if (message.room_id !== receipt.roomId
            || (receipt.memberId !== undefined && message.member_id !== receipt.memberId)) {
            throw new MultiplayerProtocolError("room membership identity changed");
          }
          const current = roomRef.current?.roomId === receipt.roomId
            ? reduceMultiplayerMessage(roomRef.current, message)
            : createMultiplayerRoomState(message, { inviteUrl: receipt.inviteUrl });
          commitRoom(current);
          setLobby({ kind: "resume", roomId: receipt.roomId });
          setPending(false);
          setConnected(true);
          setRoomError(undefined);
          reconnectAttempt.current = 0;
          resendPendingSend(socket, message.room_id, message.member_id);
          return;
        }
        const current = roomRef.current;
        if (!current || current.roomId !== receipt.roomId) return;
        if (message.type === "replay_paused") {
          const next = reduceMultiplayerMessage(current, message);
          commitRoom(next);
          socket.send(JSON.stringify({ type: "ack", cursor: message.cursor }));
          return;
        }
        if (message.type === "error") {
          const pendingCommand = pendingSendRef.current;
          if (message.id !== undefined
            && pendingCommand
            && pendingCommand.roomId === receipt.roomId
            && message.id === pendingCommand.id) {
            forgetPendingSend(pendingCommand);
            setDraft(pendingCommand.text);
            setTarget(pendingCommand.target);
          }
          setRoomError(roomOperationError(message.code));
          return;
        }
        if (message.type === "accepted") {
          const pendingCommand = pendingSendRef.current;
          if (pendingCommand
            && pendingCommand.roomId === receipt.roomId
            && multiplayerPendingSendSettled(pendingCommand, message)) {
            forgetPendingSend(pendingCommand);
          }
          return;
        }
        const next = reduceMultiplayerMessage(current, message);
        commitRoom(next);
        const pendingCommand = pendingSendRef.current;
        if (pendingCommand
          && pendingCommand.roomId === receipt.roomId
          && multiplayerPendingSendSettled(pendingCommand, message)) {
          forgetPendingSend(pendingCommand);
        }
      } catch {
        setRoomError("The room stream was invalid. Reconnect to replay its last durable cursor.");
        socket.close(1002, "invalid room protocol");
      }
    });

    socket.addEventListener("close", (event) => {
      if (!mounted.current || generation !== socketGeneration.current) return;
      if (event.code === MULTIPLAYER_ROOM_ENDED_CLOSE_CODE) {
        markRoomEnded(receipt.roomId);
        return;
      }
      setConnected(false);
      if (!roomRef.current || roomRef.current.roomId !== receipt.roomId) {
        setPending(false);
        setLobby({
          kind: "blocked",
          roomId: receipt.roomId,
          error: "The room is unavailable or this browser's membership is invalid or expired. Retry, or use the original invite link to join again.",
        });
        return;
      }
      const attempt = reconnectAttempt.current++;
      if (attempt > 0) {
        setRoomError("The room connection was interrupted. Durable messages are retained; reconnecting is automatic.");
      }
      reconnectTimer.current = window.setTimeout(
        () => connect(receipt, true),
        RECONNECT_DELAYS[Math.min(attempt, RECONNECT_DELAYS.length - 1)],
      );
    });

    socket.addEventListener("error", () => {
      if (!mounted.current || generation !== socketGeneration.current) return;
      if (!isReconnect && !roomRef.current) {
        setRoomError(undefined);
      }
    });
  }, [commitRoom, forgetPendingSend, markRoomEnded, resendPendingSend]);

  useEffect(() => {
    mounted.current = true;
    if (lifecycleAbort.current.signal.aborted) {
      lifecycleAbort.current = new AbortController();
    }
    return () => {
      mounted.current = false;
      lifecycleAbort.current.abort();
      socketGeneration.current++;
      window.clearTimeout(reconnectTimer.current);
      socketRef.current?.close(1000, "surface closed");
    };
  }, []);

  useEffect(() => {
    if (lobby.kind !== "resume" || roomRef.current?.roomId === lobby.roomId) return;
    connect({ roomId: lobby.roomId });
  }, [connect, lobby]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      if (socketRef.current?.readyState === WebSocket.OPEN) {
        socketRef.current.send(JSON.stringify({ type: "ping" }));
      }
    }, 25_000);
    return () => window.clearInterval(timer);
  }, []);

  useLayoutEffect(() => {
    if (!room) {
      transcriptSnapshot.current = { timelineLength: 0 };
      followTranscript.current = true;
      setUnreadCount(0);
      return;
    }

    const previous = transcriptSnapshot.current;
    const newRoom = previous.roomId !== room.roomId;
    const added = newRoom
      ? room.timeline.length
      : Math.max(0, room.timeline.length - previous.timelineLength);
    transcriptSnapshot.current = { roomId: room.roomId, timelineLength: room.timeline.length };

    const transcript = transcriptRef.current;
    if (!transcript) return;
    if (newRoom || followTranscript.current) {
      followTranscript.current = true;
      transcript.scrollTop = transcript.scrollHeight;
      setUnreadCount(0);
    } else if (added > 0) {
      setUnreadCount((current) => current + added);
    }
  }, [room?.roomId, room?.timeline.length]);

  useEffect(() => {
    if (room && !room.canTargetAgent && target === "agent") setTarget("room");
  }, [room, target]);

  const createRoom = async (event: FormEvent) => {
    event.preventDefault();
    const signal = lifecycleAbort.current.signal;
    const name = displayName.trim();
    if (!name) {
      setLobby({ kind: "create", error: "Enter a display name." });
      return;
    }
    let attempt = createAttemptRef.current;
    if (attempt && attempt.displayName !== name) {
      setPending(false);
      setDisplayName(attempt.displayName);
      setLobby({
        kind: "create",
        error: `Retry the pending room creation as ${attempt.displayName} so its durable receipt stays identical.`,
      });
      return;
    }
    if (!attempt) {
      attempt = createMultiplayerCreateAttempt(name);
      if (!writeMultiplayerCreateAttempt(window.sessionStorage, attempt)) {
        setPending(false);
        setLobby({
          kind: "create",
          error: "This browser could not retain a safe room-creation receipt. Enable session storage and retry.",
        });
        return;
      }
      createAttemptRef.current = attempt;
    }
    setPending(true);
    setLobby({ kind: "create" });
    try {
      const response = await fetch("/v1/rooms", {
        method: "POST",
        credentials: "same-origin",
        headers: {
          accept: "application/json",
          "content-type": "application/json",
        },
        body: JSON.stringify({
          create_id: attempt.createId,
          display_name: attempt.displayName,
        }),
        signal,
      });
      if (!response.ok) throw new Error(createRoomError(response.status));
      const receipt = decodeRoomReceipt(await response.json<unknown>(), true);
      if (signal.aborted || !mounted.current) return;
      writeDisplayName(attempt.displayName);
      const inviteUrl = multiplayerInviteUrl(window.location.origin, receipt.roomId, receipt.invite!);
      window.history.replaceState(window.history.state, "", multiplayerRoomPath(receipt.roomId));
      clearMultiplayerCreateAttempt(window.sessionStorage, attempt);
      if (createAttemptRef.current?.createId === attempt.createId) {
        createAttemptRef.current = undefined;
      }
      connect({ ...receipt, inviteUrl });
    } catch (error) {
      if (signal.aborted || !mounted.current) return;
      setPending(false);
      setLobby({
        kind: "create",
        error: error instanceof Error ? error.message : "The room could not be created.",
      });
    }
  };

  const joinRoom = async (event: FormEvent) => {
    event.preventDefault();
    if (lobby.kind !== "join") return;
    const signal = lifecycleAbort.current.signal;
    const name = displayName.trim();
    if (!name) {
      setLobby({ ...lobby, error: "Enter a display name." });
      return;
    }
    setPending(true);
    setLobby({ ...lobby, error: undefined });
    let attempt = joinAttemptRef.current;
    if (attempt
      && (attempt.roomId !== lobby.roomId || attempt.invite !== lobby.invite)) {
      attempt = undefined;
      joinAttemptRef.current = undefined;
    }
    if (attempt && attempt.displayName !== name) {
      setPending(false);
      setDisplayName(attempt.displayName);
      setLobby({
        ...lobby,
        error: `Retry the pending join as ${attempt.displayName} so its durable receipt stays identical.`,
      });
      return;
    }
    if (!attempt) {
      attempt = createMultiplayerJoinAttempt(lobby.roomId, lobby.invite, name);
      if (!writeMultiplayerJoinAttempt(window.sessionStorage, attempt)) {
        setPending(false);
        setLobby({ ...lobby, error: "This browser could not retain a safe join receipt. Enable session storage and retry." });
        return;
      }
      joinAttemptRef.current = attempt;
    }
    try {
      const response = await fetch(`/v1/rooms/${lobby.roomId}/join`, {
        method: "POST",
        credentials: "same-origin",
        headers: { accept: "application/json", "content-type": "application/json" },
        body: JSON.stringify({
          invite: attempt.invite,
          display_name: attempt.displayName,
          join_id: attempt.joinId,
        }),
        signal,
      });
      if (!response.ok) {
        if (response.status < 500) {
          clearMultiplayerJoinAttempt(window.sessionStorage, attempt);
          if (joinAttemptRef.current?.joinId === attempt.joinId) joinAttemptRef.current = undefined;
        }
        throw new Error(joinRoomError(response.status));
      }
      const receipt = decodeRoomReceipt(await response.json<unknown>(), false);
      if (signal.aborted || !mounted.current) return;
      clearMultiplayerJoinAttempt(window.sessionStorage, attempt);
      if (joinAttemptRef.current?.joinId === attempt.joinId) joinAttemptRef.current = undefined;
      writeDisplayName(attempt.displayName);
      window.history.replaceState(window.history.state, "", multiplayerRoomPath(receipt.roomId));
      connect(receipt);
    } catch (error) {
      if (signal.aborted || !mounted.current) return;
      setPending(false);
      setLobby({
        ...lobby,
        error: error instanceof Error ? error.message : "The room could not be joined.",
      });
    }
  };

  const sendMessage = (event: FormEvent) => {
    event.preventDefault();
    const text = draft.trim();
    if (!text) return;
    if (target === "agent" && !roomRef.current?.canTargetAgent) {
      setRoomError("This room does not currently allow managed-agent turns.");
      setTarget("room");
      return;
    }
    if (encoder.encode(text).byteLength > MULTIPLAYER_MAX_MESSAGE_BYTES) {
      setRoomError("Messages must be no larger than 16 KiB.");
      return;
    }
    const socket = socketRef.current;
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      setRoomError("The room is offline. Retry the connection before sending.");
      return;
    }
    if (pendingSendRef.current) {
      setRoomError("Wait for the pending message's durable receipt before sending another.");
      return;
    }
    const current = roomRef.current;
    if (!current) return;
    const pendingCommand = createMultiplayerPendingSend(
      current.roomId,
      current.memberId,
      text,
      target,
    );
    if (!writeMultiplayerPendingSend(window.sessionStorage, pendingCommand)) {
      setRoomError("This browser could not retain the command for safe retry. Enable session storage and resend.");
      return;
    }
    pendingSendRef.current = pendingCommand;
    setPendingSend(pendingCommand);
    try {
      socket.send(pendingCommand.encoded);
      setRoomError(undefined);
    } catch {
      setRoomError("The command remains saved and will be retried after reconnecting.");
    }
    setDraft("");
  };

  const handleComposerKey = (event: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
    event.preventDefault();
    event.currentTarget.form?.requestSubmit();
  };

  const handleTranscriptScroll = useCallback(() => {
    const transcript = transcriptRef.current;
    if (!transcript) return;
    followTranscript.current = isNearTranscriptEnd(transcript);
    if (followTranscript.current) setUnreadCount(0);
  }, []);

  const jumpToLatest = useCallback(() => {
    const transcript = transcriptRef.current;
    if (!transcript) return;
    followTranscript.current = true;
    transcript.scrollTop = transcript.scrollHeight;
    setUnreadCount(0);
  }, []);

  const retryRoom = () => {
    const receipt = pendingRoomRef.current;
    if (!receipt) {
      if (lobby.kind === "blocked") setLobby({ kind: "resume", roomId: lobby.roomId });
      return;
    }
    setRoomError(undefined);
    connect(receipt, true);
  };

  const leaveRoom = () => {
    socketGeneration.current++;
    window.clearTimeout(reconnectTimer.current);
    socketRef.current?.close(1000, "left room");
    socketRef.current = undefined;
    setConnected(false);
    setRoomEnded(false);
    roomRef.current = undefined;
    pendingRoomRef.current = undefined;
    const joinAttempt = joinAttemptRef.current;
    if (joinAttempt) clearMultiplayerJoinAttempt(window.sessionStorage, joinAttempt);
    joinAttemptRef.current = undefined;
    const pendingCommand = pendingSendRef.current;
    if (pendingCommand) clearMultiplayerPendingSend(window.sessionStorage, pendingCommand);
    pendingSendRef.current = undefined;
    setPendingSend(undefined);
    setRoom(undefined);
    setRoomError(undefined);
    setUnreadCount(0);
    followTranscript.current = true;
    transcriptSnapshot.current = { timelineLength: 0 };
    setLobby({ kind: "create" });
    window.history.replaceState(window.history.state, "", multiplayerRoomPath());
  };

  const endRoom = async () => {
    if (!room?.canEndRoom || endingRoom) return;
    setEndingRoom(true);
    try {
      const response = await fetch(`/v1/rooms/${room.roomId}`, {
        method: "DELETE",
        credentials: "same-origin",
        headers: { accept: "application/json" },
      });
      if (response.status !== 204 && response.status !== 404) {
        throw new Error("room_cleanup_pending");
      }
      leaveRoom();
    } catch {
      setRoomError("The room could not be ended yet. Its durable cleanup owner will keep retrying; try again shortly.");
    } finally {
      if (mounted.current) setEndingRoom(false);
    }
  };

  const copyInvite = () => {
    if (!room?.inviteUrl) return;
    void navigator.clipboard.writeText(room.inviteUrl).then(() => {
      setInviteCopied(true);
      window.setTimeout(() => setInviteCopied(false), 1_500);
    });
  };

  if (!room && lobby.kind === "resume") return null;

  if (!room) {
    return (
      <section className="multiplayer-lobby" aria-labelledby="multiplayer-title">
        <header className="multiplayer-heading">
          <div>
            <p>durable multiplayer</p>
            <h1 id="multiplayer-title">One room. Many humans. One managed agent.</h1>
          </div>
          <span>Cloudflare Durable Objects</span>
        </header>

        <div className="multiplayer-lobby-grid">
          <article className="multiplayer-lobby-card">
            {lobby.kind === "join" ? (
              <form onSubmit={joinRoom}>
                <p className="multiplayer-kicker">room invitation · {shortRoomId(lobby.roomId)}</p>
                <h2>Join the room</h2>
                <label>
                  <span>Display name</span>
                  <input
                    autoComplete="nickname"
                    maxLength={64}
                    value={displayName}
                    onChange={(event) => setDisplayName(event.target.value)}
                  />
                </label>
                {lobby.error ? <p className="multiplayer-error" role="alert">{lobby.error}</p> : null}
                <button type="submit" disabled={pending}>Join room</button>
              </form>
            ) : lobby.kind === "ended" ? (
              <div className="multiplayer-blocked">
                <p className="multiplayer-kicker">room · {shortRoomId(lobby.roomId)}</p>
                <h2>Room ended</h2>
                <p>This room has ended. Its live session is closed.</p>
                <div className="multiplayer-button-row">
                  <button type="button" onClick={leaveRoom}>Create another room</button>
                </div>
              </div>
            ) : lobby.kind === "blocked" ? (
              <div className="multiplayer-blocked">
                <p className="multiplayer-kicker">room · {shortRoomId(lobby.roomId)}</p>
                <h2>Membership required</h2>
                <p className="multiplayer-error" role="alert">{lobby.error}</p>
                <div className="multiplayer-button-row">
                  <button type="button" onClick={retryRoom}>Retry</button>
                  <button type="button" onClick={leaveRoom}>Create another room</button>
                </div>
              </div>
            ) : lobby.kind === "create" ? (
              <form onSubmit={createRoom}>
                <p className="multiplayer-kicker">host a session</p>
                <h2>Create a room</h2>
                <label>
                  <span>Display name</span>
                  <input
                    autoComplete="nickname"
                    maxLength={64}
                    value={displayName}
                    onChange={(event) => setDisplayName(event.target.value)}
                  />
                </label>
                <p className="multiplayer-form-note">
                  Room allocation is authorized server-side. No deployment or provider credential enters this page.
                </p>
                {lobby.error ? <p className="multiplayer-error" role="alert">{lobby.error}</p> : null}
                <button type="submit" disabled={pending}>Create room</button>
              </form>
            ) : null}
          </article>

          <aside className="multiplayer-boundary" aria-label="Multiplayer architecture">
            <div>
              <span>01</span>
              <h2>Room</h2>
              <p>One SQLite Durable Object commits membership, chat order, replay cursors, and the agent outbox.</p>
            </div>
            <div>
              <span>02</span>
              <h2>Agent</h2>
              <p>One private, connector-free managed agent retains WASM history. Every room member can admit a quota-bound turn.</p>
            </div>
            <div>
              <span>03</span>
              <h2>Broker</h2>
              <p>A private Worker injects OAuth or API-key credentials only into an exact upstream WebSocket.</p>
            </div>
          </aside>
        </div>
      </section>
    );
  }

  const online = new Set(roomEnded ? [] : room.onlineMemberIds);
  return (
    <section className="multiplayer-room" aria-labelledby="multiplayer-room-title">
      <header className="multiplayer-room-heading">
        <div>
          <p>durable multiplayer · {shortRoomId(room.roomId)}</p>
          <h1 id="multiplayer-room-title">Managed-agent room</h1>
        </div>
        <div className="multiplayer-room-actions">
          <span className={connected ? "is-live" : ""}><i />{roomEnded ? "ended" : connected ? "live" : "offline"}</span>
          {!roomEnded && room.inviteUrl ? (
            <button type="button" onClick={copyInvite}>{inviteCopied ? "Invite copied" : "Copy invite"}</button>
          ) : null}
          {!roomEnded && room.canEndRoom ? (
            <button type="button" disabled={endingRoom} onClick={() => void endRoom()}>End room</button>
          ) : null}
          <button type="button" onClick={leaveRoom}>Leave</button>
        </div>
      </header>

      <div className="multiplayer-room-grid">
        <div className="multiplayer-chat">
          <ol
            ref={transcriptRef}
            aria-live="polite"
            aria-label="Room transcript"
            onScroll={handleTranscriptScroll}
          >
            {room.timeline.map((item) => (
              <TimelineItem
                key={item.cursor}
                item={item}
                ownMemberId={room.memberId}
              />
            ))}
          </ol>
          {unreadCount > 0 ? (
            <button
              className="multiplayer-unread"
              type="button"
              aria-live="polite"
              onClick={jumpToLatest}
            >
              {unreadCount} unread {unreadCount === 1 ? "update" : "updates"} · Jump to latest
            </button>
          ) : null}
          {roomEnded ? (
            <div className="multiplayer-room-error is-terminal" role="status">
              <span>This room has ended. The live session is closed.</span>
              <button type="button" onClick={leaveRoom}>Create another room</button>
            </div>
          ) : roomError ? (
            <div className="multiplayer-room-error" role="alert">
              <span>{roomError}</span>
              {!connected ? <button type="button" onClick={retryRoom}>Retry</button> : null}
            </div>
          ) : null}
          {!roomEnded ? <form className="multiplayer-composer" onSubmit={sendMessage}>
            <div className="multiplayer-target" aria-label="Message target">
              <button
                className={target === "room" ? "is-active" : ""}
                type="button"
                aria-pressed={target === "room"}
                onClick={() => setTarget("room")}
              >
                Room
              </button>
              {room.canTargetAgent ? (
                <button
                  className={target === "agent" ? "is-active" : ""}
                  type="button"
                  aria-pressed={target === "agent"}
                  onClick={() => setTarget("agent")}
                >
                  Ask agent
                </button>
              ) : null}
            </div>
            <textarea
              aria-label={target === "agent" ? "Message the room and ask Nanocodex" : "Message the room"}
              placeholder={target === "agent" ? "Ask Nanocodex in the shared room" : "Message everyone"}
              rows={3}
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={handleComposerKey}
              disabled={!connected || pendingSend !== undefined}
            />
            <button type="submit" disabled={!connected || pendingSend !== undefined || !draft.trim()}>Send</button>
          </form> : null}
        </div>

        <aside className="multiplayer-sidebar">
          <section aria-labelledby="multiplayer-members-title">
            <header>
              <p>participants</p>
              <strong>{roomEnded ? "room ended" : `${room.onlineMemberIds.length + 1} online`}</strong>
            </header>
            <h2 className="sr-only" id="multiplayer-members-title">Room participants</h2>
            <ul>
              <li className={roomEnded ? "is-agent" : "is-online is-agent"}>
                <i />
                <span><strong>Nanocodex</strong><small>managed agent</small></span>
              </li>
              {room.members.map((member) => (
                <li className={online.has(member.id) ? "is-online" : ""} key={member.id}>
                  <i />
                  <span>
                    <strong>{member.name}{member.id === room.memberId ? " · you" : ""}</strong>
                    <small>{online.has(member.id) ? "in room" : "offline"}</small>
                  </span>
                </li>
              ))}
            </ul>
          </section>
          <section className="multiplayer-credential-boundary" aria-labelledby="credential-boundary-title">
            <header>
              <p>credential boundary</p>
              <strong>Per-user broker</strong>
            </header>
            <h2 id="credential-boundary-title">Connectors disabled</h2>
            <p>
              Shared-room tools receive no account connector capability. GitHub, Gmail, and Google
              Drive calls fail closed for every participant, including the room owner.
            </p>
            <dl>
              <div><dt>Browser</dt><dd>room cookie</dd></div>
              <div><dt>Agent</dt><dd>public tools only</dd></div>
              <div><dt>Broker</dt><dd>no connector grant</dd></div>
            </dl>
          </section>
        </aside>
      </div>
    </section>
  );
}

function isNearTranscriptEnd(transcript: HTMLOListElement): boolean {
  const remaining = transcript.scrollHeight - transcript.clientHeight - transcript.scrollTop;
  return remaining <= TRANSCRIPT_FOLLOW_THRESHOLD_PX;
}

function TimelineItem({
  item,
  ownMemberId,
}: {
  item: MultiplayerRoomState["timeline"][number];
  ownMemberId: string;
}) {
  const event = item.event;
  if (event.type === "member_joined") {
    return <li className="multiplayer-system"><span>{event.member.name} joined the room</span></li>;
  }
  if (event.type === "agent_error") {
    const message = event.code === "rate_limited"
      ? "The deployment-wide managed-agent budget is temporarily exhausted."
      : `Nanocodex could not complete that room turn (${event.code}).`;
    return (
      <li className="multiplayer-system is-error">
        <span>{message}</span>
      </li>
    );
  }
  const agent = event.type === "agent_message";
  const own = event.type === "member_message" && event.member.id === ownMemberId;
  const name = agent ? "Nanocodex" : event.member.name;
  const text = event.text;
  return (
    <li className={`multiplayer-message${agent ? " is-agent" : ""}${own ? " is-own" : ""}`}>
      <header>
        <strong>{name}</strong>
        {event.type === "member_message" && event.target === "agent" ? <span>asked agent</span> : null}
        <time dateTime={new Date(item.createdAt).toISOString()}>{formatTime(item.createdAt)}</time>
      </header>
      <p>{text}</p>
    </li>
  );
}

function decodeRoomReceipt(value: unknown, creator: boolean): RoomReceipt {
  const receipt = asRecord(value);
  const roomId = receipt?.room_id;
  const memberId = receipt?.member_id;
  const invite = receipt?.invite;
  if (typeof roomId !== "string" || typeof memberId !== "string"
    || (creator && typeof invite !== "string")) {
    throw new Error("The room returned an invalid creation receipt.");
  }
  multiplayerSocketUrl(window.location.origin, roomId, "0");
  return {
    roomId,
    memberId,
    ...(typeof invite === "string" ? { invite } : {}),
  };
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function createRoomError(status: number): string {
  if (status === 401) return "The deployment rejected room allocation.";
  if (status === 409) return "That room-creation receipt conflicts with an earlier attempt in this tab.";
  if (status === 429) return "Room creation is temporarily limited. Try again later.";
  if (status === 503) return "The managed Multiplayer deployment is unavailable.";
  return "The room could not be created.";
}

function joinRoomError(status: number): string {
  if (status === 401) return "This invite is invalid or no longer available.";
  if (status === 404) return "This room is no longer available.";
  if (status === 409) return "That join receipt conflicts with an earlier attempt in this tab.";
  if (status === 410) return "This invite has expired or reached its use limit.";
  if (status === 429) return "This room has reached its member limit.";
  return "The room could not be joined.";
}

function roomOperationError(code: string): string {
  if (code === "agent_queue_full") return "The managed agent queue is full. Let its current room turns finish first.";
  if (code === "owner_required") return "Only the room host can end this room.";
  if (code === "agent_rate_limited") return "The room's managed-agent budget is temporarily exhausted.";
  if (code === "agent_capacity_unavailable") return "The deployment-wide managed-agent budget is temporarily unavailable.";
  if (code === "chat_rate_limited") return "Room chat is temporarily rate limited. Wait before sending again.";
  if (code === "message_id_conflict") return "That message conflicted with an earlier durable receipt. Send it again.";
  if (code === "event_log_full") return "This demo room reached its durable event capacity. Create another room.";
  return "The room rejected that operation. Review the message and try again.";
}

function readDisplayName(): string {
  try {
    return localStorage.getItem("nanocodex-multiplayer-name") ?? "";
  } catch {
    return "";
  }
}

function writeDisplayName(name: string): void {
  try {
    localStorage.setItem("nanocodex-multiplayer-name", name);
  } catch {
    // A display name is optional convenience state, never a room capability.
  }
}

function shortRoomId(roomId: string): string {
  return roomId.slice(0, 8);
}

function formatTime(createdAt: number): string {
  return new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" }).format(createdAt);
}
