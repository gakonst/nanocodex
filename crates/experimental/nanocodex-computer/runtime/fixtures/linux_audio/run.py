#!/usr/bin/env python3
"""Owned, network-isolated PulseAudio/Xvfb fixture; never use personal audio.

Run inside fixtures/linux_audio/Dockerfile as uid17072 with --network none.
The Rust binary uses no fixture pactl/paplay commands: these create the oracle.
"""
import argparse
import array
import base64
import json
import math
import os
from pathlib import Path
import selectors
import secrets
import shutil
import subprocess
import time
import wave


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("/tmp/skyre-linux-audio-integration"))
    args = parser.parse_args()
    assert os.getuid() == 17072 and Path("/.dockerenv").exists(), "owned container required"
    assert len(Path("/proc/net/route").read_text().splitlines()) == 1, "--network none required"
    assert args.output.parent == Path("/tmp") and args.output.name.startswith("skyre-linux-audio-")
    args.output.mkdir(mode=0o700)
    env = dict(os.environ, PULSE_SERVER=f"unix:{args.output}/pulse.sock", DISPLAY=":97", SKY_ENABLE_AUDIO="1")
    children = []
    transcript = []

    def spawn(command, **kw):
        child = subprocess.Popen(command, env=env, **kw)
        children.append(child)
        return child

    def wait_for(condition, seconds=5):
        until = time.monotonic() + seconds
        while not condition():
            assert time.monotonic() < until, "owned fixture readiness deadline"
            time.sleep(.02)

    def pactl(*arguments):
        return subprocess.check_output(["pactl", *arguments], env=env, timeout=3)

    def streams():
        return json.loads(pactl("--format=json", "list", "source-outputs"))

    try:
        pulse = spawn(["pulseaudio", "-n", "--daemonize=no", "--exit-idle-time=-1", "--log-level=warning",
                       f"--load=module-native-protocol-unix socket={args.output}/pulse.sock auth-anonymous=1",
                       "--load=module-null-sink sink_name=skyre_owned rate=48000 channels=2 channel_map=front-left,front-right",
                       "--load=module-null-source source_name=skyre_owned_silent_default"],
                      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=(args.output / "pulse.log").open("wb"))
        xvfb = spawn(["Xvfb", ":97", "-screen", "0", "640x480x24", "-nolisten", "tcp"],
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=(args.output / "xvfb.log").open("wb"))
        wait_for(lambda: (args.output / "pulse.sock").exists() and Path("/tmp/.X11-unix/X97").exists())
        assert pulse.poll() is None and xvfb.poll() is None
        pactl("set-default-sink", "skyre_owned")
        pactl("set-default-source", "skyre_owned_silent_default")
        source_info = json.loads(pactl("--format=json", "list", "sources"))
        monitor = next(source["index"] for source in source_info if source["name"] == "skyre_owned.monitor")
        (args.output / "sources.json").write_text(json.dumps(source_info, indent=2) + "\n")
        tone = array.array("h", (round(math.sin(2 * math.pi * hz * frame / 48000) * 16000)
                                for frame in range(48000 * 12) for hz in (440, 660)))
        with wave.open(str(args.output / "generated-stereo-tone.wav"), "wb") as wav:
            wav.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
            wav.writeframes(tone.tobytes())
        player = spawn(["paplay", "--device=skyre_owned", "--latency-msec=20", str(args.output / "generated-stereo-tone.wav")],
                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=(args.output / "paplay.log").open("wb"))
        # The null sink starts with an idle device latency. Prime the owned
        # render clock before testing actual capture/resampling duration.
        time.sleep(2.25)
        assert player.poll() is None and streams() == []
        token = secrets.token_hex(32)
        bindings = [{"browserId": route, "route": {"conversationId": route}} for route in ("owned-a", "owned-b")]
        # Empty IAB bindings select trusted kernel routes without opening a browser.
        iab = [{"id": route, "endpoint": f"ws://127.0.0.1:9/{route}",
                "route": {"conversationId": route, "windowId": route},
                "context": {"session_id": route, "turn_id": "owned-turn"}} for route in ("owned-a", "owned-b")]
        for filename, value in (("iab.json", iab), ("host.json", {"authorityToken": token,
                                  "stateDirectory": str(args.output / "host-state"), "bindings": bindings})):
            path = args.output / filename
            path.write_text(json.dumps(value))
            path.chmod(0o600)
        server = spawn([str(args.binary), "--iab-config", str(args.output / "iab.json"),
                        "--host-turns-config", str(args.output / "host.json"), "serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                       stderr=(args.output / "skyre.log").open("wb"), text=True, bufsize=1)
        selector = selectors.DefaultSelector()
        selector.register(server.stdout, selectors.EVENT_READ)
        sequence = 0

        def request(method, params):
            nonlocal sequence
            sequence += 1
            sent = {"jsonrpc": "2.0", "id": sequence, "method": method, "params": params}
            server.stdin.write(json.dumps(sent) + "\n")
            server.stdin.flush()
            deadline = time.monotonic() + 15
            while True:
                assert selector.select(max(0, deadline - time.monotonic())), "MCP response deadline"
                line = server.stdout.readline()
                assert line, f"Rust MCP exited {server.poll()}"
                response = json.loads(line)
                transcript.append({"request": sent, "response": response})
                if response.get("id") == sequence:
                    assert "error" not in response, response
                    assert not response.get("result", {}).get("isError"), response
                    return response["result"]

        def js(code):
            return request("tools/call", {"name": "js", "arguments": {"code": code, "timeout_ms": 10000}})

        def route(name, sequence):
            return request("host/turn", {"authorityToken": token, "event": {"eventId": name,
                           "sequence": sequence, "phase": "started", "route": {"conversationId": name},
                           "turnId": "owned-turn"}})

        request("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                               "clientInfo": {"name": "owned-linux-audio-fixture", "version": "1"}})
        route("owned-a", 1)
        js("if(cua.computer.target!=='linux')throw Error('wrong platform');"
           "if(await cua.computer.start_audio_recording({max_duration_ms:1500})!==undefined)throw Error('start must return void');"
           "nodeRepl.write('recording started');")
        attached = streams()
        assert len(attached) == 1 and attached[0]["source"] == monitor, attached
        (args.output / "active-stream.json").write_text(json.dumps(attached, indent=2) + "\n")
        result = js("await new Promise(resolve=>setTimeout(resolve,1800));"
                    "const ownedAudio=await cua.computer.stop_audio_recording();"
                    "if(Object.keys(ownedAudio).sort().join(',')!=='bytes,data_url,filepath')throw Error('audio shape');"
                    "if(!(ownedAudio.bytes instanceof Uint8Array))throw Error('audio bytes');"
                    "let consumed=false;try{await cua.computer.stop_audio_recording()}catch(e){consumed=e.message==='computer audio recording is not active'};"
                    "if(!consumed)throw Error('audio must be consumed once');"
                    "nodeRepl.write(JSON.stringify({fixture:'owned-linux-audio',filepath:ownedAudio.filepath,bytes:ownedAudio.bytes.length,data_url:ownedAudio.data_url}));")
        records = [json.loads(content["text"]) for content in result["content"] if content.get("type") == "text" and content.get("text", "").startswith("{")]
        record = next(item for item in records if item.get("fixture") == "owned-linux-audio")
        data = base64.b64decode(record["data_url"].removeprefix("data:audio/wav;base64,"), validate=True)
        path = Path(record["filepath"])
        assert path.read_bytes() == data and len(data) == record["bytes"]
        assert path.stat().st_mode & 0o777 == 0o600
        assert path.parent.stat().st_mode & 0o777 == 0o700
        shutil.copy2(path, args.output / "captured-stereo-tone.wav")
        with wave.open(str(args.output / "captured-stereo-tone.wav")) as wav:
            assert (wav.getnchannels(), wav.getsampwidth(), wav.getframerate()) == (2, 2, 24000)
            frames = wav.getnframes()
            assert 24000 <= frames <= 36000, frames
            samples = array.array("h", wav.readframes(frames))
        frequencies = []
        for channel, expected in ((0, 440), (1, 660)):
            middle = samples[frames // 3 * 2 + channel:frames * 2 // 3 * 2:2]
            frequency = sum(a <= 0 < b for a, b in zip(middle, middle[1:])) * 24000 / len(middle)
            assert abs(frequency - expected) < 4, (channel, frequency)
            assert max(abs(sample) for sample in middle) > 8000
            frequencies.append(frequency)
        assert streams() == []
        js("await cua.computer.start_audio_recording({max_duration_ms:5000});nodeRepl.write('parked capture started');")
        route("owned-b", 2)
        js("let denied=false;try{await cua.computer.start_audio_recording({max_duration_ms:100})}catch(e){denied=e.message==='Audio recording belongs to another kernel'};"
           "if(!denied)throw Error('cross-route audio must reject');nodeRepl.write('foreign route rejected');")
        assert len(streams()) == 1
        request("tools/call", {"name": "js_reset", "arguments": {}})
        assert len(streams()) == 1, "unrelated kernel reset stopped the parked owner's recording"
        route("owned-a", 1)
        js("await cua.computer.stop_audio_recording();nodeRepl.write('parked owner consumed');")
        assert streams() == []
        js("await cua.computer.start_audio_recording({max_duration_ms:5000});nodeRepl.write('reset capture started');")
        assert len(streams()) == 1
        request("tools/call", {"name": "js_reset", "arguments": {}})
        wait_for(lambda: streams() == [], 2)
        js("await cua.computer.start_audio_recording({max_duration_ms:5000});nodeRepl.write('disconnect capture started');")
        assert len(streams()) == 1
        server.stdin.close()
        server.wait(timeout=5)
        wait_for(lambda: streams() == [], 2)
        report = {"real_native_pulse": True, "integrated_public_mcp_sky": True, "configured_provider": False,
                  "scope": "isolated owned null sink", "silent_default_source": True, "frames": frames,
                  "sample_rate": 24000, "channels": 2, "sample_bits": 16, "frequencies": frequencies,
                  "wav_bytes": len(data), "consume_once": True, "owner_reset_cleanup": True,
                  "mcp_disconnect_cleanup": True, "private_media_modes": True,
                  "cross_route_start_rejected": True, "other_route_reset_preserves_owner": True,
                  "parked_owner_can_consume": True}
        (args.output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report))
    finally:
        for item in transcript:
            if "authorityToken" in item["request"].get("params", {}):
                item["request"]["params"]["authorityToken"] = "[REDACTED_OWNED_EPHEMERAL_TOKEN]"
        (args.output / "transcript.json").write_text(json.dumps(transcript, indent=2) + "\n")
        if (args.output / "host.json").exists():
            (args.output / "host.json").unlink()
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=3)


if __name__ == "__main__":
    main()
