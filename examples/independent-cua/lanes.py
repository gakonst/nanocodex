"""Bounded independent observe/decide/act lanes over one persistent JSONL transport."""
import asyncio
import fcntl
import json
import os
import time
import threading
from pathlib import Path


class RemoteError(RuntimeError):
    pass


class Transport:
    """One subprocess (optionally ssh), one reader, correlated out-of-order replies.

    Requests are never replayed, including after EOF or a timeout. A deadline
    cancels only the waiting caller; it does not claim to cancel remote input.
    """
    def __init__(self, process, timeout=30):
        self.process, self.timeout = process, timeout
        self.pending, self.seq = {}, 0
        self.writer = asyncio.Lock()
        self.reader = asyncio.create_task(self._read())

    @classmethod
    async def open(cls, command, timeout=30):
        process = await asyncio.create_subprocess_exec(
            *command, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            limit=16 * 1024 * 1024)
        return cls(process, timeout)

    async def _read(self):
        error = RemoteError('transport EOF; outstanding writes are uncertain')
        try:
            while line := await self.process.stdout.readline():
                reply = json.loads(line)
                future = self.pending.get(reply['id'])
                if future and not future.done():
                    if 'error' in reply:
                        future.set_exception(RemoteError(str(reply['error'])))
                    else:
                        future.set_result(reply['result'])
        except Exception as exc:
            error = exc
        finally:
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(error)

    async def call(self, lane, operation, **args):
        if self.reader.done():
            raise RemoteError('transport closed')
        self.seq += 1
        ident = self.seq
        future = asyncio.get_running_loop().create_future()
        self.pending[ident] = future
        try:
            async with asyncio.timeout(self.timeout):
                async with self.writer:
                    self.process.stdin.write((json.dumps(dict(id=ident, lane=lane,
                        operation=operation, **args)) + '\n').encode())
                    await self.process.stdin.drain()
                return await future
        finally:
            self.pending.pop(ident, None)

    async def close(self):
        self.process.stdin.close()
        try:
            await asyncio.wait_for(self.process.wait(), 5)
        except asyncio.TimeoutError:
            self.process.terminate()
            try:
                await asyncio.wait_for(self.process.wait(), 3)
            except asyncio.TimeoutError:
                self.process.kill()
                await self.process.wait()
        await self.reader


class Journal:
    """Fsync before each write. Refuse automatic resume after an uncertain write."""
    def __init__(self, path):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.unresolved = set()
        self.lock = threading.Lock()
        self.file = self.path.open('a+')
        try:
            fcntl.flock(self.file, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BaseException:
            self.file.close()
            raise
        self.file.seek(0)
        try:
            for line in self.file.read().splitlines():
                event = json.loads(line)
                if event['event'] == 'prepared':
                    self.unresolved.add(str(event['lane']))
                elif event['event'] == 'committed':
                    self.unresolved.discard(str(event['lane']))
        except BaseException:
            self.file.close()
            raise

    def append(self, event, lane, **fields):
        # Match host routing, including numeric IDs in journals from older runs.
        lane = str(lane)
        with self.lock:
            self.file.write(json.dumps(dict(event=event, lane=lane, at=time.time(), **fields))+'\n')
            self.file.flush()
            os.fsync(self.file.fileno())
            if event == 'prepared':
                self.unresolved.add(lane)
            elif event == 'committed':
                self.unresolved.discard(lane)

    async def record(self, event, lane, **fields):
        # A durable write must finish before input. Shield and join on cancellation
        # so close/reopen cannot race an abandoned journal worker.
        write = asyncio.create_task(asyncio.to_thread(self.append, event, lane, **fields))
        try:
            await asyncio.shield(write)
        except asyncio.CancelledError:
            await write
            raise

    def close(self):
        with self.lock:
            self.file.close()


async def run_lanes(transport, lanes, decide, journal, *, steps=10, decision_timeout=10):
    """One bounded task per window; no cross-window round or global JS session.

    decide(lane, observation) is async; blocking model SDKs must use to_thread.
    No write failure is retried. Restart requires reconciliation of journal
    entries without a committed receipt. Lanes represent distinct native clients.
    Host and journal identities use strings; callbacks retain the supplied IDs.
    """
    if not 1 <= len(lanes) <= 8 or len({str(lane) for lane in lanes}) != len(lanes):
        raise ValueError('one to eight distinct lanes required')
    if not 1 <= steps <= 10000:
        raise ValueError('steps must be bounded')

    async def lane_loop(lane):
        result = dict(lane=lane, completed=0, timings=[], status='running')
        try:
            if str(lane) in journal.unresolved:
                raise RemoteError('unreconciled prior write; manual observation required')
            for _ in range(steps):
                start = time.monotonic()
                observation = await transport.call(lane, 'observe')
                observed = time.monotonic()
                choice = await asyncio.wait_for(decide(lane, observation), decision_timeout)
                decided = time.monotonic()
                await journal.record('prepared', lane, revision=observation['revision'], choice=choice)
                receipt = await transport.call(lane, 'act', revision=observation['revision'], choice=choice)
                await journal.record('committed', lane, receipt=receipt)
                result['completed'] += 1
                result['timings'].append(dict(observe_ms=(observed-start)*1000,
                    decide_ms=(decided-observed)*1000, act_ms=(time.monotonic()-decided)*1000,
                    completed_at=time.monotonic()))
            result['status'] = 'complete'
        except Exception as exc:
            result.update(status='stopped', error=str(exc) or type(exc).__name__)
            await journal.record('stopped', lane, error=result['error'])
        return result

    return await asyncio.gather(*(lane_loop(lane) for lane in lanes))
