"""Deterministic transport/scheduling acceptance, not model quality or latency."""
import argparse
import asyncio
import json
import time
from pathlib import Path
from lanes import Transport, Journal, run_lanes

async def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--journal', type=Path, required=True)
    parser.add_argument('--native-stall', action='store_true', help='assert configured lane-5 first observation stall')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    transport = await Transport.open(args.command, timeout=20)
    journal = Journal(args.journal)
    start = time.monotonic()
    async def decide(lane, observation):
        if lane == 6 and observation['revision'] >= 1:
            raise RuntimeError('injected decision failure')
        await asyncio.sleep(2 if lane == 7 else .02)
        return 'advance'
    try:
        results = await run_lanes(transport, list(range(8)), decide, journal, steps=5)
        args.output.write_text(json.dumps(dict(results=results), indent=2))
        boundary = {}
        try:
            await transport.call(0, 'act', revision=0, choice='advance')
        except Exception as exc:
            boundary['stale_rejected'] = str(exc)
        after = await transport.call(0, 'observe')
        boundary['revision_after_stale'] = after['revision']
        evidence = dict(scope='deterministic scheduler/transport; no model', started_at=start, elapsed_s=time.monotonic()-start,
            transport_processes=1, results=results, boundary=boundary)
        args.output.write_text(json.dumps(evidence, indent=2))
        fast = results[:5]
        assert all(r['completed']==5 for r in fast), evidence
        assert results[6]['completed']==1 and results[6]['status']=='stopped', evidence
        assert results[5]['completed']==5 and results[7]['completed']==5, evidence
        assert max(r['timings'][-1]['completed_at'] for r in fast) < results[7]['timings'][-1]['completed_at'], evidence
        if args.native_stall:
            assert max(r['timings'][-1]['completed_at'] for r in fast) < results[5]['timings'][0]['completed_at'], evidence
        assert max(r['timings'][-1]['completed_at'] for r in fast) < results[7]['timings'][1]['completed_at'], evidence
        assert boundary['revision_after_stale']==5 and boundary.get('stale_rejected'), evidence
        print(json.dumps(dict(elapsed_s=evidence['elapsed_s'], completed=[r['completed'] for r in results], boundary=boundary)))
    finally:
        journal.close()
        await transport.close()

if __name__ == '__main__':
    asyncio.run(main())
