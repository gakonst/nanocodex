#!/usr/bin/env python3
"""Run interleaved Codex/Nanocodex trials and summarize successful turns."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any


def load_api_key(env_file: Path) -> str:
    key = os.environ.get("OPENAI_API_KEY")
    if key:
        return key
    for raw_line in env_file.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        name, value = line.split("=", 1)
        if name.strip() == "OPENAI_API_KEY":
            value = value.strip().strip("\"'")
            if value:
                return value
    raise RuntimeError(f"OPENAI_API_KEY is missing from the environment and {env_file}")


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    weight = position - lower
    return ordered[lower] * (1 - weight) + ordered[upper] * weight


def distribution(values: list[float]) -> dict[str, float | int | None]:
    return {
        "n": len(values),
        "p50": rounded(percentile(values, 0.50)),
        "p95": rounded(percentile(values, 0.95)),
    }


def rounded(value: float | None) -> float | None:
    return round(value, 1) if value is not None else None


def metrics(result: dict[str, Any]) -> dict[str, list[float]]:
    if result["implementation"] == "stock_codex":
        chain = result["chain_turn_latency_ms"]
        chain_first_output = [
            value
            for value in result["chain_time_to_first_output_ms"]
            if value is not None
        ]
        return {
            "cold_turn_ms": [chain[0]],
            "warm_turn_ms": chain[1:],
            "time_to_first_output_ms": chain_first_output,
            "fork_construction_ms": [result["fork_rpc_wall_ms"]],
            "mainline_ms": [result["mainline_latency_ms"]],
            "branch_ms": result["branch_latency_ms"],
            "branch_cached_tokens": [
                float(value)
                for value in result["branch_cached_tokens"]
                if value is not None
            ],
        }
    chain = result["chain"]
    return {
        "cold_turn_ms": [chain[0]["latency_ms"]],
        "warm_turn_ms": [turn["latency_ms"] for turn in chain[1:]],
        "time_to_first_output_ms": [
            turn["time_to_first_output_ms"]
            for turn in chain
            if turn["time_to_first_output_ms"] is not None
        ],
        "fork_construction_ms": [result["fork_api_wall_ms"]],
        "mainline_ms": [result["mainline"]["latency_ms"]],
        "branch_ms": [turn["latency_ms"] for turn in result["branches"]],
        "branch_cached_tokens": [
            float(turn["usage"]["cached_input_tokens"])
            for turn in result["branches"]
        ],
    }


def run_command(command: list[str], environment: dict[str, str]) -> dict[str, Any]:
    completed = subprocess.run(
        command,
        check=False,
        capture_output=True,
        env=environment,
        text=True,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"command exited {completed.returncode}: {' '.join(command)}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return json.loads(completed.stdout)


def benchmark(args: argparse.Namespace) -> dict[str, Any]:
    cwd = args.cwd.resolve()
    workload = args.workload.resolve()
    env_file = args.env_file.resolve()
    environment = os.environ.copy()
    auth_file = args.auth_file.resolve() if args.auth_file is not None else None
    if auth_file is not None:
        if not auth_file.is_file():
            raise RuntimeError(f"shared ChatGPT auth file does not exist: {auth_file}")
        environment.pop("CODEX_API_KEY", None)
        environment.pop("OPENAI_API_KEY", None)
    else:
        environment["OPENAI_API_KEY"] = load_api_key(env_file)
    trials: list[dict[str, Any]] = []
    for trial in range(1, args.trials + 1):
        order = ["nanocodex", "stock_codex"]
        if trial % 2 == 0:
            order.reverse()
        for implementation in order:
            if implementation == "nanocodex":
                command = [
                    str(args.nanocodex_bin.resolve()),
                    "--cwd",
                    str(cwd),
                    "--workload",
                    str(workload),
                    "--source-commit",
                    args.nanocodex_commit,
                ]
                if auth_file is not None:
                    command.extend(["--auth-file", str(auth_file)])
            else:
                app_server = (
                    [str(args.codex_app_server.resolve())]
                    if args.codex_app_server is not None
                    else [args.codex_bin, "app-server"]
                )
                command = [
                    sys.executable,
                    str(args.codex_script.resolve()),
                    "--cwd",
                    str(cwd),
                    "--workload",
                    str(workload),
                    "--source-commit",
                    args.codex_commit,
                    "--codex-cli-version",
                    args.codex_version,
                    "--app-server",
                    *app_server,
                ]
                if auth_file is not None:
                    command.extend(["--auth-file", str(auth_file)])
                else:
                    command.extend(["--env-file", str(env_file)])
            try:
                result = run_command(command, environment)
                trials.append(
                    {
                        "trial": trial,
                        "implementation": implementation,
                        "success": True,
                        "result": result,
                    }
                )
            except Exception as error:
                trials.append(
                    {
                        "trial": trial,
                        "implementation": implementation,
                        "success": False,
                        "error": str(error),
                    }
                )

    summary: dict[str, Any] = {}
    for implementation in ("nanocodex", "stock_codex"):
        selected = [
            trial
            for trial in trials
            if trial["implementation"] == implementation and trial["success"]
        ]
        combined: dict[str, list[float]] = {}
        for trial in selected:
            for name, values in metrics(trial["result"]).items():
                combined.setdefault(name, []).extend(values)
        summary[implementation] = {
            "successful_runs": len(selected),
            "attempted_runs": args.trials,
            **{name: distribution(values) for name, values in combined.items()},
        }

    document = {
        "schema_version": 1,
        "workload": str(workload),
        "workload_fnv1a64": next(
            (
                trial["result"]["workload_fnv1a64"]
                for trial in trials
                if trial["success"]
            ),
            None,
        ),
        "trials_per_implementation": args.trials,
        "schedule": "alternating_sequential",
        "auth_mode": "chatgpt" if auth_file is not None else "api_key",
        "summary": summary,
        "trials": trials,
    }
    if args.output is not None:
        args.output.write_text(json.dumps(document, indent=2) + "\n")
    return document


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--nanocodex-bin", type=Path, required=True)
    parser.add_argument("--nanocodex-commit", default="unknown")
    parser.add_argument("--codex-bin", default="codex")
    parser.add_argument("--codex-app-server", type=Path)
    parser.add_argument("--codex-script", type=Path, default=Path(__file__).with_name("stock_codex_fork_bench.py"))
    parser.add_argument("--codex-commit", default="installed-unmapped")
    parser.add_argument("--codex-version", default="unknown")
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument("--workload", type=Path, required=True)
    parser.add_argument("--env-file", type=Path, default=Path.cwd() / ".env")
    parser.add_argument(
        "--auth-file",
        type=Path,
        help="shared Codex auth.json used by both implementations",
    )
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    result = benchmark(parse_args())
    print(
        json.dumps(
            {
                key: result[key]
                for key in (
                    "workload",
                    "trials_per_implementation",
                    "schedule",
                    "auth_mode",
                    "summary",
                )
            },
            indent=2,
        )
    )
    attempted = result["trials_per_implementation"]
    return int(
        any(
            result["summary"][implementation]["successful_runs"] != attempted
            for implementation in ("nanocodex", "stock_codex")
        )
    )


if __name__ == "__main__":
    raise SystemExit(main())
