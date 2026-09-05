use std::path::Path;

pub(crate) fn prompt(
    profile: Option<&str>,
    config: &Path,
    state_dir: Option<&Path>,
    coordinator: Option<&str>,
    executable: Option<&Path>,
) -> String {
    let selected = profile.unwrap_or("the manifest default profile");
    let profile_argument =
        profile.map_or_else(String::new, |profile| format!(" {}", shell_quote(profile)));
    let config_argument = shell_quote(&config.to_string_lossy());
    let state_argument = state_dir.map_or_else(String::new, |directory| {
        format!(" --state-dir {}", shell_quote(&directory.to_string_lossy()))
    });
    let coordinator_argument = coordinator.map_or_else(String::new, |coordinator| {
        format!(" --coordinator {}", shell_quote(coordinator))
    });
    let task_authority = if coordinator.is_some() {
        "The coordinator's D1 board"
    } else {
        "SQLite"
    };
    let status_command = coordinator.map_or_else(
        || {
            format!(
                "nanocodex eval status{profile_argument} --config {config_argument}{state_argument} --json"
            )
        },
        |coordinator| {
            let profile_query = profile.map_or_else(String::new, |profile| {
                format!(
                    " --get --data-urlencode {}",
                    shell_quote(&format!("profile={profile}"))
                )
            });
            format!(
                "curl -fsS{profile_query} {}/v1/status",
                coordinator.trim_end_matches('/')
            )
        },
    );
    let reconciliation = coordinator.map_or_else(
        || {
            "Local status releases rows whose worker process disappeared; do not maintain another recovery record."
                .to_owned()
        },
        |coordinator| {
            format!(
                "status.workers is a JSON array of worker-name strings: extract it exactly with `jq -r '.workers[]'` and never access a field on an element. For every name whose nanocodex-eval-worker@<name>.service is not live, read that unit's bounded warning journal, then POST {{\"worker\":<name>,\"error\":<concise process or OOM classification>}} to {}/v1/workers/exited with `curl -fsS -H \"Authorization: Bearer $NANOCODEX_EVALS_WRITE_TOKEN\" -H 'Content-Type: application/json' --data <body>`. A journal containing `oom-kill` must be reported as literal `OOM`; otherwise retain the exact main-process exit line and never use a vague `see journal` classification. The operation is idempotent. Never perform the inverse: a live unit absent from status.workers is not an exited worker and must never be posted to this endpoint.",
                coordinator.trim_end_matches('/')
            )
        },
    );
    let executable = executable.map_or_else(
        || "nanocodex".to_owned(),
        |path| shell_quote(&path.to_string_lossy()),
    );
    let worker_command = format!(
        "{executable} eval run{profile_argument} --config {config_argument}{state_argument}{coordinator_argument} --worker <name>"
    );

    format!(
        r#"Drive the pre-materialized benchmark {selected} to completion at the highest productive host occupancy. You are the neural controller. {task_authority} is the only task authority, systemd is the only process authority, and every worker is one independent `{worker_command}` process. Each worker is intentionally single-shot: it claims at most one row, reports that attempt to the coordinator, and exits. A successfully completed worker therefore disappears from both status.workers and systemd after its collected unit exits; that is normal productive throughput, not a launch discrepancy or process failure.

Repeat this short control cycle until the board is terminal:

1. Observe, in one fresh compact Code Mode call, `{status_command}`; live or activating `nanocodex-eval-worker@*.service` user units with only their `Id`, `ActiveState`, `SubState`, and `ExecStart`; the `nanocodex-eval.slice` aggregate `MemoryCurrent`, `MemoryPeak`, `CPUUsageNSec`, and `TasksCurrent`; and the relevant lines from `/proc/meminfo`, `/proc/loadavg`, `/proc/pressure/memory`, and `df -B1 / "${{TMPDIR:-/tmp}}"`. Compact status only with `jq '{{tasks, recent_attempts, workers}}'`: the field is exactly `.recent_attempts`, never `.recent`. {task_authority} task counters are the authoritative record of work. Compare `success + failed` across cycles for terminal throughput; status.workers and live systemd units are only an instantaneous view of claims still in flight, never a history of workers admitted. Status includes terminal attempt outcomes and the newest eight retryable failures from the last five minutes; reason from their exact errors. For an interrupted attempt, use its retained worker name to read at most that unit's last 20 warning-or-higher journal lines and distinguish OOM from another process exit. A unit belongs to this board only when its `ExecStart` has the `{worker_command}` shape for this selected profile; unrelated eval units affect host pressure but never this board's live count or reconciliation. Do not list inactive units, request unselected systemd properties, read other journals or tracing during normal control, or keep a JavaScript loop, PID marker, worker pool, or other controller state.
2. Reconcile before admission. {reconciliation} Only a worker name that remains in status.workers while its systemd unit is not live represents a disappeared active claim requiring reconciliation. A name absent from status.workers has no active claim. Never investigate, classify, or reconcile a successfully launched single-shot worker merely because its collected unit and worker name are both gone at the next observation.
3. Reason from the current and recent observations and choose an absolute desired live-worker count that maximizes terminal completions per hour. Starting units count as live. Judge the previous batch by authoritative task movement: an increase in `success + failed` is productive completion, and names still in status.workers are productive work in flight. If admitted units disappear while terminal counters advance and pressure stays clear, the batch succeeded; grow the next batch aggressively instead of diagnosing launch failure or shrinking desired occupancy. Because a whole fast batch may finish inside one observation interval, a low later live count is not evidence that the batch was small or failed. A launch failed only when systemd-run itself returns nonzero, or when the coordinator retains an active worker name whose unit vanished. Enter recovery mode whenever memory `full avg10` is nonzero: admit nothing and let existing workers drain until it clears. A recent OOM remains a saturation calibration signal after pressure clears: resume below the failed occupancy with only a small probe, and do not grow again until the probe produces terminal throughput without renewed pressure. One isolated non-resource infrastructure retry such as a download or transport failure does not itself require recovery. A burst of the same provider or harness failure after increasing occupancy is admission-rate saturation: stop admitting until the burst has drained, then resume below the failed occupancy with a small probe and grow only after the probe produces terminal throughput without that failure. Outside recovery mode, with backlog and no measured overload or throughput stall, grow aggressively in a batch; unused healthy capacity is a controller failure. Repeatedly replacing OOMs or a provider-rate-limit burst without first reducing admission is a controller failure. High CPU or load alone is not overload.
4. Let `live` be this board's live or activating unit count. Launch `min(unclaimed, max(0, desired - live))` workers immediately. Before the batch, read current Unix nanoseconds once; name its workers `w-<unix-nanoseconds>-<ordinal>` so a controller restart can never collide with a loaded prior unit. For each worker set `worker_tmp="${{TMPDIR:-/tmp}}/workers/<name>"`, create it, then use `systemd-run --user --quiet --collect --service-type=exec --unit nanocodex-eval-worker@<name>.service --slice nanocodex-eval.slice --property OOMScoreAdjust=500 --property OOMPolicy=kill --property "ExecStopPost=/usr/bin/rm -rf -- $worker_tmp" --working-directory "$PWD" --setenv "PATH=$PATH" --setenv "TMPDIR=$worker_tmp" {worker_command}`. Remove `worker_tmp` yourself only if the launch command fails. The worker's systemd unit owns both its process cgroup and scratch directory, including cleanup after kernel OOM; the controller and tool session own neither.
5. With live workers and no elevated pressure or vanished worker, combine waiting and observation in one Code Mode call: start it with `// @exec: {{"yield_time_ms": 30000, "max_output_tokens": 2000}}`, await a 25-second `setTimeout` promise, and then collect the next compact observation. Never create a sleep subprocess, yielded background cell, or separate wait call. Let existing workers drain under overload; never stop, signal, or shed one manually.

Controller failure or restart must leave every worker untouched. On restart, derive the complete situation again from SQLite and systemd. Do not use subagents to own workers and do not wait for worker processes in Code Mode. Finish only when status has zero unclaimed and running rows and no live unit belonging to this board remains."#,
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
