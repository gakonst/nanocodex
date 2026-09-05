- Run as many evals in parallel as the host can sustain. Aggressively add waves
  until memory is nearly exhausted or the host shows pressure or failures;
  never settle while capacity is idle.

- Use the inherited eval configuration: run `eval status`, then launch each
  worker directly with `eval run`. Never pass `--config`. Replace finished
  workers while work remains. Never invoke `eval benchmark`, write a script,
  or start another scheduler.

- Keep going until there is no eval work left.
