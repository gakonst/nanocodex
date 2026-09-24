# Synthetic Jev memory capture evaluation

Frozen cases: 20. Completed: 20. Classifier calls: 17 (cap 20; no retries or downstream generations).
Exact cases: 19/20. Exact provider-classified cases: 16/17.
Selected-span precision: 1. Durable-span recall: 0.8888888888888888.
TP 8; FP 0; FN 1; provider/selector failures 0.
Host-filter cases: 3; unexpected calls for excluded sources: 0.
Retain answers below the fixed 0.9 threshold: 2.
Provider latency p50/p95: 321.58045800000036 / 1583.985208 ms. Dollar cost was not measured.
Stopped: no.

All fixtures are synthetic. Labels, source hashes and one-call reservations were persisted before calls. The selector was bundled once; it was not tuned or rerun against these outcomes.
This is a small task-specific evaluation from a development host, not a calibrated accuracy guarantee, independent holdout, production-latency estimate, or authenticated background-pipeline test.
Raw sanitized answers/confidence/probabilities and bounded usage are in results.jsonl; exact fixture texts and gold ranges are in cases.json. No raw provider errors or credentials are retained.

| Case | Exact | TP | FP | FN | Failure | Calls |
| --- | --- | --- | --- | --- | --- | --- |
| metric-preference | true | 1 | 0 | 0 |  | 1 |
| detailed-replies | true | 1 | 0 | 0 |  | 1 |
| inactive-business | true | 1 | 0 | 0 |  | 1 |
| business-constraints | true | 1 | 0 | 0 |  | 1 |
| later-correction | true | 1 | 0 | 0 |  | 1 |
| qualified-preference | true | 1 | 0 | 0 |  | 1 |
| project-ownership | true | 1 | 0 | 0 |  | 1 |
| ongoing-migration | true | 1 | 0 | 0 |  | 1 |
| single-table | true | 0 | 0 | 0 |  | 1 |
| hypothetical-company | true | 0 | 0 | 0 |  | 1 |
| third-party-company | true | 0 | 0 | 0 |  | 1 |
| quoted-example | true | 0 | 0 | 0 |  | 1 |
| thanks | true | 0 | 0 | 0 |  | 1 |
| extractor-injection | true | 0 | 0 | 0 |  | 1 |
| explicit-opt-out | true | 0 | 0 | 0 |  | 1 |
| missing-context | true | 0 | 0 | 0 |  | 1 |
| secret-bearing | true | 0 | 0 | 0 |  | 0 |
| assistant-provenance | true | 0 | 0 | 0 |  | 0 |
| recalled-provenance | true | 0 | 0 | 0 |  | 0 |
| long-business-message | false | 0 | 0 | 1 |  | 1 |
