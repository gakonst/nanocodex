import http.cookiejar
import json
import os
import urllib.error
import urllib.request
from pathlib import Path


STATE_PATH = Path("/tmp/nanocodex-arc-agi-3-state.json")
COOKIE_PATH = Path("/tmp/nanocodex-arc-agi-3-cookies.txt")


def close_scorecard(case, state):
    cookies = http.cookiejar.MozillaCookieJar(str(COOKIE_PATH))
    cookies.load(ignore_discard=True, ignore_expires=True)
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies))
    request = urllib.request.Request(
        case["api_base_url"] + "/api/scorecard/close",
        headers={
            "Accept": "application/json",
            "Content-Type": "application/json",
            "X-API-Key": state["api_key"],
        },
        data=json.dumps({"card_id": state["card_id"]}).encode(),
        method="POST",
    )
    with opener.open(request, timeout=30) as response:
        return json.load(response)


def main():
    logs = Path(os.environ.get("NANOCODEX_EVAL_VERIFIER_LOGS", "/logs/verifier"))
    logs.mkdir(parents=True, exist_ok=True)
    case = json.loads(Path("/tests/case.json").read_text())
    evidence = {
        "benchmark": "arc-agi-3",
        "game_id": case["game_id"],
        "benchmark_revision": case["benchmark_revision"],
        "score_contract": case["score_contract"],
        "smoke_action_cap": case["smoke_action_cap"],
    }
    protocol_complete = 0
    environment_score = 0.0
    failure = None
    try:
        state = json.loads(STATE_PATH.read_text())
        evidence.update(
            {
                "actions_submitted": state["actions"],
                "forced_resets": state["forced_resets"],
                "exit_reason": state["exit_reason"] or "AGENT_STOPPED",
            }
        )
        scorecard = close_scorecard(case, state)
        environments = scorecard.get("environments", [])
        if len(environments) != 1 or environments[0].get("id") != case["game_id"]:
            raise ValueError("official scorecard does not contain exactly the selected game")
        environment_score = float(environments[0]["score"])
        protocol_complete = int(state["actions"] > 0)
        evidence["scorecard"] = scorecard
    except (OSError, KeyError, ValueError, json.JSONDecodeError, urllib.error.URLError) as error:
        failure = error
        evidence["error"] = str(error)
    evidence["protocol_complete"] = protocol_complete
    evidence["official_environment_score"] = environment_score
    (logs / "arc-agi-3-scorecard.json").write_text(json.dumps(evidence, indent=2))
    if failure is not None:
        raise SystemExit(f"ARC-AGI-3 verifier infrastructure failure: {failure}")
    (logs / "reward.json").write_text(
        json.dumps({"protocol_complete": protocol_complete}) + "\n"
    )
    STATE_PATH.unlink(missing_ok=True)
    COOKIE_PATH.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
