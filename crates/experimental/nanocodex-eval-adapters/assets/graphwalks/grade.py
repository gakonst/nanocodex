import json
import os
import re
from pathlib import Path


def extract(response: str) -> tuple[list[str], bool]:
    # This intentionally preserves the extractor published by OpenAI for the
    # pinned public dataset, including its current prefix handling.
    line = response.split("\n")[-1]
    if "Final Answer:" not in line:
        return [], True
    list_part = re.search(r"Final Answer: ?\[.*\]", line)
    if list_part:
        result = list_part.group(0).strip("[]").split(",")
        return [item.strip() for item in result if item.strip()], False
    return [], True


workspace = Path(os.environ["NANOCODEX_EVAL_WORKSPACE"])
response = (workspace / "answer.txt").read_text()
truth = set(json.loads(Path("/tests/expected.json").read_text()))
sampled, extraction_error = extract(response)
sampled = set(sampled)

if extraction_error:
    f1 = 0.0
else:
    overlap = len(sampled & truth)
    recall = overlap / len(truth) if truth else 0.0
    precision = overlap / len(sampled) if sampled else 0.0
    f1 = 2 * recall * precision / (recall + precision) if recall + precision else 1.0

Path("/logs/verifier/reward.json").write_text(json.dumps({"f1": f1}) + "\n")
