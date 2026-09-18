"""Equivalent tiny replies, fresh SSH per read vs one persistent SSH process."""
import asyncio
import json
import shlex
import statistics
import sys
import time
from pathlib import Path
from lanes import Transport

async def main():
    host, output = sys.argv[1:]
    echo = "import sys,json\nfor l in sys.stdin:\n r=json.loads(l);print(json.dumps({'id':r['id'],'result':{}}),flush=True)"
    command = ['ssh', '-o', 'BatchMode=yes', host, 'python3 -u -c '+shlex.quote(echo)]
    fresh = []
    for index in range(10):
        start = time.monotonic()
        process = await asyncio.create_subprocess_exec(*command, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE)
        stdout, _ = await process.communicate((json.dumps(dict(id=index))+'\n').encode())
        assert process.returncode == 0 and json.loads(stdout)['id']==index
        fresh.append((time.monotonic()-start)*1000)
    start = time.monotonic()
    transport = await Transport.open(command)
    await transport.call(0, 'observe')
    startup = (time.monotonic()-start)*1000
    warm = []
    try:
        for _ in range(10):
            start = time.monotonic()
            await transport.call(0, 'observe')
            warm.append((time.monotonic()-start)*1000)
    finally:
        await transport.close()
    result = dict(scope='tiny JSON echo only, not GUI or model latency', fresh_ms=fresh,
        persistent_ms=warm, persistent_startup_ms=startup,
        fresh_median_ms=statistics.median(fresh), persistent_median_ms=statistics.median(warm))
    Path(output).write_text(json.dumps(result, indent=2))
    print(json.dumps(result))

asyncio.run(main())
