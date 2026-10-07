#!/usr/bin/env python3
"""Export file-verifiable cases as native nanoeval/Harbor tasks (no model answers)."""
import argparse
import json
from pathlib import Path
import shutil
from run import safe, validate

def export(cases, out):
    validate(cases)
    out.mkdir(parents=True,exist_ok=False)
    for case in cases:
        if any(c['kind'].startswith('answer_') for c in case['checks']):
            raise ValueError('nanoeval exporter requires filesystem checks')
        task=safe(out,case['case_id']); env=task/'environment';tests=task/'tests'
        (env/'fixture').mkdir(parents=True); tests.mkdir()
        for name,content in case.get('files',{}).items():
            p=safe(env/'fixture',name);p.parent.mkdir(parents=True,exist_ok=True);p.write_text(content)
        (env/'fixture/.keep').touch()
        (task/'instruction.md').write_text(case['prompt'])
        (task/'task.toml').write_text('schema_version = "1.1"\n[task]\nname = '+json.dumps('differential/'+case['case_id'])+'\ndescription = '+json.dumps(case['hard_reason'])+'\n[agent]\ntimeout_sec = 180.0\n[verifier]\ntimeout_sec = 30.0\n[environment]\ndocker_image = "alpine:3.21"\ncpus = 1\nmemory_mb = 512\nstorage_mb = 256\ngpus = 0\nallow_internet = false\n')
        (env/'Dockerfile').write_text('FROM alpine:3.21\nRUN apk add --no-cache python3 git bash ripgrep\nWORKDIR /app\nCOPY fixture/ ./\nRUN git init -q\n')
        (tests/'case.json').write_text(json.dumps(case))
        shutil.copyfile(Path(__file__).with_name('run.py'), tests/'grader.py')
        (tests/'verify.py').write_text('import json, os\nfrom pathlib import Path\nfrom grader import grade\ncase=json.loads(Path(__file__).with_name("case.json").read_text())\nv=grade(case,Path.cwd(),"")\nscore=sum(x["pass"] for x in v)/len(v)\nlogs=Path(os.environ.get("NANOEVAL_VERIFIER_LOGS","/logs/verifier"));logs.mkdir(parents=True,exist_ok=True)\n(logs/"reward.txt").write_text(str(score)+"\\n")\nprint(json.dumps(v))\n')
        (tests/'test.sh').write_text('#!/bin/sh\nset -eu\ncd "${NANOEVAL_WORKSPACE:-/app}"\npython3 "$(dirname "$0")/verify.py"\n');(tests/'test.sh').chmod(0o755)
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--cases',required=True);p.add_argument('--out',required=True);a=p.parse_args();export(json.loads(Path(a.cases).read_text()),Path(a.out))
