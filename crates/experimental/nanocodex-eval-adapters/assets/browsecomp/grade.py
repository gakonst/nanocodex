import json
import os
import re
import urllib.request
from pathlib import Path


GRADER_TEMPLATE = r"""Judge whether the following [response] to [question] is correct or not based on the precise and unambiguous [correct_answer] below.

[question]: {question}

[response]: {response}

Your judgement must be in the format and criteria specified below:

extracted_final_answer: The final exact answer extracted from the [response]. Put the extracted answer as 'None' if there is no exact, final answer to extract from the response.

[correct_answer]: {correct_answer}

reasoning: Explain why the extracted_final_answer is correct or incorrect based on [correct_answer], focusing only on if there are meaningful differences between [correct_answer] and the extracted_final_answer. Do not comment on any background to the problem, do not attempt to solve the problem, do not argue for any answer different than [correct_answer], focus only on whether the answers match.

correct: Answer 'yes' if extracted_final_answer matches the [correct_answer] given above, or is within a small margin of error for numerical problems. Answer 'no' otherwise, i.e. if there if there is any inconsistency, ambiguity, non-equivalency, or if the extracted answer is incorrect.


confidence: The extracted confidence score between 0|\%| and 100|\%| from [response]. Put 100 if there is no confidence score available."""


def parse_decision(text):
    match = re.search(r"correct: (yes|no)", text)
    return match.group(1) if match else "no"


def judge(question, correct_answer, candidate, scoring):
    prompt = GRADER_TEMPLATE.format(
        question=question,
        correct_answer=correct_answer,
        response=candidate,
    )
    body = json.dumps(
        {
            "model": scoring["grader_model"],
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": prompt},
            ],
            "temperature": scoring["reference_temperature"],
            "max_tokens": scoring["reference_max_output_tokens"],
        }
    ).encode()
    request = urllib.request.Request(
        f"{os.environ['NANOCODEX_JUDGE_BASE_URL']}/chat/completions",
        data=body,
        headers={
            "Authorization": f"Bearer {os.environ['NANOCODEX_JUDGE_TOKEN']}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=300) as response:
        document = json.load(response)
    text = document["choices"][0]["message"]["content"]
    return {
        "response_id": document.get("id"),
        "effective_model": document.get("model"),
        "text": text,
        "decision": parse_decision(text),
    }


def main():
    workspace = Path(os.environ["NANOCODEX_EVAL_WORKSPACE"])
    logs = Path(os.environ.get("NANOCODEX_EVAL_VERIFIER_LOGS", "/logs/verifier"))
    logs.mkdir(parents=True, exist_ok=True)
    case = json.loads(
        Path(os.environ.get("NANOCODEX_EVAL_CASE", "/tests/case.json")).read_text()
    )
    candidate = (workspace / "answer.txt").read_text()
    judgment = judge(
        case["question"], case["correct_answer"], candidate, case["scoring"]
    )
    accuracy = 1 if judgment["decision"] == "yes" else 0
    evidence = {
        "benchmark": "browsecomp",
        "case_id": case["id"],
        "topic": case["topic"],
        "grader_contract": case["scoring"]["grader_contract"],
        "requested_model": case["scoring"]["grader_model"],
        "reference_model": case["scoring"]["reference_model"],
        "reference_temperature": case["scoring"]["reference_temperature"],
        "reference_max_output_tokens": case["scoring"]["reference_max_output_tokens"],
        "candidate_chars": len(candidate),
        "judgment": judgment,
        "accuracy": accuracy,
    }
    (logs / "judgment.json").write_text(json.dumps(evidence, indent=2))
    (logs / "reward.json").write_text(json.dumps({"accuracy": accuracy}) + "\n")


if __name__ == "__main__":
    main()
