# JavaScript binding performance

The JavaScript boundary must remain small compared with a model turn. Run the
performance gate against the built Node and browser WASM packages:

```sh
just build-wasm
pnpm --filter nanocodex test:performance
```

The package's `test` script runs this gate after the functional test pool, so
installation and lifecycle fixtures cannot contend with the measurements.
The gate measures package-owned costs against these limits:

| Boundary | Regression limit |
| --- | ---: |
| Cold Node or precompiled-browser `Agent.create` | 250 ms |
| Warm `Agent.create` p50 | 1.5 ms |
| Warm `Agent.create` p95 | 10 ms |
| Retained browser WASM linear memory after repeated creation | 2.5 MB, with no growth |
| Persisted payload after the long-history replay scenario | Less than 32 KiB |

The creation scenarios verify that Node compiles and instantiates its module
once, and that browser agents reuse one caller-compiled module and WASM instance.
The browser scenario warms the constructor before collecting its timed samples.

The long-history scenario completes 96 turns through a local synthetic Responses
server, reopens the retained agent, and replays the final result. It then checks
432 cancellations at admission. WASM allocation is reported for the live,
reopened, and cancellation phases without a separate memory pass/fail threshold;
the retained payload has the size limit above.

Keep per-run timing and allocation diagnostics in CI artifacts or ignored
`output/`, together with the runtime and machine details needed for comparison.
