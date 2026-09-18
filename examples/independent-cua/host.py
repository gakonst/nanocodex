"""Owned-window demo adapter. Start once locally, over SSH, or on single-use loopback TCP.

Configured state files are an oracle from owned test apps, not a general UI API.
Only observe and validated button clicks are accepted on the JSONL wire.
"""
import concurrent.futures
import json
import os
import queue
import subprocess
import sys
import threading
import time
from pathlib import Path


class Companion:
    def __init__(self, config, lane):
        self.messages = queue.Queue(maxsize=16)
        self.seq = 0
        self.closed = threading.Event()
        self.close_lock = threading.Lock()
        self.failure = None
        with open(lane['log'], 'w') as log:
            self.process = subprocess.Popen([config['companion'], '--allow-native-control', 'serve'],
                env=dict(os.environ, **config['env']), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=log, text=True)
        threading.Thread(target=self.read, daemon=True).start()

    def fail(self, error):
        self.failure = error
        # Wake an RPC even when a broken companion has filled the queue.
        while True:
            try:
                self.messages.put_nowait(error)
                return
            except queue.Full:
                try:
                    self.messages.get_nowait()
                except queue.Empty:
                    pass

    def read(self):
        try:
            while not self.closed.is_set():
                line = self.process.stdout.readline(32 * 1024 * 1024 + 1)
                if not line:
                    raise RuntimeError('companion EOF; do not replay input')
                if len(line) > 32 * 1024 * 1024:
                    raise RuntimeError('companion response exceeds 32 MiB')
                reply = json.loads(line)
                if not isinstance(reply, dict) or reply.get('jsonrpc') != '2.0':
                    raise RuntimeError('malformed companion response')
                self.messages.put_nowait(reply)
        except Exception as exc:
            self.fail(RuntimeError('companion protocol failure: '+str(exc)))

    def initialize(self, lane):
        self.rpc('initialize', dict(protocolVersion='2025-06-18', capabilities={},
            clientInfo=dict(name='independent-window-lane', version='1')))
        self.js('var app = await cua.getApp('+json.dumps(str(lane['pid']))+');')

    def rpc(self, method, params):
        if self.failure or self.closed.is_set():
            raise self.failure or RuntimeError('companion closed')
        self.seq += 1
        self.process.stdin.write(json.dumps(dict(jsonrpc='2.0', id=self.seq,
            method=method, params=params))+'\n')
        self.process.stdin.flush()
        end = time.monotonic()+15
        while True:
            if self.failure:
                raise self.failure
            try:
                reply = self.messages.get(timeout=max(.001, end-time.monotonic()))
            except queue.Empty:
                raise TimeoutError('companion deadline; do not replay input') from None
            if isinstance(reply, Exception):
                raise reply
            if 'id' not in reply and isinstance(reply.get('method'), str):
                if time.monotonic() >= end:
                    raise TimeoutError('companion deadline; do not replay input')
                continue
            if type(reply.get('id')) is not int or reply['id'] != self.seq:
                raise RuntimeError('unexpected companion response id')
            if 'error' in reply:
                raise RuntimeError(str(reply)[:1000])
            result = reply.get('result')
            if not isinstance(result, dict) or result.get('isError'):
                raise RuntimeError('invalid or failed companion result: '+str(reply)[:1000])
            return result

    def js(self, code):
        return self.rpc('tools/call', dict(name='js', arguments=dict(code=code)))

    def close(self):
        with self.close_lock:
            if self.closed.is_set():
                return
            self.closed.set()
            self.fail(RuntimeError('companion closed; do not replay input'))
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    try:
                        self.process.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        pass


class RejectedRequest(ValueError):
    """Validated rejection: no uncertain companion operation occurred."""


class Lane:
    def __init__(self, config, lane):
        self.config, self.lane = config, lane
        self.lock = threading.Lock()
        self.companion = None
        self.failed = False
        self.lifecycle_lock = threading.Lock()

    def state(self):
        state = json.loads(Path(self.lane['state']).read_text())
        if state['pid'] != self.lane['pid']:
            raise RuntimeError('window identity changed')
        os.kill(self.lane['pid'], 0)
        return state

    def close(self):
        with self.lifecycle_lock:
            self.failed = True
            companion = self.companion
        if companion is not None:
            try:
                companion.close()
            except Exception:
                # Preserve the original failure and continue closing other lanes.
                pass

    def execute(self, request):
        try:
            return self._execute(request)
        except RejectedRequest:
            raise
        except BaseException:
            self.close()
            raise

    def _execute(self, request):
        with self.lifecycle_lock:
            if self.failed:
                raise RuntimeError('lane quarantined; reconcile before restarting')
            created = self.companion is None
            if created:
                # Publish before initialization can block, so disconnect can close it.
                self.companion = Companion(self.config, self.lane)
        if created:
            self.companion.initialize(self.lane)
        state = self.state()
        if request['operation'] == 'observe':
            # Optional owned-lab delay stalls this JS session once, not the host.
            delay = self.lane.pop('test_observe_delay_ms', 0)
            if delay:
                self.companion.js('await new Promise(resolve => setTimeout(resolve, '+str(int(delay))+'));')
            # Capture this window only; no unrelated screenshots or barrier.
            capture = self.companion.js('await nodeRepl.emitImage(await app.getScreenshot({emit:false}));')
            return dict(self.state(), capture=capture)
        if request['operation'] != 'act':
            raise RejectedRequest('unknown operation')
        if type(request.get('revision')) is not int or request['revision'] != state['revision']:
            raise RejectedRequest('stale revision; input not sent')
        if request.get('choice') not in state['buttons']:
            raise RejectedRequest('unknown choice; input not sent')
        # This lock is per window and held through validation and receipt. The
        # owned fixture changes only by its lane's input. Real adapters must
        # validate native capture/window generation at the input boundary too.
        self.companion.js('await app.click('+json.dumps(state['buttons'][request['choice']])+');')
        end = time.monotonic()+2
        while time.monotonic() < end:
            after = self.state()
            if after['revision'] != state['revision']:
                if after['revision'] != state['revision']+1 or after['completed'][-1]['choice'] != request['choice']:
                    raise RuntimeError('unexpected receipt')
                return dict(revision=after['revision'], choice=request['choice'])
            time.sleep(.01)
        raise TimeoutError('input receipt missing; do not replay')


def serve(config):
    if not 1 <= len(config['lanes']) <= 8:
        raise ValueError('one to eight lanes required')
    if len({v['pid'] for v in config['lanes'].values()}) != len(config['lanes']):
        raise ValueError('lanes must use distinct native clients')
    lanes = {k: Lane(config, value) for k, value in config['lanes'].items()}
    output_lock = threading.Lock()
    disconnected = threading.Event()
    def reply(value):
        with output_lock:
            if not disconnected.is_set():
                print(json.dumps(value), flush=True)
    def execute(request, lane):
        try:
            reply(dict(id=request['id'], result=lane.execute(request)))
        except Exception as exc:
            reply(dict(id=request['id'], error=str(exc) or type(exc).__name__))
        finally:
            lane.lock.release()
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=len(lanes))
    try:
        while line := sys.stdin.buffer.readline(16385):
            if len(line) > 16384:
                raise ValueError('request exceeds 16 KiB')
            request = json.loads(line)
            lane = lanes.get(str(request.get('lane')))
            if lane is None:
                reply(dict(id=request['id'], error='unknown lane'))
            elif not lane.lock.acquire(blocking=False):
                reply(dict(id=request['id'], error='lane busy; request not queued'))
            else:
                pool.submit(execute, request, lane)
    finally:
        disconnected.set()
        # Close first: initialization and RPCs may be waiting on a companion.
        closers = [threading.Thread(target=lane.close, daemon=True) for lane in lanes.values()]
        for closer in closers:
            closer.start()
        deadline = time.monotonic()+3
        for closer in closers:
            closer.join(max(0, deadline-time.monotonic()))
        pool.shutdown(wait=False, cancel_futures=True)


if __name__ == '__main__':
    config = json.loads(Path(sys.argv[1]).read_text())
    if len(sys.argv) == 4 and sys.argv[2] == '--listen':
        # A single owned-lab connection, normally reached by ssh -W. No HTTP
        # accept loop, shared request queue, remote execution or reconnect.
        import socket
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', int(sys.argv[3])))
            listener.listen(1)
            connection, _ = listener.accept()
        with connection:
            sys.stdin = connection.makefile('r')
            sys.stdout = connection.makefile('w')
            serve(config)
    else:
        serve(config)
