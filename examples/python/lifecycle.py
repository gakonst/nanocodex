"""Demonstrate Python steer, cancel, spawn, and fork controls."""

from __future__ import annotations

import os
import threading
import time

from nanocodex import Nanocodex


def main() -> None:
    api_key = os.environ["OPENAI_API_KEY"]
    agent, _ = Nanocodex(api_key, thinking="low")

    turn = agent.prompt(
        "Count slowly from 1 to 20 in words. Do not stop early unless steered."
    )

    def steer_later() -> None:
        time.sleep(1.5)
        turn.steer("Stop counting and reply with only the word STEERED.")

    threading.Thread(target=steer_later, daemon=True).start()
    print("steered:", turn.result().final_message)

    sibling, _ = agent.spawn()
    print(
        "spawned:",
        sibling.prompt("Reply with only SPAWNED.").result().final_message,
    )

    parent = agent.prompt("Remember the token FORK_PY. Reply with OK.")
    parent_result = parent.result()
    print("parent:", parent_result.final_message)
    branch, _ = agent.fork_from(parent_result)
    print(
        "forked:",
        branch.prompt("What token did I ask you to remember?").result().final_message,
    )
    branch.shutdown()
    sibling.shutdown()
    agent.shutdown()


if __name__ == "__main__":
    main()
