import json


class TransientError(RuntimeError):
    pass


def fetch(send, sleep, attempts=3):
    for attempt in range(attempts):
        try:
            return send()
        except TransientError:
            if attempt + 1 == attempts:
                raise
            sleep(0.1 * (2**attempt))


def format_result(value):
    return json.dumps({"value": value}, sort_keys=True)
