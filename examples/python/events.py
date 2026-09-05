import os
import threading

from nanocodex import Nanocodex

agent, events = Nanocodex(os.environ["OPENAI_API_KEY"])


def print_events() -> None:
    while event := events.recv():
        print(event.seq, event.kind, event.payload, flush=True)


event_thread = threading.Thread(target=print_events)
event_thread.start()
turn = agent.prompt("Inspect this repository and summarize it.")
print(turn.result().final_message)
agent.shutdown()
event_thread.join()
