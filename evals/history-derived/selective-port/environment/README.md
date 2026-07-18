# Selective port

The `goal` branch contains multiple later commits. We need its bounded retry
behavior for `fetch`, including the recorded backoff delays, but the current
`format_result` text format is a compatibility contract.

Only `TransientError` is retryable. At most `attempts` sends are made. Before
each retry, call the injected `sleep` with delays `0.1`, `0.2`, `0.4`, and so
on. Return immediately on success and propagate the last transient failure.
