#!/usr/bin/env python3
"""One-coordinate config hillclimb, not an autonomous prompt editor.
Kept config only changes on significant paired train AND test improvement.
Never consumes transcripts. Held-out cases remain with the evaluator.
"""
import argparse
import json
from pathlib import Path
import statistics
from run import ci


def decide(before, after):
    def index(rows):
        result={}
        for r in rows:
            key=(r['case_id'],r['repeat'])
            if key in result: raise ValueError('one configuration per results file required')
            if r.get('error') or r.get('mock'): raise ValueError('mock or plumbing failures cannot support hillclimbing')
            if r['grader_verdicts'][0]!=r['grader_verdicts'][1]: raise ValueError('unstable grader')
            result[key]=r
        return result
    a,b=index(before),index(after)
    if not a or a.keys()!=b.keys(): raise ValueError('exact paired case/repeat coverage required')
    results={}
    for split in ('train','test'):
        cases={}
        for key in a:
            if a[key]['split']!=b[key]['split']: raise ValueError('split drift')
            if a[key]['split']==split: cases.setdefault(key[0],[]).append(b[key]['score']-a[key]['score'])
        values=[statistics.mean(v) for v in cases.values()]
        bounds=ci(values)
        results[split]=dict(delta=statistics.mean(values) if values else None,ci95=bounds,within_noise=not values or bounds[0]<=0,independent_cases=len(values))
    keep=all(v['independent_cases']>=5 and v['delta']>0 and not v['within_noise'] for v in results.values())
    return dict(keep=keep,splits=results,advice='Keep candidate' if keep else 'Revert / do not merge; increase repeats or cases if within noise')


def main():
    p=argparse.ArgumentParser();p.add_argument('--baseline',required=True);p.add_argument('--candidate',required=True);p.add_argument('--baseline-config',required=True);p.add_argument('--candidate-config',required=True);p.add_argument('--kept-config',required=True);p.add_argument('--decision',required=True)
    a=p.parse_args(); old=json.loads(Path(a.baseline_config).read_text());new=json.loads(Path(a.candidate_config).read_text())
    changed=[k for k in old.keys()|new.keys() if old.get(k)!=new.get(k)]
    if len(changed)!=1 or changed[0] not in ('model','effort'): p.error('Exactly one model OR effort change allowed; no failure-derived prompt edits')
    read=lambda p:[json.loads(l) for l in Path(p).read_text().splitlines()]
    result=decide(read(a.baseline),read(a.candidate));result['changed_field']=changed[0]
    for path in (a.kept_config,a.decision):
        if Path(path).exists(): p.error('Output exists; never overwrite earlier round evidence')
    Path(a.decision).write_text(json.dumps(result,indent=2));Path(a.kept_config).write_text(json.dumps(new if result['keep'] else old,indent=2));print(json.dumps(result,indent=2))
if __name__=='__main__':main()
