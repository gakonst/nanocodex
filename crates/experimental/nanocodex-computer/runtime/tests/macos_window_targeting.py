"""Live public CLI proof: swiftc + granted companion, owned windows only.
Run: python3 tests/macos_window_targeting.py /path/to/nanocodex-computer
"""
import concurrent.futures, json, math, pathlib, plistlib, subprocess, sys, tempfile, time
binary = str(pathlib.Path(sys.argv[1]).resolve())
cursor_tolerance = float(sys.argv[2]) if len(sys.argv)>2 else 0.0
fixture = pathlib.Path(__file__).parent / 'fixtures/macos_windows.swift'
with tempfile.TemporaryDirectory(prefix='nanocodex-windows-') as tmp:
    root = pathlib.Path(tmp)
    bundle = root / 'OwnedWindows.app'
    exe = bundle / 'Contents/MacOS/OwnedWindows'
    exe.parent.mkdir(parents=True)
    (bundle / 'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier':'org.nanocodex.owned-windows','CFBundleExecutable':'OwnedWindows','CFBundleName':'OwnedWindows','CFBundlePackageType':'APPL'}))
    subprocess.run(['swiftc',str(fixture),'-o',str(exe)],check=True)
    log = root / 'receipts.jsonl'; log.touch()
    child = subprocess.Popen([str(exe),str(log)])
    try:
        deadline = time.monotonic()+15
        while not log.read_text():
            assert time.monotonic()<deadline, 'fixture startup timeout'
            time.sleep(.02)
        header = json.loads(log.read_text().splitlines()[0])
        print('fixture',json.dumps(header),flush=True)
        windows = header['windows']
        def run(index):
            # Public CUA creates two persistent handles in each independent companion.
            # Both writers contend for the same PID, but operate on disjoint windows.
            code = f'''const ws=await cua.listWindows('{child.pid}',{{emit:false}});
const a=await cua.getApp('{child.pid}',{{windowId:{windows[0]}}});
const b=await cua.getApp('{child.pid}',{{windowId:{windows[1]}}});
const target=[a,b][{index}];
for(let i=0;i<4;i++) await target.typeText('W{index}β🧪');
const state=await target.getAXState({{emit:false,disableDiffing:true}});
nodeRepl.write({{windows:ws,state}});'''
            result = subprocess.run([binary,'--allow-native-control','eval','--code',code],text=True,capture_output=True,timeout=50)
            print('companion', index, result.returncode, result.stdout, result.stderr, flush=True)
            assert result.returncode==0, f'companion {index} failed'
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            list(pool.map(run,range(2)))
        time.sleep(.1)
        rows = [json.loads(line) for line in log.read_text().splitlines()]
        for i in range(2):
            received = [r['text'] for r in rows if r.get('window')==i]
            assert received and received[-1]==f'W{i}β🧪'*4, received
        displacement = max(math.dist(row['cursor'],header['cursor']) for row in rows if 'cursor' in row)
        print(json.dumps({'text_receipts_correct':True,'front_unchanged':all(row.get('front',header['front'])==header['front'] for row in rows),'max_cursor_displacement_points':displacement,'cursor_tolerance_points':cursor_tolerance}),flush=True)
        for row in rows:
            if 'front' in row: assert row['front']==header['front'], ('foreground changed',row)
        assert displacement <= cursor_tolerance, ('cursor changed',displacement)
        print(json.dumps({'passed':True,'same_pid_windows':windows,'companion_processes':2,'transactions':8,'front':header['front'],'cursor':header['cursor'],'samples':len(rows)}),flush=True)
    finally:
        print("fixture_exit",child.poll(),"receipts",log.read_text()[-4000:],flush=True)
        child.terminate(); child.wait(timeout=5)
