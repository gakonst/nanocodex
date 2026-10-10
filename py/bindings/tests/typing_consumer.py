from nanocodex import AgentEvents, Nanocodex, SessionCheckpoint, TurnResult, Usage


def consume_result(result: TurnResult) -> str:
    usage: Usage = result.usage()
    estimated = usage["estimated_cost"]
    if estimated is not None:
        _ = estimated["usd"]
    checkpoint: SessionCheckpoint = result.checkpoint()
    session_id: str = checkpoint.session_id
    family: str = checkpoint.family
    turn_id: str | None = checkpoint.turn_id
    has_conversation: bool = checkpoint.has_conversation
    _ = (session_id, family, turn_id, has_conversation)
    return checkpoint.to_json()


def consume_events(events: AgentEvents) -> None:
    event = events.recv()
    if event is not None:
        protocol_version: int = event.protocol_version
        request_id: str = event.request_id
        sequence: int = event.seq
        kind: str = event.kind
        payload: dict[str, object] = event.payload
        payload_json: str = event.payload_json
        _ = (
            protocol_version,
            request_id,
            sequence,
            kind,
            payload,
            payload_json,
        )


def owned_lifecycle(api_key: str, encoded: str) -> None:
    checkpoint = SessionCheckpoint.from_json(encoded)
    agent, events = Nanocodex(
        api_key,
        instructions="Preserve exact identifiers and run relevant tests.",
        resume=checkpoint,
    )
    turn = agent.prompt("Inspect the parser failure.")
    turn.steer("Keep the public grammar unchanged.")
    result = turn.result()
    branch, branch_events = agent.fork(result)
    replay, replay_events = agent.fork(at=checkpoint)
    latest, latest_events = agent.fork()
    _ = consume_result(result)
    consume_events(events)
    for child in (branch, replay, latest):
        child.shutdown()
    agent.shutdown()
    _ = (branch_events, replay_events, latest_events)
