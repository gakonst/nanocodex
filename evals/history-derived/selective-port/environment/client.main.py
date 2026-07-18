class TransientError(RuntimeError):
    pass


def fetch(send, sleep, attempts=3):
    return send()


def format_result(value):
    return f"value={value}"
