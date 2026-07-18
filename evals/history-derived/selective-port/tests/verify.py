import importlib.util
from pathlib import Path
import sys


path = Path("/app/client.py")
spec = importlib.util.spec_from_file_location("candidate_client", path)
client = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = client
spec.loader.exec_module(client)

attempts = 0
delays = []


def succeeds_on_third_send():
    global attempts
    attempts += 1
    if attempts < 3:
        raise client.TransientError("again")
    return 17


assert client.fetch(succeeds_on_third_send, delays.append, attempts=4) == 17
assert attempts == 3
assert delays == [0.1, 0.2]

attempts = 0
delays = []


def always_transient():
    global attempts
    attempts += 1
    raise client.TransientError("still failing")


try:
    client.fetch(always_transient, delays.append, attempts=3)
except client.TransientError:
    pass
else:
    raise AssertionError("last transient failure must propagate")

assert attempts == 3
assert delays == [0.1, 0.2]


def permanent_failure():
    raise ValueError("do not retry")


try:
    client.fetch(permanent_failure, lambda _delay: (_ for _ in ()).throw(AssertionError()))
except ValueError:
    pass
else:
    raise AssertionError("non-transient failure must propagate")

assert client.format_result("x") == "value=x"
assert "json.dumps" not in path.read_text(), "unrelated JSON output change was ported"
