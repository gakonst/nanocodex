#!/usr/bin/env python3
"""Explicitly publish a copied addon export for the generic snapshot provider."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
from project import project

MAX_INPUT = 512 * 1024
MAX_DATA = 8000


def encode(value):
    return json.dumps(value,ensure_ascii=False,allow_nan=False,separators=(',',':')).encode('utf-8')


def envelope(source, app, window, now_ms=None):
    for label in (app, window):
        if not label or len(label.encode())>512 or any(ord(c)<32 or 127<=ord(c)<=159 for c in label):
            raise ValueError('invalid application/window identity')
    captured, data = project(source)
    now_ms = round(time.time()*1000) if now_ms is None else now_ms
    if captured > now_ms+1000 or now_ms-captured>60_000:
        raise ValueError('copy a fresh /ncobserve export before publishing')
    # Prefer readable labels over unlabeled controls; preserve original bounds.
    data['elements'].sort(key=lambda e: not any(any(c.isalpha() for c in s) for s in e['labels']))
    data['omitted_elements']=0
    while len(encode(data))>MAX_DATA and data['elements']:
        data['elements'].pop();data['omitted_elements']+=1;data['partial']=True
    while len(encode(data))>MAX_DATA and data['recent_speech']:
        data['recent_speech'].pop(0);data['partial']=True
    if len(encode(data))>MAX_DATA:
        raise ValueError('observation cannot fit provider budget')
    return {'schemaVersion':1,'capturedAt':captured,'app':app,'window':window,'data':data}


def publish(value, output):
    output=Path(output).absolute()
    output.parent.mkdir(parents=True,exist_ok=True)
    fd,temp=tempfile.mkstemp(prefix='.'+output.name+'.',dir=output.parent)
    try:
        with os.fdopen(fd,'wb') as file:
            file.write(encode(value));file.flush();os.fsync(file.fileno())
        os.replace(temp,output)
    finally:
        if os.path.exists(temp): os.unlink(temp)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    group=p.add_mutually_exclusive_group(required=True)
    group.add_argument('--input',type=Path)
    group.add_argument('--clipboard',action='store_true',help='read the explicit copy once; never watch clipboard')
    p.add_argument('--app',required=True);p.add_argument('--window',required=True)
    p.add_argument('--output',type=Path,required=True)
    a=p.parse_args()
    if a.clipboard:
        # A one-shot read initiated explicitly by the operator, not by observe().
        with tempfile.TemporaryFile() as f:
            subprocess.run(['wl-paste','--no-newline'],stdout=f,stderr=subprocess.DEVNULL,timeout=3,check=True)
            f.seek(0);raw=f.read(MAX_INPUT+1)
    else:
        with a.input.open('rb') as f: raw=f.read(MAX_INPUT+1)
    if len(raw)>MAX_INPUT: raise ValueError('export exceeds input budget')
    result=envelope(json.loads(raw),a.app,a.window)
    publish(result,a.output)
    print(json.dumps({'published':str(a.output.absolute()),'capturedAt':result['capturedAt'],
                      'elements':len(result['data']['elements']),'partial':result['data']['partial']}))


if __name__=='__main__':
    try: main()
    except (ValueError,KeyError,TypeError,OSError,subprocess.SubprocessError):
        raise SystemExit('No snapshot published: invalid/stale export or clipboard/file unavailable.')
