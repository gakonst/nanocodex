"""Owned live cursor test; requires native macOS Accessibility/Screen Recording.
Usage: python3 tests/macos_cursors.py /path/to/nanocodex-computer [artifact-dir]
Human activity is allowed; receipts report strict foreground/cursor stability
separately. No event is sent to an existing user application.
"""
import json, subprocess, pathlib, time, select, os, sys, tempfile, shutil, plistlib
tests=pathlib.Path(__file__).resolve().parent
root=pathlib.Path(sys.argv[2]).resolve() if len(sys.argv)>2 else pathlib.Path(tempfile.mkdtemp(prefix='nanocodex-cursors-'))
root.mkdir(parents=True,exist_ok=True)
binary=pathlib.Path(sys.argv[1]).resolve()
source=(tests.parent/'src/native/macos_background_fixture.swift').read_text().replace('x: 60,y: 60','x: Double(CommandLine.arguments[2])!,y: 60').replace('withTimeInterval: 20,','withTimeInterval: 90,')
(root/'Fixture.swift').write_text(source)
for index in range(2):
    bundle=root/('CursorFixture.app' if index==0 else 'CursorFixture2.app')/'Contents'
    (bundle/'MacOS').mkdir(parents=True,exist_ok=True)
    (bundle/'Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier':f'org.nanocodex.cursor-proof{index}','CFBundleExecutable':'CursorFixture','CFBundleName':'Nanocodex Cursor Proof','CFBundlePackageType':'APPL','LSUIElement':True}))
    if index==0:subprocess.run(['swiftc',str(root/'Fixture.swift'),'-o',str(bundle/'MacOS/CursorFixture')],check=True)
    else:shutil.copy2(root/'CursorFixture.app/Contents/MacOS/CursorFixture',bundle/'MacOS/CursorFixture')
subprocess.run(['swiftc',str(tests/'fixtures/macos_cursor_probe.swift'),'-o',str(root/'probe')],check=True)
children=[]
report={}
owned_companion_pids=set()
def probe(pid):
    # Native cursor panels now belong to persistent per-window children. Keep
    # observed child IDs through reset so orphaned panels cannot escape checks.
    if pid:
        owned_companion_pids.add(pid)
        found=subprocess.run(['pgrep','-P',str(pid)],capture_output=True,text=True)
        assert found.returncode in (0,1), found.stderr
        owned_companion_pids.update(int(value) for value in found.stdout.split())
    pids=sorted(owned_companion_pids) if pid else [0]
    result=json.loads(subprocess.check_output([str(root/'probe'),*map(str,pids)],text=True))
    result['companion_pids']=pids
    return result
try:
    before=probe(0); report['before']=before
    fixtures=[]
    for index,x in enumerate([60,460]):
        log=root/f'fixture{index}.log';log.write_text('')
        proc=subprocess.Popen([str(root/('CursorFixture.app' if index==0 else 'CursorFixture2.app')/'Contents/MacOS/CursorFixture'),str(log),str(x)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);children.append(proc)
        deadline=time.monotonic()+10
        while not log.read_text().startswith('ready '):
            assert time.monotonic()<deadline, 'fixture startup'
            time.sleep(.02)
        ready=log.read_text().splitlines()[0].split()
        fixtures.append((int(ready[1]),int(ready[2]),list(map(float,ready[3:7]))))
    err=open(root/'companion.stderr','w')
    proc=subprocess.Popen([str(binary),'--allow-native-control','serve'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=err,text=True,bufsize=1);children.append(proc)
    seq=0
    def rpc(method,params):
        global seq
        seq+=1
        proc.stdin.write(json.dumps({'jsonrpc':'2.0','id':seq,'method':method,'params':params})+'\n');proc.stdin.flush()
        while True:
            assert select.select([proc.stdout],[],[],35)[0], 'RPC timed out'
            line=proc.stdout.readline();assert line, 'companion exited'
            value=json.loads(line)
            if value.get('id')==seq:
                assert 'error' not in value, value
                return value['result']
    rpc('initialize',{'protocolVersion':'2025-03-26','capabilities':{},'clientInfo':{'name':'cursor-proof','version':'1'}})
    proc.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');proc.stdin.flush()
    def js(code):
        result=rpc('tools/call',{'name':'js','arguments':{'code':code}})
        with open(root/'responses.jsonl','a') as f:f.write(json.dumps(result)+'\n')
        assert not result.get('isError'), str(result)[:1400]
        return result
    for index,(pid,window,frame) in enumerate(fixtures):
        result=js(f'let target{index} = await cua.getApp("{pid}", {{ windowId: {window} }});')
        texts=[c['text'] for c in result.get('content',[]) if c.get('type')=='text']
        assert any('1 text entry area' in t for t in texts), 'owned text target missing'
    js('await target0.getScreenshot({emit:false}); await target1.getScreenshot({emit:false});')
    started=time.monotonic()
    js('await target0.click([280,150]); await target1.click([280,150]);')
    report['clicks_ms']=(time.monotonic()-started)*1000
    report['visible']=probe(proc.pid)
    frame=fixtures[0][2]; region=f'{int(frame[0])},{int(frame[1])},760,{int(frame[3])}'

    if report['visible']['windows']:
        cursor_id=str(report['visible']['windows'][0]['kCGWindowNumber'])
        # A system screenshot may outlive the 1.2s visual indicator. Record this
        # optional render artifact separately from delivery/lifecycle assertions.
        try:
            subprocess.run(['/usr/sbin/screencapture','-x','-o','-l',cursor_id,str(root/'cursor.png')],check=True,timeout=2)
            report['cursor_capture']='saved'
        except (subprocess.CalledProcessError,subprocess.TimeoutExpired) as error:
            report['cursor_capture']=str(error)
    time.sleep(1.5)
    report['faded']=probe(proc.pid)
    report['unchanged_foreground']=all(state['front']==before['front'] for state in [report['visible'],report['faded']])
    report['unchanged_cursor']=all(state['cursor']==before['cursor'] for state in [report['visible'],report['faded']])
    for state in [report['visible'],report['faded']]:
        assert state['front'] not in list(owned_companion_pids)+[f[0] for f in fixtures], 'agent activated owned target or overlay'
    assert len(report['visible']['windows'])>=2, 'two virtual cursor windows not visible'
    assert len(report['faded']['windows'])==0, 'virtual cursor did not expire while idle'
    js('await target0.getScreenshot({emit:false}); await target1.getScreenshot({emit:false});')
    started=time.monotonic()
    js('await target0.click([280,150]); await target1.click([280,150]);')
    report['warm_clicks_ms']=(time.monotonic()-started)*1000
    rpc('tools/call',{'name':'js_reset','arguments':{}})
    deadline=time.monotonic()+.5
    while True:
        report['reset']=probe(proc.pid)
        if not report['reset']['windows'] or time.monotonic()>=deadline:break
        time.sleep(.02)
    assert len(report['reset']['windows'])==0, 'reset left cursor windows'
    for index in range(2):
        receipt=(root/f'fixture{index}.log').read_text()
        assert 'pointer type=1 ' in receipt and 'pointer type=2 ' in receipt, 'owned target did not receive a balanced click'
    report['passed']=True
finally:
    for p in reversed(children):
        if p.poll() is None:p.terminate()
    for p in children:
        try:p.wait(timeout=5)
        except subprocess.TimeoutExpired:p.kill();p.wait()
    (root/'result.json').write_text(json.dumps(report,indent=2))
print(json.dumps(report,indent=2))
