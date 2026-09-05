import json
import os
from pathlib import Path

workspace = Path(os.environ["NANOCODEX_EVAL_WORKSPACE"])
answer = (workspace / "answer.txt").read_text()
expected = json.loads(Path("/tests/expected.json").read_text())
mode = Path("/tests/mode").read_text().strip()

if not isinstance(expected, list):
    expected = [expected]

if mode == "match":
    passed = any(answer.startswith(value) for value in expected)
elif mode == "includes":
    passed = any(value in answer for value in expected)
elif mode == "includes_ignore_case":
    passed = any(value.lower() in answer.lower() for value in expected)
else:
    raise RuntimeError(f"unknown OpenAI Evals mode: {mode}")

Path("/logs/verifier/reward.txt").write_text("1\n" if passed else "0\n")
