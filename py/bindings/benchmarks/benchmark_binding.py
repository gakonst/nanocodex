from __future__ import annotations

# The benchmark runs from an installed wheel while importing its source-owned
# mock server only after adding the tests directory.
import argparse
import json
import os
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

TESTS = Path(__file__).resolve().parents[1] / "tests"
sys.path.insert(0, str(TESTS))

import nanocodex
from nanocodex import Nanocodex
from support import MockResponsesServer  # pyright: ignore[reportMissingImports]


def elapsed_ms(started_ns: int) -> float:
    return (time.perf_counter_ns() - started_ns) / 1_000_000


def median(values: list[float]) -> float:
    return statistics.median(values)


def percentile(values: list[float], percentile_value: float) -> float:
    ordered = sorted(values)
    index = round((len(ordered) - 1) * percentile_value)
    return ordered[index]


def native_thread_count() -> int:
    if sys.platform.startswith("linux"):
        return len(os.listdir("/proc/self/task"))
    if sys.platform == "darwin":
        output = subprocess.check_output(
            ["ps", "-M", "-p", str(os.getpid())],
            text=True,
        )
        return max(0, len(output.splitlines()) - 1)
    raise RuntimeError(f"native thread count is unsupported on {sys.platform}")


def fresh_import_times(rounds: int = 7) -> list[float]:
    times = []
    command = [
        sys.executable,
        "-I",
        "-c",
        "import nanocodex; assert nanocodex.__version__",
    ]
    for _ in range(rounds):
        started = time.perf_counter_ns()
        subprocess.run(command, check=True)
        times.append(elapsed_ms(started))
    return times


def construction_metrics(rounds: int = 20) -> tuple[list[float], int, int]:
    first, first_events = Nanocodex("benchmark-only", thinking="none")
    threads_after_first = native_thread_count()
    agents: list[tuple[Nanocodex, Any]] = []
    times = []
    for _ in range(rounds):
        started = time.perf_counter_ns()
        agents.append(Nanocodex("benchmark-only", thinking="none"))
        times.append(elapsed_ms(started))
    threads_after_all = native_thread_count()
    for agent, _ in agents:
        agent.shutdown()
    first.shutdown()
    while first_events.recv_json() is not None:
        pass
    return times, threads_after_first, threads_after_all


def sequential_turn_metrics(
    rounds: int = 30,
) -> tuple[list[float], list[float], float, float, int]:
    with MockResponsesServer() as server:
        agent, events = Nanocodex(
            "benchmark-only",
            thinking="none",
            prompt_cache_key="python-binding-benchmark",
            websocket_url=server.endpoint,
        )
        acceptance_times = []
        result_times = []
        for index in range(rounds):
            started = time.perf_counter_ns()
            turn = agent.prompt(f"benchmark turn {index}")
            acceptance_times.append(elapsed_ms(started))
            started = time.perf_counter_ns()
            result = turn.result()
            result_times.append(elapsed_ms(started))
            if result.final_message != f"benchmark turn {index}":
                raise AssertionError("mock turn returned an unexpected result")

        event_count = 0
        terminals = 0
        received_events = []
        started = time.perf_counter_ns()
        while terminals < rounds:
            event = events.recv()
            if event is None:
                raise AssertionError("event stream closed before every terminal")
            event_count += 1
            received_events.append(event)
            if event.kind in {"run.completed", "run.failed"}:
                terminals += 1
        event_delivery_us = elapsed_ms(started) * 1000 / event_count
        started = time.perf_counter_ns()
        for event in received_events:
            _ = event.payload
        payload_decode_us = elapsed_ms(started) * 1000 / event_count
        agent.shutdown()
        while events.recv() is not None:
            pass
        if server.connection_count != 1:
            raise AssertionError("sequential turns did not reuse one WebSocket")
        return (
            acceptance_times,
            result_times,
            event_delivery_us,
            payload_decode_us,
            event_count,
        )


def concurrent_agent_wall_ms(agent_count: int = 8) -> float:
    with MockResponsesServer() as server:
        agents = [
            Nanocodex(
                "benchmark-only",
                thinking="none",
                websocket_url=server.endpoint,
            )[0]
            for _ in range(agent_count)
        ]
        barrier = threading.Barrier(agent_count)
        errors: list[Exception] = []
        lock = threading.Lock()

        def run(index: int) -> None:
            try:
                barrier.wait()
                result = agents[index].prompt(f"agent {index}").result()
                if result.final_message != f"agent {index}":
                    raise AssertionError("concurrent result did not match its prompt")
            # Propagate arbitrary binding failures from the worker to the caller.
            except Exception as error:  # noqa: BLE001
                with lock:
                    errors.append(error)

        workers = [
            threading.Thread(target=run, args=(index,)) for index in range(agent_count)
        ]
        started = time.perf_counter_ns()
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join(5)
        wall_ms = elapsed_ms(started)
        if errors:
            raise errors[0]
        if any(worker.is_alive() for worker in workers):
            raise TimeoutError("concurrent binding benchmark did not finish")
        if server.connection_count != agent_count:
            raise AssertionError("independent agents did not own independent sockets")
        for agent in agents:
            agent.shutdown()
        return wall_ms


def run() -> dict[str, Any]:
    threads_before = native_thread_count()
    imports = fresh_import_times()
    constructions, threads_after_first, threads_after_all = construction_metrics()
    shared_runtime_thread_growth = threads_after_first - threads_before
    additional_agent_construction_thread_growth = (
        threads_after_all - threads_after_first
    )
    (
        acceptance,
        results,
        event_delivery_us,
        payload_decode_us,
        event_count,
    ) = sequential_turn_metrics()
    concurrent_wall = concurrent_agent_wall_ms()
    threads_after_shutdown = native_thread_count()
    retained_shared_thread_growth = threads_after_shutdown - threads_before
    return {
        "environment": {
            "python": sys.version.split()[0],
            "nanocodex": nanocodex.__version__,
            "module": str(Path(nanocodex.__file__).resolve()),
            "platform": sys.platform,
        },
        "metrics": {
            "fresh_import_ms_p50": median(imports),
            "fresh_import_ms_p95": percentile(imports, 0.95),
            "warm_agent_construction_ms_p50": median(constructions),
            "warm_agent_construction_ms_p95": percentile(constructions, 0.95),
            "shared_runtime_thread_growth_after_first_agent": (
                shared_runtime_thread_growth
            ),
            "additional_agent_construction_thread_growth": (
                additional_agent_construction_thread_growth
            ),
            "prompt_acceptance_ms_p50": median(acceptance),
            "prompt_acceptance_ms_p95": percentile(acceptance, 0.95),
            "mock_turn_result_ms_p50": median(results),
            "mock_turn_result_ms_p95": percentile(results, 0.95),
            "event_delivery_us_per_event": event_delivery_us,
            "event_payload_decode_us_per_event": payload_decode_us,
            "delivered_events": event_count,
            "eight_agent_wall_ms": concurrent_wall,
            "retained_shared_thread_growth_after_shutdown": (
                retained_shared_thread_growth
            ),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail when a committed binding SLA is exceeded",
    )
    args = parser.parse_args()
    report = run()
    print(json.dumps(report, indent=2, sort_keys=True))
    if not args.check:
        return
    thresholds = json.loads((Path(__file__).with_name("thresholds.json")).read_text())
    failures = []
    metrics = report["metrics"]
    for name, maximum in thresholds.items():
        observed = metrics[name]
        if observed > maximum:
            failures.append(f"{name}: {observed:.3f} > {maximum:.3f}")
    if failures:
        raise SystemExit("binding performance SLA exceeded:\n" + "\n".join(failures))


if __name__ == "__main__":
    main()
