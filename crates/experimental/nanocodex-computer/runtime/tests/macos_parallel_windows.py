"""Live public CLI regression: ONE companion, ONE eval, four exact windows.

Usage: python3 tests/macos_parallel_windows.py /path/to/nanocodex-computer
       [--report /path/to/report.md] [--timeout 120]
Requires granted macOS Accessibility and Screen Recording. Only owned accessory
fixtures receive input; neither fixture activation nor global cursor input is
requested. Timings are diagnostic, NOT a causal concurrency proof. Native lane
and async bridge barrier tests must establish that stronger property separately.
Human cursor/foreground activity is reported raw; activation of any owned process
is a failure. No global process kills, arbitrary foreground app, or production
hooks are used. Subprocess startup, eval, and cleanup are bounded.
"""
import argparse
import json
import math
import pathlib
import plistlib
import shutil
import subprocess
import tempfile
import time
import traceback


def stop(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)


def rows(path):
    # A sampling timer may currently be writing the final line.
    return [json.loads(line) for line in path.read_text().splitlines(keepends=True)
            if line.endswith('\n')]


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for item in value.values():
            yield from strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=pathlib.Path)
    repo = pathlib.Path(__file__).resolve().parents[5]
    parser.add_argument('--report', type=pathlib.Path,
                        default=repo / '.build/background-cua/single-companion-report.md')
    parser.add_argument('--timeout', type=float, default=120)
    args = parser.parse_args()
    binary = args.binary.resolve()
    report = {'passed': False, 'binary': str(binary), 'companion_processes': 1,
              'eval_calls': 1, 'causal_concurrency_proof': False,
              'timing_interpretation': 'Diagnostic only; no scheduler/barrier assertion.'}
    children, logs = [], []
    with tempfile.TemporaryDirectory(prefix='nanocodex-parallel-') as tmp:
        root = pathlib.Path(tmp)
        try:
            source = pathlib.Path(__file__).parent / 'fixtures/macos_parallel_windows.swift'
            headers = []
            for instance in range(2):
                bundle = root / f'Parallel{instance}.app/Contents'
                exe = bundle / 'MacOS/ParallelFixture'
                exe.parent.mkdir(parents=True)
                (bundle / 'Info.plist').write_bytes(plistlib.dumps({
                    'CFBundleIdentifier': f'org.nanocodex.parallel-proof{instance}',
                    'CFBundleExecutable': 'ParallelFixture', 'CFBundlePackageType': 'APPL',
                    'CFBundleName': f'Parallel proof {instance}', 'LSUIElement': True}))
                if instance == 0:
                    subprocess.run(['swiftc', str(source), '-o', str(exe)],
                                   check=True, capture_output=True, text=True, timeout=60)
                    first_exe = exe
                else:
                    shutil.copy2(first_exe, exe)
                log = root / f'fixture{instance}.jsonl'
                log.touch()
                child = subprocess.Popen([str(exe), str(log), str(instance)],
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                children.append(child)
                logs.append(log)
                deadline = time.monotonic() + 15
                while not rows(log):
                    assert child.poll() is None, 'fixture exited before ready'
                    assert time.monotonic() < deadline, 'fixture startup timed out'
                    time.sleep(.02)
                header = rows(log)[0]
                assert header['ready'] == child.pid, header
                headers.append(header)
            targets = [{'pid': h['ready'], 'windowId': window,
                        'token': f'P{instance}W{index}β🧪',
                        'width': 320 + (instance * 2 + index) * 40}
                       for instance, h in enumerate(headers)
                       for index, window in enumerate(h['windows'])]
            report['targets'] = targets
            # Creation is deliberately sequential: the parallel phase below tests
            # already-bound exact handles rather than racing discovery itself.
            code = '''const specs = SPECS;
const handles = [];
for (const s of specs) handles.push(await cua.getApp(String(s.pid), {windowId:s.windowId}));
const events = [];
const epoch = Date.now();
async function timed(label, task) {
  const event = {label, start_ms:Date.now()-epoch}; events.push(event);
  try { const value=await task(); event.end_ms=Date.now()-epoch; return value; }
  catch(e) {event.error=String(e);event.end_ms=Date.now()-epoch;throw e;}
}
function dimensions(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.length < 24)
    throw new Error('Expected encoded screenshot bytes');
  const u16=o=>bytes[o]*256+bytes[o+1];
  const u32=o=>bytes[o]*16777216+bytes[o+1]*65536+bytes[o+2]*256+bytes[o+3];
  if(bytes[0]===137 && bytes[1]===80 && bytes[2]===78 && bytes[3]===71)
    return {width:u32(16),height:u32(20),bytes:bytes.length,format:'png'};
  if(bytes[0]===255 && bytes[1]===216) {
    let p=2;
    while(p+4<bytes.length) {
      if(bytes[p++]!==255) throw new Error('Malformed JPEG marker');
      while(bytes[p]===255) p++;
      const marker=bytes[p++];
      if(marker===217 || marker===218) break;
      if(marker===1 || (marker>=208 && marker<=215)) continue;
      const size=u16(p);
      if(size<2 || p+size>bytes.length) throw new Error('Malformed JPEG segment');
      if([192,193,194,195,197,198,199,201,202,203,205,206,207].includes(marker))
        return {width:u16(p+5),height:u16(p+3),bytes:bytes.length,format:'jpeg'};
      p+=size;
    }
  }
  throw new Error('Screenshot dimensions unavailable');
}
// Four concurrent captures cover same-PID and different-PID native read lanes.
const initial = await Promise.all(handles.map((h,i)=>timed('initial-shot-'+i,
  async()=>dimensions(await h.getScreenshot({emit:false})))));
// Each writer's final AX snapshot must contain only its own exact-window token.
// A simultaneous observer branch repeatedly captures a different exact handle.
const results = await Promise.all([
  ...handles.map((h,i)=>timed('writer-'+i,async()=>{
    for(let n=0;n<4;n++) await h.typeText(specs[i].token.repeat(8));
    const state=await h.getAXState({emit:false,disableDiffing:true});
    if(!state.includes(specs[i].token.repeat(32))) throw new Error('Missing own AX receipt '+i);
    for(let j=0;j<specs.length;j++) if(j!==i && state.includes(specs[j].token))
      throw new Error('Cross-window AX receipt '+i+' from '+j);
    return {index:i,state};
  })),
  timed('observer',async()=>{
    const shots=[];
    for(let n=0;n<3;n++) shots.push(await Promise.all(handles.map((h,i)=>
      timed('shot-'+n+'-'+i,async()=>({index:i,...dimensions(await h.getScreenshot({emit:false}))})))));
    return {shots};
  })
]);
nodeRepl.write('PARALLEL_RESULT:'+JSON.stringify({initial,results,events,elapsed_ms:Date.now()-epoch}));
'''.replace('SPECS', json.dumps(targets))
            code_path = root / 'proof.js'
            code_path.write_text(code)
            # Exactly one public CLI child and one eval. Native helper subprocesses
            # are implementation details, not extra CLI companions.
            proc = subprocess.Popen([str(binary), '--allow-native-control', 'eval',
                                     '--file', str(code_path), '--timeout', str(max(1, int(args.timeout) - 5))], text=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            children.append(proc)
            report['companion_pid'] = proc.pid
            try:
                stdout, stderr = proc.communicate(timeout=args.timeout)
            except subprocess.TimeoutExpired:
                stop(proc)
                stdout, stderr = proc.communicate(timeout=5)
                report['stdout'], report['stderr'] = stdout, stderr
                raise AssertionError('single eval timed out')
            report['returncode'], report['stderr'] = proc.returncode, stderr
            report['stdout'] = stdout
            assert proc.returncode == 0, f'eval exited {proc.returncode}: {stderr}'
            output = json.loads(stdout)
            payloads = [s[len('PARALLEL_RESULT:'):] for s in strings(output)
                        if s.startswith('PARALLEL_RESULT:')]
            assert len(payloads) == 1, 'missing/duplicate final eval receipt'
            payload = json.loads(payloads[0])
            report['eval'] = payload
            writers = [e for e in payload['events'] if e['label'].startswith('writer-')]
            captures = [e for e in payload['events'] if e['label'].startswith('shot-')]
            report['capture_completions_during_writer_intervals'] = {
                relation: sum(
                    writer['start_ms'] < shot['end_ms'] < writer['end_ms']
                    and (targets[int(writer['label'].split('-')[-1])]['pid'] ==
                         targets[int(shot['label'].split('-')[-1])]['pid']) == same_pid
                    and writer['label'].split('-')[-1] != shot['label'].split('-')[-1]
                    for writer in writers for shot in captures)
                for relation, same_pid in [('same_pid_other_window', True), ('different_pid', False)]}
            report['timing_note'] = ('Counts compare JS task intervals, including scheduling and waits; '
                                     'they do not establish native operation overlap.')
            # Encoded dimensions independently identify routing; widths are unique.
            initial = payload['initial']
            assert len(initial) == 4 and len({s['width'] for s in initial}) == 4, initial
            scale = initial[0]['width'] / targets[0]['width']
            assert scale in (1, 2), ('unexpected capture scale', scale)
            for target, shot in zip(targets, initial):
                assert shot['width'] == target['width'] * scale, (target, shot)
            for batch in payload['results'][4]['shots']:
                assert len(batch) == 4, batch
                for index, shot in enumerate(batch):
                    assert shot['index'] == index and shot['width'] == initial[index]['width'], shot
                    assert shot['height'] == initial[index]['height'], shot
            deadline = time.monotonic() + 3
            while True:
                receipt_text = []
                for instance, log in enumerate(logs):
                    data = rows(log)
                    for index in range(2):
                        received = [r['text'] for r in data if r.get('window') == index]
                        receipt_text.append(received[-1] if received else '')
                if receipt_text == [t['token'] * 32 for t in targets]:
                    break
                assert time.monotonic() < deadline, ('incorrect fixture receipts', receipt_text)
                time.sleep(.02)
            report['fixture_text_receipts'] = receipt_text
            report['receipt_routing_correct'] = True
            report['passed'] = True
            del report['stdout']
        except Exception as error:
            report['error'] = str(error)
            report['traceback'] = traceback.format_exc()
        finally:
            # Read monitoring receipts even when the eval fails. Human activity
            # cannot be distinguished from synthetic motion by these samples.
            owned = {p.pid for p in children}
            args.report.parent.mkdir(parents=True, exist_ok=True)
            report['fixture_log_artifacts'] = []
            monitors = []
            for log_index, log in enumerate(logs):
                artifact = args.report.with_name(args.report.stem + f'.fixture{log_index}.jsonl')
                artifact.write_text(log.read_text())
                report['fixture_log_artifacts'].append(str(artifact))
                data = rows(log)
                if not data:
                    continue
                samples = [r for r in data if 'front' in r and 'cursor' in r]
                first = samples[0]
                monitors.append({
                    'sample_count': len(samples), 'initial_front': first['front'],
                    'front_pids': sorted({r['front'] for r in samples}),
                    'front_unchanged': all(r['front'] == first['front'] for r in samples),
                    'owned_process_became_frontmost': any(r['front'] in owned for r in samples),
                    'max_cursor_displacement_points': max(math.dist(r['cursor'], first['cursor']) for r in samples),
                    'cursor_movement_samples': sum(a['cursor'] != b['cursor'] for a,b in zip(samples,samples[1:]))})
            report['monitoring'] = monitors
            report['foreground_unchanged_in_samples'] = bool(monitors) and all(m['front_unchanged'] for m in monitors) and len({m['initial_front'] for m in monitors}) == 1
            report['cursor_unchanged_in_samples'] = bool(monitors) and all(m['max_cursor_displacement_points'] == 0 for m in monitors)
            if any(m['owned_process_became_frontmost'] for m in monitors):
                report['passed'] = False
                report['foreground_error'] = 'An owned test process became frontmost'
            cleanup_errors = []
            for child in reversed(children):
                try:
                    stop(child)
                except Exception as error:
                    cleanup_errors.append(str(error))
            if cleanup_errors:
                report['passed'] = False
                report['cleanup_errors'] = cleanup_errors
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text('# Single-companion parallel window live regression\n\n'
                'One public CLI eval uses Promise.all on four exact handles across two fixture PIDs. '
                'Timing intervals are diagnostic and do not prove causal independence. '
                'Foreground/cursor observations are sampled; nonzero motion may be human activity.\n\n'
                '```json\n' + json.dumps(report, indent=2, ensure_ascii=False) + '\n```\n')
    print(json.dumps({k: v for k, v in report.items() if k not in ('stdout', 'eval')}, indent=2, ensure_ascii=False))
    print(f'Report: {args.report}')
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
