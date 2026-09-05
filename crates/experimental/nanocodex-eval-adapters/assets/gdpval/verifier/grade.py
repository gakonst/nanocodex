import json
import gzip
import os
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path


PAIRWISE_PROMPT = """You are evaluating two anonymous professional work submissions against the original task and its human-authored rubric.

Return one JSON object with exactly these fields:
- "winner": one of "A", "B", or "tie".
- "explanation": a concise evidence-based comparison.
- "rubric_decisions": an array containing exactly one object for every rubric item, in the supplied order. Each object must have exactly "rubric_item_id", "a_score", "b_score", and "explanation". Scores must be numbers between 0 and 1.

Judge only the supplied artifacts. Missing, malformed, or unusable deliverables count against that submission. Negative-point rubric items describe undesirable behavior; a higher per-item score means the submission better avoids that behavior.

# Original task
<<TASK>>

# Rubric
<<RUBRIC>>

# Submission A
<<A>>

# Submission B
<<B>>
"""

RUBRIC_PROMPT = """You are evaluating one professional work submission against the original task and its human-authored rubric. No expert submission is available.

Return one JSON object with exactly these fields:
- "explanation": a concise evidence-based evaluation.
- "rubric_decisions": an array containing exactly one object for every rubric item, in the supplied order. Each object must have exactly "rubric_item_id", "criteria_met", and "explanation". "criteria_met" must be boolean. For a negative-point item, true means the undesirable behavior is present.

Judge only the supplied artifacts. Missing, malformed, or unusable deliverables count against the submission.

# Original task
<<TASK>>

# Rubric
<<RUBRIC>>

# Submission
<<SUBMISSION>>
"""

TEXT_EXTENSIONS = {
    ".csv",
    ".html",
    ".ipynb",
    ".json",
    ".md",
    ".overpassql",
    ".py",
    ".sql",
    ".txt",
    ".xml",
    ".yaml",
    ".yml",
}
OFFICE_EXTENSIONS = {
    ".doc",
    ".docx",
    ".odp",
    ".ods",
    ".odt",
    ".ppt",
    ".pptx",
    ".xls",
    ".xlsx",
}
MAX_ARTIFACT_CHARS = 120_000
MAX_SUBMISSION_CHARS = 240_000


def response_text(response):
    return "".join(
        part.get("text", "")
        for item in response.get("output", [])
        for part in item.get("content", [])
        if part.get("type") == "output_text"
    )


def parse_document(text):
    cleaned = re.sub(r"^```(?:json)?\s*|\s*```$", "", text.strip(), flags=re.IGNORECASE)
    document = json.loads(cleaned)
    if not isinstance(document, dict):
        raise ValueError("judge response is not an object")
    return document


def call_judge(prompt, model, validator, rubric_ids):
    attempts = []
    last_error = None
    for attempt in range(1, 4):
        response_document = {}
        text = ""
        poll_errors = []
        uncompressed = json.dumps(
            {
                "model": model,
                "input": [{"role": "user", "content": prompt}],
            }
        ).encode()
        body = gzip.compress(uncompressed, compresslevel=1)
        print(
            json.dumps(
                {
                    "judge_request": {
                        "attempt": attempt,
                        "uncompressed_bytes": len(uncompressed),
                        "compressed_bytes": len(body),
                    }
                }
            ),
            flush=True,
        )
        request = urllib.request.Request(
            f"{os.environ['NANOCODEX_JUDGE_BASE_URL']}/responses/async",
            data=body,
            headers={
                "Authorization": f"Bearer {os.environ['NANOCODEX_JUDGE_TOKEN']}",
                "Content-Type": "application/json",
                "Content-Encoding": "gzip",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                submitted = json.load(response)
            response_id = submitted["id"]
            response_document = submitted
            deadline = time.monotonic() + 600
            while True:
                poll = urllib.request.Request(
                    f"{os.environ['NANOCODEX_JUDGE_BASE_URL']}/responses/async/{response_id}",
                    headers={
                        "Authorization": f"Bearer {os.environ['NANOCODEX_JUDGE_TOKEN']}"
                    },
                )
                try:
                    with urllib.request.urlopen(poll, timeout=30) as response:
                        response_document = json.load(response)
                except urllib.error.HTTPError:
                    raise
                except OSError as error:
                    poll_errors.append(f"{type(error).__name__}: {error}")
                    if time.monotonic() >= deadline:
                        raise TimeoutError(
                            f"judge polling did not recover: {poll_errors[-1]}"
                        ) from error
                    time.sleep(2)
                    continue
                if response_document.get("status") == "completed":
                    break
                if response_document.get("status") != "in_progress":
                    raise ValueError("judge job returned an unexpected status")
                if time.monotonic() >= deadline:
                    raise TimeoutError("judge job did not complete within 600 seconds")
                time.sleep(2)
            text = response_text(response_document)
            document = parse_document(text)
            validator(document, rubric_ids)
            attempts.append(
                {
                    "attempt": attempt,
                    "response_id": response_document.get("id"),
                    "text": text,
                    "poll_errors": poll_errors,
                    "error": None,
                }
            )
            return document, attempts
        except (OSError, ValueError) as error:
            last_error = error
            attempts.append(
                {
                    "attempt": attempt,
                    "response_id": response_document.get("id"),
                    "text": text,
                    "poll_errors": poll_errors,
                    "error": f"{type(error).__name__}: {error}",
                }
            )
            print(json.dumps({"judge_attempt_failed": attempts[-1]}), flush=True)
    raise RuntimeError(f"GDPval judge failed after 3 attempts: {last_error}")


def truncate(value):
    if len(value) <= MAX_ARTIFACT_CHARS:
        return value
    return value[:MAX_ARTIFACT_CHARS] + "\n[artifact text truncated by public reproduction grader]"


def command_text(command):
    result = subprocess.run(command, check=True, capture_output=True, text=True, timeout=120)
    return result.stdout


def artifact_text(path):
    suffix = path.suffix.lower()
    try:
        if suffix in TEXT_EXTENSIONS:
            return truncate(path.read_text(errors="replace"))
        if suffix == ".pdf":
            return truncate(command_text(["pdftotext", str(path), "-"]))
        if suffix in OFFICE_EXTENSIONS:
            with tempfile.TemporaryDirectory() as temporary:
                subprocess.run(
                    [
                        "libreoffice",
                        "--headless",
                        "--convert-to",
                        "pdf",
                        "--outdir",
                        temporary,
                        str(path),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=180,
                )
                converted = list(Path(temporary).glob("*.pdf"))
                if len(converted) != 1:
                    raise RuntimeError("LibreOffice did not produce one PDF")
                return truncate(command_text(["pdftotext", str(converted[0]), "-"]))
        kind = command_text(["file", "--brief", str(path)]).strip()
        return f"[binary artifact: {kind}; {path.stat().st_size} bytes]"
    except (OSError, subprocess.SubprocessError, RuntimeError) as error:
        return f"[artifact extraction failed: {type(error).__name__}: {error}]"


def render_submission(root, deliverables):
    rendered = []
    missing = []
    remaining = MAX_SUBMISSION_CHARS
    for deliverable in deliverables:
        name = Path(deliverable["path"]).name
        path = root / name
        if not path.is_file():
            missing.append(name)
            content = "[MISSING]"
        else:
            content = artifact_text(path)
        retained = content[:remaining]
        rendered.append(f"## {name}\n{retained}")
        remaining -= len(retained)
        if len(retained) != len(content):
            rendered.append("[submission text truncated by public reproduction grader]")
        if remaining == 0:
            break
    return "\n\n".join(rendered), missing


def render_workspace(root):
    rendered = []
    files = []
    remaining = MAX_SUBMISSION_CHARS
    truncated = False
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(root)
        if (
            relative in {Path("Dockerfile"), Path("instruction.md")}
            or relative.parts[:1] in {("reference_files",), (".git",), (".nanocodex",)}
        ):
            continue
        files.append(str(relative))
        if remaining == 0:
            truncated = True
            continue
        content = artifact_text(path)
        retained = content[:remaining]
        rendered.append(f"## {relative}\n{retained}")
        remaining -= len(retained)
        truncated = truncated or len(retained) != len(content)
    if truncated:
        rendered.append("[submission text truncated by public reproduction grader]")
    return "\n\n".join(rendered), files


def validate_pairwise(document, rubric_ids):
    if set(document) != {"winner", "explanation", "rubric_decisions"}:
        raise ValueError("pairwise judge response has unexpected fields")
    if document["winner"] not in {"A", "B", "tie"} or not isinstance(
        document["explanation"], str
    ):
        raise ValueError("pairwise judge response has invalid verdict")
    decisions = document["rubric_decisions"]
    if not isinstance(decisions, list) or len(decisions) != len(rubric_ids):
        raise ValueError("pairwise judge response has wrong rubric count")
    for decision, rubric_id in zip(decisions, rubric_ids):
        if set(decision) != {"rubric_item_id", "a_score", "b_score", "explanation"}:
            raise ValueError("pairwise rubric decision has unexpected fields")
        if decision["rubric_item_id"] != rubric_id:
            raise ValueError("pairwise rubric decisions are out of order")
        if not all(
            isinstance(decision[key], (int, float)) and 0 <= decision[key] <= 1
            for key in ("a_score", "b_score")
        ) or not isinstance(decision["explanation"], str):
            raise ValueError("pairwise rubric decision has invalid values")


def validate_rubric(document, rubric_ids):
    if set(document) != {"explanation", "rubric_decisions"}:
        raise ValueError("rubric judge response has unexpected fields")
    if not isinstance(document["explanation"], str):
        raise ValueError("rubric judge explanation is not text")
    decisions = document["rubric_decisions"]
    if not isinstance(decisions, list) or len(decisions) != len(rubric_ids):
        raise ValueError("rubric judge response has wrong rubric count")
    for decision, rubric_id in zip(decisions, rubric_ids):
        if set(decision) != {"rubric_item_id", "criteria_met", "explanation"}:
            raise ValueError("rubric decision has unexpected fields")
        if (
            decision["rubric_item_id"] != rubric_id
            or not isinstance(decision["criteria_met"], bool)
            or not isinstance(decision["explanation"], str)
        ):
            raise ValueError("rubric decision has invalid values")


def pairwise_score(winner, candidate_label):
    if winner == "tie":
        return 0.5
    return 1.0 if winner == candidate_label else 0.0


# Generated workspace-output tasks stage the candidate tree at this canonical
# path even when the isolated verifier image has a different working directory.
workspace = Path("/workspace")
logs = Path(os.environ.get("NANOCODEX_EVAL_VERIFIER_LOGS", "/logs/verifier"))
logs.mkdir(parents=True, exist_ok=True)
case = json.loads(Path(os.environ.get("NANOCODEX_EVAL_CASE", "/tests/case.json")).read_text())
model = os.environ.get("GDPVAL_JUDGE", case["scoring"]["grader_model"])
rubric_json = json.dumps(case["rubric_items"], indent=2)
rubric_ids = [item["rubric_item_id"] for item in case["rubric_items"]]
candidate, candidate_files = render_workspace(workspace)
expert_root = Path(os.environ.get("NANOCODEX_EVAL_EXPERT", "/tests/expert"))

evidence = {
    "benchmark": "gdpval-public-reproduction",
    "case_id": case["task_id"],
    "judge_model": model,
    "judge_reasoning_effort": case["scoring"]["grader_reasoning_effort"],
    "candidate_deliverables": candidate_files,
    "method": None,
    "judgments": [],
}

if not candidate_files:
    score = 0.0
    evidence["method"] = "deterministic-no-deliverables"
elif case["deliverables"]:
    expert, missing_expert = render_submission(expert_root, case["deliverables"])
    if missing_expert:
        raise RuntimeError(f"GDPval verifier package is missing expert deliverables: {missing_expert}")
    score = 0.0
    for candidate_label, submission_a, submission_b in (
        ("B", expert, candidate),
        ("A", candidate, expert),
    ):
        prompt = (
            PAIRWISE_PROMPT.replace("<<TASK>>", case["prompt"])
            .replace("<<RUBRIC>>", rubric_json)
            .replace("<<A>>", submission_a)
            .replace("<<B>>", submission_b)
        )
        judgment, attempts = call_judge(prompt, model, validate_pairwise, rubric_ids)
        order_score = pairwise_score(judgment["winner"], candidate_label)
        evidence["judgments"].append(
            {
                "candidate_label": candidate_label,
                "score": order_score,
                "judgment": judgment,
                "attempts": attempts,
            }
        )
        score += order_score / 2.0
    evidence["method"] = "order-swapped-pairwise"
else:
    prompt = (
        RUBRIC_PROMPT.replace("<<TASK>>", case["prompt"])
        .replace("<<RUBRIC>>", rubric_json)
        .replace("<<SUBMISSION>>", candidate)
    )
    judgment, attempts = call_judge(prompt, model, validate_rubric, rubric_ids)
    achieved = 0
    positive = sum(item["score"] for item in case["rubric_items"] if item["score"] > 0)
    for rubric, decision in zip(case["rubric_items"], judgment["rubric_decisions"]):
        if decision["criteria_met"]:
            achieved += rubric["score"]
    score = achieved / positive
    evidence["method"] = "rubric-only-no-expert"
    evidence["achieved_points"] = achieved
    evidence["positive_points"] = positive
    evidence["judgments"].append({"judgment": judgment, "attempts": attempts})

evidence["public_score"] = score
(logs / "judgments.json").write_text(json.dumps(evidence, indent=2))
(logs / "reward.json").write_text(json.dumps({"public_score": score}) + "\n")
