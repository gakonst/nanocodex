import importlib.util
import json
import os
from pathlib import Path


spec = importlib.util.spec_from_file_location("ale_score_outputs", "/tests/score_outputs.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

workspace = Path(os.environ["NANOCODEX_EVAL_WORKSPACE"])
report = module.score(workspace / "base/output", Path("/tests/reference"))
score = float(report.get("score", 0.0))

logs = Path("/logs/verifier")
logs.mkdir(parents=True, exist_ok=True)
(logs / "ale-score.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
(logs / "reward.json").write_text(json.dumps({"ale_score": score}) + "\n")
