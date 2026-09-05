import difflib
import json
import os
from pathlib import Path


workspace = Path(os.environ["NANOCODEX_EVAL_WORKSPACE"])
response = (workspace / "answer.txt").read_text()
expected = json.loads(Path("/tests/expected.json").read_text())
prefix = expected["prefix"]

if not response.startswith(prefix):
    similarity = 0.0
else:
    sampled = response.removeprefix(prefix)
    answer = expected["answer"].removeprefix(prefix)
    similarity = float(difflib.SequenceMatcher(None, sampled, answer).ratio())

Path("/logs/verifier/reward.json").write_text(
    json.dumps({"similarity": similarity}) + "\n"
)
