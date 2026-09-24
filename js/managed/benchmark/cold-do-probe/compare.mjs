import { performance } from 'node:perf_hooks';

// Synthetic, admin-only production DO probe; creates no managed agents.
const apiKey = process.env.NANOCODEX_PROBE_API_KEY;
const probeToken = process.env.NANOCODEX_COLD_PROBE_TOKEN;
const slimUrl = process.env.NANOCODEX_COLD_PROBE_URL;
const managedUrl = process.env.NANOCODEX_MANAGED_URL ?? 'https://nanocodex.gakonst.workers.dev';
const trials = Number(process.env.NANOCODEX_COLD_PROBE_TRIALS ?? 24);
if (!apiKey || !probeToken || !slimUrl || !Number.isInteger(trials) || trials < 1 || trials > 48) {
  throw new Error('Require probe API key, token, URL, and 1–48 trials');
}
const arms = {
  full: {
    url: new URL('/v1/agents/activation-probe', managedUrl),
    headers: { authorization: `Bearer ${apiKey}`, 'x-nanocodex-probe-kind': 'key-unique' },
  },
  slim: { url: new URL('/activation', slimUrl), headers: { authorization: `Bearer ${probeToken}` } },
};
const records = [];
for (let trial = 0; trial < trials; trial++) {
  const order = trial % 2 ? ['slim', 'full'] : ['full', 'slim'];
  for (const arm of order) {
    const started = performance.now();
    const response = await fetch(arms[arm].url, {
      method: 'POST', headers: arms[arm].headers, signal: AbortSignal.timeout(15_000),
    });
    const value = await response.json().catch(() => ({}));
    const record = {
      trial, arm, status: response.status,
      client_ms: Math.round(performance.now() - started),
      dispatch_ms: value.dispatch_ms,
      before_handler_ms: value.before_constructor_ms,
      ingress_colo: response.headers.get('cf-ray')?.split('-').at(-1) ?? null,
    };
    records.push(record);
    console.log(JSON.stringify(record));
    if (!response.ok || !Number.isFinite(record.dispatch_ms)) {
      throw new Error(`Probe ${arm} trial ${trial} failed: HTTP ${response.status}`);
    }
  }
  await new Promise((resolve) => setTimeout(resolve, 250));
}
const median = (values) => {
  const sorted = values.toSorted((a, b) => a - b);
  return (sorted[Math.floor((sorted.length - 1) / 2)] + sorted[Math.floor(sorted.length / 2)]) / 2;
};
for (const arm of ['full', 'slim']) {
  const values = records.filter((record) => record.arm === arm).map((record) => record.dispatch_ms);
  console.log(JSON.stringify({ arm, trials: values.length, min_ms: Math.min(...values),
    median_ms: median(values), max_ms: Math.max(...values) }));
}
console.log(JSON.stringify({ arm: 'paired-full-minus-slim', trials,
  median_ms: median(Array.from({ length: trials }, (_, trial) =>
    records.find((record) => record.trial === trial && record.arm === 'full').dispatch_ms
    - records.find((record) => record.trial === trial && record.arm === 'slim').dispatch_ms)) }));
