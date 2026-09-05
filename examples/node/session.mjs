/**
 * Runs two follow-on prompts and closes every owned lifecycle handle.
 *
 * Long-lived applications normally retain the agent. This short-lived example
 * joins the agent after copying and releasing each typed result.
 */
export async function runOwnedSession(
  agent,
  {
    log = console.log,
    logDiagnostic = console.error,
  } = {},
) {
  const watch = agent.events.watch();
  const unwatch = watch.onEvent((event) => {
    if (event.type === "tool.call") {
      logDiagnostic(`tool: ${event.payload.tool}`);
    }
  });
  const turns = [];
  const unfinishedTurns = new Set();

  try {
    const first = agent.turn.prompt({
      input: "Use multiply to calculate 6 × 7. Return only the number.",
    });
    turns.push(first);
    unfinishedTurns.add(first);
    let firstResult;
    let firstMessage;
    try {
      firstResult = await first.result();
      firstMessage = firstResult.finalMessage;
    } finally {
      try {
        firstResult?.dispose();
      } finally {
        firstResult = undefined;
        unfinishedTurns.delete(first);
      }
    }
    log("first:", firstMessage);

    // Follow-on state, response IDs, and prompt-cache identity stay in Rust.
    const second = agent.turn.prompt({
      input: "Add one to that result. Return only the number.",
    });
    turns.push(second);
    unfinishedTurns.add(second);
    let secondResult;
    let secondMessage;
    try {
      secondResult = await second.result();
      secondMessage = secondResult.finalMessage;
    } finally {
      try {
        secondResult?.dispose();
      } finally {
        secondResult = undefined;
        unfinishedTurns.delete(second);
      }
    }
    log("second:", secondMessage);

    return { first: firstMessage, second: secondMessage };
  } finally {
    await Promise.allSettled([...unfinishedTurns].map((turn) => turn.cancel()));
    for (const turn of turns) turn.dispose();
    unwatch();
    watch.off();
    await agent.session.shutdown();
  }
}
