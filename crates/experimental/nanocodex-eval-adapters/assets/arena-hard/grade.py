import json
import os
import re
import urllib.request
from pathlib import Path

SYSTEM_PROMPT = """Please act as an impartial judge and evaluate the quality of the responses provided by two AI assistants to the user prompt displayed below. You will be given assistant A's answer and assistant B's answer. Your job is to evaluate which assistant's answer is better.

Begin your evaluation by generating your own answer to the prompt. You must provide your answers before judging any answers.

When evaluating the assistants' answers, compare both assistants' answers with your answer. You must identify and correct any mistakes or inaccurate information.

Then consider if the assistant's answers are helpful, relevant, and concise. Helpful means the answer correctly responds to the prompt or follows the instructions. Note when user prompt has any ambiguity or more than one interpretation, it is more helpful and appropriate to ask for clarifications or more information from the user than providing an answer based on assumptions. Relevant means all parts of the response closely connect or are appropriate to what is being asked. Concise means the response is clear and not verbose or excessive.

Then consider the creativity and novelty of the assistant's answers when needed. Finally, identify any missing important information in the assistants' answers that would be beneficial to include when responding to the user prompt.

After providing your explanation, you must output only one of the following choices as your final verdict with a label:

1. Assistant A is significantly better: [[A>>B]]
2. Assistant A is slightly better: [[A>B]]
3. Tie, relatively the same: [[A=B]]
4. Assistant B is slightly better: [[B>A]]
5. Assistant B is significantly better: [[B>>A]]

Example output: \"My final verdict is tie: [[A=B]]\"."""


def response_text(response):
    return "".join(
        part.get("text", "")
        for item in response.get("output", [])
        for part in item.get("content", [])
        if part.get("type") == "output_text"
    )


def judge(question, answer_a, answer_b):
    prompt = (
        f"<|User Prompt|>\n{question}\n\n"
        f"<|The Start of Assistant A's Answer|>\n{answer_a}\n"
        f"<|The End of Assistant A's Answer|>\n\n"
        f"<|The Start of Assistant B's Answer|>\n{answer_b}\n"
        f"<|The End of Assistant B's Answer|>"
    )
    body = json.dumps(
        {
            "model": os.environ.get("ARENA_HARD_JUDGE", "gpt-5.6-sol"),
            "temperature": 0,
            "max_output_tokens": 16000,
            "input": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": prompt},
            ],
        }
    ).encode()
    request = urllib.request.Request(
        f"{os.environ['NANOCODEX_JUDGE_BASE_URL']}/responses",
        data=body,
        headers={
            "Authorization": f"Bearer {os.environ['NANOCODEX_JUDGE_TOKEN']}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=300) as response:
        document = json.load(response)
    text = response_text(document)
    labels = re.findall(r"\[\[([AB<>=]+)\]\]", text.upper())
    if not labels:
        labels = re.findall(r"\[([AB<>=]+)\]", text.upper())
    if not labels:
        raise RuntimeError("Arena-Hard judge returned no verdict label")
    return {"label": labels[-1], "text": text, "response_id": document.get("id")}


def score_for_a(label):
    if label in {"A>B", "A>>B", "B<A", "B<<A"}:
        return 1.0
    if label in {"A=B", "B=A"}:
        return 0.5
    if label in {"B>A", "B>>A", "A<B", "A<<B"}:
        return 0.0
    raise RuntimeError(f"unknown Arena-Hard label: {label}")


workspace = Path(os.environ["NANOCODEX_EVAL_WORKSPACE"])
logs = Path(os.environ.get("NANOCODEX_EVAL_VERIFIER_LOGS", "/logs/verifier"))
logs.mkdir(parents=True, exist_ok=True)
case = json.loads(Path("/tests/case.json").read_text())
baseline = json.loads(Path("/tests/baseline.json").read_text())
baseline_answer = baseline["messages"][-1]["content"]["answer"]
candidate = (workspace / "answer.txt").read_text()

baseline_first = judge(case["prompt"], baseline_answer, candidate)
candidate_first = judge(case["prompt"], candidate, baseline_answer)
reward = (1.0 - score_for_a(baseline_first["label"]) + score_for_a(candidate_first["label"])) / 2.0
passed = reward > 0.0
evidence = {
    "judge_model": os.environ.get("ARENA_HARD_JUDGE", "gpt-5.6-sol"),
    "baseline_model": baseline["model"],
    "baseline_first": baseline_first,
    "candidate_first": candidate_first,
    "reward": reward,
}
(logs / "judgments.json").write_text(json.dumps(evidence, indent=2))
(logs / "ctrf.json").write_text(
    json.dumps(
        {
            "results": {
                "tool": {"name": "arena-hard-auto", "version": "v2"},
                "summary": {
                    "tests": 1,
                    "passed": int(passed),
                    "failed": int(not passed),
                    "pending": 0,
                    "skipped": 0,
                    "other": 0,
                },
                "tests": [
                    {
                        "name": case["uid"],
                        "status": "passed" if passed else "failed",
                        "duration": 0,
                        "extra": evidence,
                    }
                ],
            }
        },
        indent=2,
    )
)
(logs / "reward.txt").write_text(f"{reward}\n")
