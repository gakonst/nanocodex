import json
import os
import subprocess
from pathlib import Path


def run(command, *, check=True):
    return subprocess.run(
        command,
        cwd=workspace,
        check=check,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


workspace = Path(os.environ["NANOCODEX_EVAL_WORKSPACE"])
logs = Path(os.environ.get("NANOCODEX_EVAL_VERIFIER_LOGS", "/logs/verifier"))
logs.mkdir(parents=True, exist_ok=True)
case = json.loads(Path("/tests/instance.json").read_text())

# Preserve the candidate delta, install the benchmark's hidden test patch on a
# clean base, and then restore the candidate delta exactly as the official
# SWE-bench evaluation order requires.
run(["git", "add", "-N", "."], check=False)
candidate_patch = run(["git", "diff", "--binary", case["base_commit"]]).stdout
(logs / "candidate.patch").write_text(candidate_patch)
run(["git", "reset", "--hard", case["base_commit"]])
run(["git", "clean", "-fd"])
(logs / "test.patch").write_text(case["test_patch"])
run(["git", "apply", str(logs / "test.patch")])
if candidate_patch:
    run(["git", "apply", str(logs / "candidate.patch")])

fail_to_pass = json.loads(case["FAIL_TO_PASS"])
pass_to_pass = json.loads(case["PASS_TO_PASS"])
tests = fail_to_pass + pass_to_pass
result = run(["python", "-m", "pytest", "-rA", *tests], check=False)
(logs / "test-output.txt").write_text(result.stdout)
reward = 1.0 if result.returncode == 0 else 0.0
(logs / "swe-bench.json").write_text(
    json.dumps(
        {
            "instance_id": case["instance_id"],
            "base_commit": case["base_commit"],
            "version": case["version"],
            "fail_to_pass": fail_to_pass,
            "pass_to_pass": pass_to_pass,
            "exit_code": result.returncode,
            "resolved": bool(reward),
        },
        indent=2,
    )
)
(logs / "reward.txt").write_text(f"{reward}\n")
