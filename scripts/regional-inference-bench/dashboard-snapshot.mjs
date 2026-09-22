import { readFileSync, writeFileSync } from 'node:fs';
const root = new URL('../../docs/performance/2026-09-21-regional-inference/', import.meta.url);
const read = name => JSON.parse(readFileSync(new URL(name, root), 'utf8'));
const comparison = read('comparison.json'), jev = read('jev/summary.json');
const colo = { 'us-east-1': 'IAD', 'eu-west-1': 'LHR', 'ap-northeast-1': 'NRT' };
const snapshot = {
  date: '2026-09-21', model: 'gpt-5.6-luna', effort: 'low',
  source: 'https://github.com/gakonst/nanocodex/blob/e4dc946bd430ea0cc8fbb62ebdb63d3a8c0c9eb9/docs/performance/2026-09-21-regional-inference/README.md',
  cells: comparison.mixed_descriptive_groups.map(g => ({ colo: colo[g.region], provider: g.provider,
    baseline: g.first_meaningful.before_p50_ms, streaming: g.first_meaningful.after_p50_ms,
    baselineCount: g.completed_before, streamingCount: g.completed_after })),
  jev: { cases: jev.cases, bindingFailures: jev.classifier_binding_ok.false,
    lowConfidence: jev.confidence_status.low, accepted: jev.confidence_status.accepted, completed: jev.downstream_completed,
    rows: jev.rows.map(r => Object.fromEntries(['arm','binding_ok','confidence_status','confidence','probabilities','selected','outer_router_ms','placement'].map(k => [k,r[k]]))) },
};
writeFileSync(new URL('../../js/account/src/routerBenchmark.json', import.meta.url), JSON.stringify(snapshot, null, 2)+'\n');
