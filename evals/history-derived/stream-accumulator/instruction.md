Fix the immutable streaming accumulator in `accumulator.mjs` so incremental
updates do not flash unchanged content and snapshots commit atomically. Preserve
the exported API and run the tests.
