import json
import os
import subprocess
import sys
from pathlib import Path


workspace = Path(os.environ["NANOCODEX_EVAL_WORKSPACE"])
logs = Path(os.environ["NANOCODEX_EVAL_VERIFIER_LOGS"])
completed = subprocess.run(
    [
        sys.executable,
        "/tests/reference_grader.py",
        "/tests/eval_config.json",
        str(workspace / "answer.txt"),
    ],
    check=False,
    capture_output=True,
    text=True,
)
(logs / "official-grader.stdout.json").write_text(completed.stdout)
(logs / "official-grader.stderr.txt").write_text(completed.stderr)

passed = False
if completed.returncode == 0:
    try:
        result = json.loads(completed.stdout)
        passed = result.get("passed") is True
    except (json.JSONDecodeError, AttributeError):
        pass

(logs / "reward.json").write_text(json.dumps({"reward": 1.0 if passed else 0.0}) + "\n")
