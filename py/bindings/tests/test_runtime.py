from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import unittest

from nanocodex import Nanocodex
from support import MockResponsesServer


def native_thread_count() -> int:
    if sys.platform.startswith("linux"):
        return len(os.listdir("/proc/self/task"))
    if sys.platform == "darwin":
        output = subprocess.check_output(
            ["ps", "-M", "-p", str(os.getpid())],
            text=True,
        )
        return max(0, len(output.splitlines()) - 1)
    raise unittest.SkipTest(f"native thread count is unsupported on {sys.platform}")


class RuntimeTests(unittest.TestCase):
    def test_independent_agents_share_one_bounded_runtime(self) -> None:
        first, first_events = Nanocodex("test-key", thinking="none")
        baseline = native_thread_count()
        agents = [Nanocodex("test-key", thinking="none") for _ in range(8)]
        growth = native_thread_count() - baseline
        self.assertLessEqual(
            growth,
            1,
            f"eight additional agents created {growth} native threads",
        )
        for agent, _ in agents:
            agent.shutdown()
        first.shutdown()
        while first_events.recv_json() is not None:
            pass

    def test_fresh_process_runtime_thread_growth_is_absolutely_bounded(self) -> None:
        script = r"""
import json
import os
import subprocess
import sys
from nanocodex import Nanocodex

def threads():
    if sys.platform.startswith("linux"):
        return len(os.listdir("/proc/self/task"))
    output = subprocess.check_output(
        ["ps", "-M", "-p", str(os.getpid())],
        text=True,
    )
    return max(0, len(output.splitlines()) - 1)

before = threads()
agents = [Nanocodex("test-key", thinking="none") for _ in range(16)]
after = threads()
print(json.dumps({"before": before, "after": after}))
for agent, _ in agents:
    agent.shutdown()
"""
        completed = subprocess.run(
            [sys.executable, "-I", "-c", script],
            check=True,
            capture_output=True,
            text=True,
        )
        counts = json.loads(completed.stdout)
        self.assertLessEqual(
            counts["after"] - counts["before"],
            3,
            counts,
        )

    def test_fresh_process_post_io_thread_growth_is_absolutely_bounded(self) -> None:
        with MockResponsesServer() as server:
            script = f"""
import json
import os
import subprocess
import sys
import threading
import time
from nanocodex import Nanocodex

def threads():
    if sys.platform.startswith("linux"):
        return len(os.listdir("/proc/self/task"))
    output = subprocess.check_output(
        ["ps", "-M", "-p", str(os.getpid())],
        text=True,
    )
    return max(0, len(output.splitlines()) - 1)

before = threads()
agents = [
    Nanocodex(
        "test-key",
        thinking="none",
        websocket_url={server.endpoint!r},
    )[0]
    for _ in range(8)
]
barrier = threading.Barrier(len(agents))
errors = []

def run(index):
    try:
        barrier.wait()
        result = agents[index].prompt(f"agent {{index}}").result()
        if result.final_message != f"agent {{index}}":
            raise AssertionError("concurrent result did not match its prompt")
    except Exception as error:
        errors.append(error)

workers = [
    threading.Thread(target=run, args=(index,))
    for index in range(len(agents))
]
for worker in workers:
    worker.start()
for worker in workers:
    worker.join(5)
if any(worker.is_alive() for worker in workers):
    raise TimeoutError("concurrent binding audit did not finish")
if errors:
    raise errors[0]
during = threads()
for agent in agents:
    agent.shutdown()
deadline = time.monotonic() + 2
while True:
    after = threads()
    if during - after >= len(agents) or time.monotonic() >= deadline:
        break
    time.sleep(0.01)
print(json.dumps({{"before": before, "during": during, "after": after}}))
"""
            completed = subprocess.run(
                [sys.executable, "-I", "-c", script],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(server.connection_count, 8)
        counts = json.loads(completed.stdout)
        self.assertEqual(
            counts["during"] - counts["after"],
            8,
            counts,
        )
        self.assertLessEqual(
            counts["after"] - counts["before"],
            5,
            counts,
        )

    def test_multiple_agents_complete_concurrently_on_the_shared_runtime(self) -> None:
        with MockResponsesServer() as server:
            agents = [
                Nanocodex(
                    "test-key",
                    thinking="none",
                    websocket_url=server.endpoint,
                )[0]
                for _ in range(4)
            ]
            barrier = threading.Barrier(len(agents))
            results: list[str] = []
            errors: list[Exception] = []
            lock = threading.Lock()

            def run(index: int) -> None:
                try:
                    barrier.wait()
                    message = (
                        agents[index]
                        .prompt(f"concurrent agent {index}")
                        .result()
                        .final_message
                    )
                    with lock:
                        results.append(message)
                # Propagate arbitrary binding failures back to the test thread.
                except Exception as error:  # noqa: BLE001
                    with lock:
                        errors.append(error)

            workers = [
                threading.Thread(target=run, args=(index,))
                for index in range(len(agents))
            ]
            for worker in workers:
                worker.start()
            for worker in workers:
                worker.join(5)
            self.assertFalse(errors)
            self.assertCountEqual(
                results,
                [f"concurrent agent {index}" for index in range(len(agents))],
            )
            self.assertEqual(server.connection_count, len(agents))
            for agent in agents:
                agent.shutdown()


if __name__ == "__main__":
    unittest.main()
