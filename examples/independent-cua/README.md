# Independent owned-window lanes

This runnable Python 3.11+ example replaces shared observe/decide/act rounds with
one retained task per window. `lanes.py` is the reusable orchestration layer;
`host.py` adapts **owned GTK test windows** to the public CUA JavaScript tool.
The state files are an explicit test oracle. This is not a production accessibility
adapter, model-quality benchmark, or a claim that arbitrary application state can
be safely represented by a counter.

Each lane observes its own state and screenshot, awaits its own async decision,
then submits a revision-bound action. The host validates that lane's PID, revision,
and button immediately before input. Only the target screenshot is captured.
A fixed maximum of eight tasks and eight host workers bounds scheduling. The host
admits one outstanding request per lane and rejects extras instead of queueing.
Different lanes must own different native client processes.

The transport is one persistent subprocess, optionally SSH. A dedicated reader
correlates out-of-order replies by request ID. Only writing a JSONL frame holds the
transport lock; decisions, screenshots, native actions, and receipt polling have
no global lock or shared round barrier. Network bandwidth itself remains shared.
The host has one persistent companion and JS kernel per window: ordinary separate
`js` calls in one companion are queued while an active eval runs (`runtime/src/main.rs`,
`eval_without_completion` and `pending`). Native operation lanes within one eval
already support concurrency, but do not make separate long-lived evals independent.
This example does not change the native runtime's existing scheduling contract.

A lane failure stops only that lane. Uncertain initialization, capture, state,
protocol, or input errors quarantine the lane and close its companion. Validated
stale-action rejections leave it usable. Disconnect closes companions concurrently
before shutting down the worker pool, waking stalled RPCs. Timeouts never retry input. A locked,
fsynced append-only journal records `prepared` before sending input and `committed`
only after its receipt. Journal writes run on a synchronized worker thread so
fsync cannot block the shared asyncio event loop; input awaits durable preparation.
Cancellation waits for an in-flight journal write to finish. Any unresolved write prevents that lane from automatically
resuming. Reconcile it against actual application state before starting a new run;
do not delete an unresolved journal just to retry. A lost response may mean the
input already happened. Cancellation of a waiting caller is not remote cancellation.

The fixture's revision is valid because its only state mutation is a lane click.
A real adapter must bind its observation to native window/capture generation and
validate application-specific freshness at the input boundary. Replace the async
`decide(lane, observation)` callback to use a model; run blocking SDK work with
`asyncio.to_thread`. The included callback uses deterministic delays only.

## Tests

```sh
python3 -m unittest discover -s examples/independent-cua -v
```

## Isolated Omarchy acceptance

Requires existing `background-cua` lab assets: headless labwc, the compatible staged
Hyprland plugin/capture helper/companion, GTK3 Python, and primary-keyboard fixture.
The helper creates a private short `/dev/shm` runtime path (Unix socket length
limits matter), nested compositor, eight native clients, and a foreground fixture.
The primary keyboard stays connected through a private FIFO for the entire run.
It never loads a plugin into the user's compositor or changes installed binaries.
Use a new evidence directory on every run; prior artifacts remain intact.

```sh
python3 examples/independent-cua/lab.py start /path/to/new-evidence --assets /path/to/background-cua
python3 examples/independent-cua/lab.py snapshot /path/to/new-evidence
python3 examples/independent-cua/host.py /path/to/new-evidence/config.json --listen 18896
```

The loopback adapter accepts exactly one connection and exits after disconnect.
It serves only the configured owned fixtures. Prefer direct SSH stdio when the
remote account can execute the adapter. For a companion in a separate host-network
container, connect to the loopback adapter using a single SSH channel from the Mac:

```sh
python3 examples/independent-cua/benchmark.py --output result.json --journal journal.jsonl \
  ssh -o BatchMode=yes -o ConnectTimeout=10 -T -W 127.0.0.1:18896 omarchy
```

While running, use `lab.py keys /path/to/new-evidence` to send twenty primary-keyboard
events to the foreground fixture; compare `lab.py snapshot` before/after. For a
causal native-JS stall, set lane 5's `test_observe_delay_ms` to `8000` in the private
host config before starting it. This delays that companion's first observation.
Pass `--native-stall` to the benchmark to assert all five fast lanes finish before
lane 5's first action. Lane 7 has a two-second decision delay per step. Lane 6's second decision fails.
The other lanes must advance independently. The benchmark also submits a stale
lane-0 revision and checks its state did not advance.

```sh
python3 examples/independent-cua/lab.py stop /path/to/new-evidence
```

Stop only the PIDs/process groups recorded by this invocation, checking their
Linux process start times before signaling. Failed startup also runs cleanup. Keep the evidence
files. The stopped private runtime directory may be removed after inspection.

For an equivalent tiny JSON echo microbenchmark, separate from GUI/model latency:

```sh
python3 examples/independent-cua/transport_benchmark.py omarchy transport.json
```

Companion startup and per-process memory are costs of this explicit isolation.
This example does not claim lower model inference latency, validate Blender, or
activate any staged desktop deployment.

## Verified acceptance (2026-09-18)

Mac orchestration over one retained SSH channel to eight GTK windows in a private
Omarchy compositor: lanes 0–4 each completed five actions by 2.797 s. Lane 5's
injected eight-second native JS delay held its first action until 8.709 s. Lane 7's
two-second async decisions produced actions at 2.623, 4.919, 7.164, 9.419, and
11.722 s. Lane 6 stopped after its first action on an injected decision failure.
The complete benchmark took 11.889 s, and a stale revision was rejected without
advancing the target. These are one-run deterministic scheduling measurements.

Before, during, and after snapshots kept the same foreground PID and pointer
coordinates. All 40 primary keyboard events reached the foreground fixture;
background fixtures received zero keys and the foreground received zero clicks.
A separate real companion kill quarantined that lane while a healthy lane observed
in 195 ms; disconnect during a native JS stall exited the host in 15 ms.
The 21 focused tests passed on both Mac and Linux. The isolated compositor and all
processes using its private runtime directory were stopped after verification.
