#!/usr/bin/env python3
"""Paired, isolated-state CLI evaluation. Stdlib only; native mode is NOT a sandbox."""
import argparse
import hashlib
import html
import json
import os
from pathlib import Path
import random
import signal
import shutil
import statistics
import subprocess
import tempfile
import time


def safe(root, name):
    p = root / name
    if p.is_symlink() or not p.resolve().is_relative_to(root.resolve()):
        raise ValueError('unsafe fixture/check path')
    return p


def grade(case, root, answer):
    verdicts = []
    for c in case['checks']:
        try:
            kind = c['kind']
            p = safe(root, c['path']) if 'path' in c else None
            if kind == 'file_equals':
                ok = p.is_file() and p.read_text() == c['expected']
            elif kind == 'file_absent':
                ok = not p.exists()
            elif kind == 'answer_contains':
                ok = c['expected'] in answer
            elif kind == 'answer_equals':
                ok = answer.strip() == c['expected']
            elif kind == 'json_equals':
                ok = json.loads(p.read_text()) == c['expected']
            else:
                raise ValueError('unknown check ' + kind)
        except (OSError, ValueError, UnicodeError):
            ok = False
        verdicts.append({'claim': c.get('claim', str(c)), 'pass': bool(ok)})
    return verdicts


def validate(cases):
    ids = set()
    for c in cases:
        assert c['case_id'] not in ids, 'duplicate case'
        ids.add(c['case_id'])
        assert c['tags'] and c['hard_reason'] and c['prompt'] and c['checks']
        assert c['split'] in ('train', 'test')
        for path in c.get('files', {}):
            assert not Path(path).is_absolute() and '..' not in Path(path).parts
        for check in c['checks']:
            assert check['kind'] in ('file_equals','file_absent','answer_contains','answer_equals','json_equals')


def command(cfg, root, prompt):
    fields = dict(workspace=str(root), model=cfg['model'], effort=cfg['effort'], prompt=prompt)
    if 'argv' in cfg:
        return [a.format(**fields) for a in cfg['argv']]
    if cfg['agent'] == 'stock_codex':
        return ['codex','exec','--json','--skip-git-repo-check','-s','workspace-write','-c','approval_policy="never"','-m',cfg['model'],'-c',f'model_reasoning_effort="{cfg["effort"]}"','-C',str(root),prompt]
    if cfg['agent'] == 'nanocodex':
        return ['nanocodex','--harness','codex','--model',cfg['model'],'--thinking',cfg['effort'],'--cwd',str(root),'--memory','false','run',prompt]
    raise ValueError('custom agent requires argv')


def decode(stdout):
    """Stock JSONL or explicit custom adapter contract. Unknown streams fail closed."""
    answer, tokens, cost, completed, error = '', None, None, False, None
    for line in stdout.splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if not isinstance(e, dict):
            continue
        if 'answer' in e and 'completed' in e:
            answer = e['answer']; tokens = e.get('tokens'); cost = e.get('cost')
            completed = bool(e['completed']); error = e.get('error')
        if e.get('type') == 'item.completed' and e.get('item', {}).get('type') == 'agent_message':
            answer += e['item'].get('text', '')
        if e.get('type') == 'turn.completed':
            completed = True; tokens = e.get('usage')
        if e.get('type') == 'assistant.message':
            answer += e.get('payload', {}).get('text', '')
        if e.get('type') == 'run.completed':
            payload = e.get('payload', {})
            completed = payload.get('status') == 'completed'
            tokens = payload.get('usage'); cost = payload.get('cost_usd')
        if e.get('type') in ('error', 'turn.failed', 'run.failed', 'run.error'):
            error = 'provider_error'
    return dict(answer=answer, tokens=tokens, cost=cost, completed=completed, error=error)


def trial(case, cfg, repeat, out, mock, timeout, auth_file=None):
    started = time.monotonic(); error = None; stdout = stderr = ''; result = {}
    with tempfile.TemporaryDirectory(prefix='differential-') as tmp:
        root = Path(tmp) / 'workspace'; root.mkdir()
        home = Path(tmp) / 'agent-home'; home.mkdir()
        for name, content in case.get('files', {}).items():
            p = safe(root, name); p.parent.mkdir(parents=True, exist_ok=True); p.write_text(content)
        subprocess.run(['git','init','-q',str(root)], check=True, capture_output=True)
        env = {k:v for k,v in os.environ.items() if k in ('PATH','LANG','LC_ALL','TMPDIR','OPENAI_API_KEY','ANTHROPIC_API_KEY')}
        env.update(HOME=str(home), CODEX_HOME=str(home / '.codex'), XDG_CONFIG_HOME=str(home / '.config'))
        (home / '.codex').mkdir()
        if auth_file and not mock:
            # Same private-file auth pattern as benchmarks/stock_codex_fork_bench.py.
            dest = home / '.codex/auth.json'
            shutil.copyfile(auth_file, dest); dest.chmod(0o600)
        if mock:
            # Plumbing-only oracle, intentionally not evidence of agent capability.
            answer = ''
            for check in case['checks']:
                if check['kind'] in ('file_equals','json_equals'):
                    p = safe(root, check['path']); p.parent.mkdir(parents=True, exist_ok=True)
                    p.write_text(check['expected'] if check['kind']=='file_equals' else json.dumps(check['expected']))
                elif check['kind'] == 'file_absent':
                    safe(root, check['path']).unlink(missing_ok=True)
                else:
                    answer += check['expected']
            result = dict(answer=answer, completed=True, tokens=None, cost=None)
            stdout = json.dumps(result)
        else:
            argv = command(cfg, root, case['prompt'])
            try:
                process = subprocess.Popen(argv, cwd=root, env=env, text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
                try:
                    stdout, stderr = process.communicate(None if case['prompt'] in argv else case['prompt'], timeout=timeout)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    stdout, stderr = process.communicate(); error = 'timeout'
                if process.returncode and not error:
                    error = f'exit_{process.returncode}'
                result = decode(stdout)
                error = error or result.get('error') or (None if result['completed'] else 'missing_completion')
            except OSError as exc:
                error = type(exc).__name__
        first = grade(case, root, result.get('answer',''))
        second = grade(case, root, result.get('answer',''))
        if first != second:
            error = 'nondeterministic_grader'
        score = sum(v['pass'] for v in first) / len(first) if not error else 0.0
    # Never serialize environment, API keys or command-line auth material.
    for key in ('OPENAI_API_KEY','ANTHROPIC_API_KEY'):
        secret = os.environ.get(key)
        if secret:
            stdout = stdout.replace(secret,'[REDACTED]'); stderr = stderr.replace(secret,'[REDACTED]')
    identity = hashlib.sha256(json.dumps([case['case_id'],cfg,repeat],sort_keys=True).encode()).hexdigest()[:20]
    trace = 'transcripts/' + identity + '.json'
    (out / trace).write_text(json.dumps(dict(stdout=stdout,stderr=stderr,mock=mock,grader_runs=[first,second]),indent=2))
    return dict(case_id=case['case_id'],agent=cfg['agent'],model=cfg['model'],effort=cfg['effort'],repeat=repeat,score=score,grader_verdicts=[first,second],tokens=result.get('tokens'),latency_ms=round((time.monotonic()-started)*1000),cost=result.get('cost'),error=error,transcript_path=trace,split=case['split'],mock=mock)


def ci(values, seed=71):
    if not values: return [None,None]
    rng = random.Random(seed)
    means = sorted(statistics.mean(rng.choices(values,k=len(values))) for _ in range(2000))
    return [means[49],means[1949]]


def report(rows, out):
    groups = {}
    for r in rows:
        key = (r['agent'],r['model'],r['effort'])
        groups.setdefault(key,[]).append(r)
    summary = []
    for key, rs in groups.items():
        cases = {}
        for r in rs: cases.setdefault(r['case_id'],[]).append(r['score'])
        vals = [statistics.mean(v) for v in cases.values()]
        mean = statistics.mean(vals)
        summary.append(dict(config=key,mean=mean,ci95=ci(vals),case_count=len(vals),trials=len(rs),errors=sum(bool(r['error']) for r in rs),headroom_warning=mean>=.95,all_fail_cases=[k for k,v in cases.items() if max(v)==0]))
    warnings = ['MOCK ONLY: no capability inference'] if any(r.get('mock') for r in rows) else []
    if any(s['case_count']<10 for s in summary): warnings.append('Too few independent cases for reliable CI')
    comparisons = []
    keys = list(groups)
    for a,b in zip(keys,keys[1:]):
        av={(r['case_id'],r['repeat']):r['score'] for r in groups[a]}
        bv={(r['case_id'],r['repeat']):r['score'] for r in groups[b]}
        deltas={}
        for k in av.keys() & bv.keys(): deltas.setdefault(k[0],[]).append(bv[k]-av[k])
        vals=[statistics.mean(v) for v in deltas.values()]
        bounds=ci(vals)
        comparisons.append(dict(baseline=a,candidate=b,delta=statistics.mean(vals) if vals else None,ci95=bounds,within_noise=not vals or bounds[0]<=0<=bounds[1]))
    data=dict(groups=summary,comparisons=comparisons,warnings=warnings,ci_method='paired case-cluster percentile bootstrap; 2000 resamples; fixed seed; repeats averaged within case')
    (out/'summary.json').write_text(json.dumps(data,indent=2))
    esc=lambda x:html.escape(str(x),quote=True)
    table=''.join('<tr><td>'+esc(r['case_id'])+'</td><td>'+esc(r['agent'])+'</td><td>'+esc(r['model'])+'/'+esc(r['effort'])+'</td><td>'+esc(r['repeat'])+'</td><td>'+esc(r['score'])+'</td><td>'+esc(r['error'])+'</td><td><a href="'+esc(r['transcript_path'])+'">trace</a></td></tr>' for r in rows)
    (out/'index.html').write_text('<!doctype html><meta charset="utf-8"><title>Differential eval</title><h1>Differential eval</h1><pre>'+esc(json.dumps(data,indent=2))+'</pre><table><tr><th>Case<th>Agent<th>Config<th>Repeat<th>Score<th>Error<th>Evidence</tr>'+table+'</table>')
    return data


def main():
    p=argparse.ArgumentParser(); p.add_argument('--cases',required=True); p.add_argument('--config',required=True); p.add_argument('--out',required=True); p.add_argument('--repeats',type=int,default=3); p.add_argument('--timeout',type=float,default=180); p.add_argument('--mock',action='store_true'); p.add_argument('--limit',type=int); p.add_argument('--split',choices=['train','test']); p.add_argument('--allow-native',action='store_true'); p.add_argument('--auth-file',type=Path)
    args=p.parse_args()
    if not args.mock and not args.allow_native: p.error('native execution is not an isolation boundary; use a disposable host and --allow-native')
    if args.repeats<1: p.error('repeats must be positive')
    cases=json.loads(Path(args.cases).read_text()); validate(cases)
    if args.split: cases=[c for c in cases if c['split']==args.split]
    cases=cases[:args.limit]; configs=json.loads(Path(args.config).read_text())
    out=Path(args.out).resolve(); out.mkdir(parents=True,exist_ok=False); (out/'transcripts').mkdir()
    (out/'manifest.json').write_text(json.dumps(dict(cases_sha256=hashlib.sha256(Path(args.cases).read_bytes()).hexdigest(),configs=configs,repeats=args.repeats,mock=args.mock),indent=2))
    rows=[]; rng=random.Random(20261007)
    with (out/'results.jsonl').open('w') as f:
        for rep in range(args.repeats):
            order=list(cases); rng.shuffle(order)
            for idx,case in enumerate(order):
                variants=configs if (rep+idx)%2==0 else list(reversed(configs))
                for cfg in variants:
                    r=trial(case,cfg,rep,out,args.mock,args.timeout,args.auth_file); rows.append(r); f.write(json.dumps(r)+'\n'); f.flush()
    print(json.dumps(report(rows,out),indent=2))

if __name__=='__main__': main()
