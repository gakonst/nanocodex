import os

from nanocodex import Nanocodex

agent, _ = Nanocodex(os.environ["OPENAI_API_KEY"], thinking="low")

first = agent.prompt("Choose one word for this project.")
print("first:", first.result().final_message)

# The owned Rust session already has the first result and its complete history.
second = agent.prompt("Return that word in uppercase.")
print("second:", second.result().final_message)
agent.shutdown()
