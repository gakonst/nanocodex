import importlib.util
from pathlib import Path
import sys


spec = importlib.util.spec_from_file_location("candidate_analysis", Path("/app/analyze.py"))
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)


def record(agent, task, completed_at, reward, exception, seconds, input_tokens, cached, output):
    return {
        "agent": agent,
        "task": task,
        "completed_at": completed_at,
        "reward": reward,
        "exception": exception,
        "seconds": seconds,
        "input_tokens": input_tokens,
        "cached_input_tokens": cached,
        "output_tokens": output,
    }


records = [
    record("a", "x", 1, 0, False, 99, 100, 0, 10),
    record("a", "x", 2, 1, False, 4, 20, 10, 2),
    record("b", "x", 2, 1, False, 5, 30, 15, 3),
    record("a", "y", 2, 1, True, 1, 10, 10, 1),
    record("b", "y", 2, 1, False, 2, 20, 0, 2),
    record("a", "z", 2, 1, False, 3, 40, 20, 4),
    record("b", "z", 2, 0, False, 1, 50, 25, 5),
    record("a", "tie", 2, 1, False, 4, 10, 0, 1),
    record("b", "tie", 2, 1, False, 4, 10, 5, 1),
]

assert module.compare(records, "a", "b") == {
    "score": {"a": [3, 4], "b": [3, 4]},
    "shared_passes": ["tie", "x"],
    "faster": {"a": ["x"], "b": []},
    "usage": {
        "a": {
            "input_tokens": 80,
            "cached_input_tokens": 40,
            "output_tokens": 8,
            "cache_rate": 0.5,
        },
        "b": {
            "input_tokens": 110,
            "cached_input_tokens": 45,
            "output_tokens": 11,
            "cache_rate": 45 / 110,
        },
    },
}

zero = [record("a", "empty", 1, 0, False, 1, 0, 0, 0)]
empty_result = module.compare(zero, "a", "b")
assert empty_result["usage"]["a"]["cache_rate"] == 0
assert empty_result["usage"]["b"] == {
    "input_tokens": 0,
    "cached_input_tokens": 0,
    "output_tokens": 0,
    "cache_rate": 0,
}
