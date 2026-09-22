#!/usr/bin/env python3
"""Decode a product publisher's RTMP output against a private loopback receiver.

The command after -- must run the actual publisher, read NANOCODEX_RTMP_TEST_URL,
and stop itself. No platform account or real stream key is used.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import ssl
import threading
import time


def run(args):
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--timeout', type=int, default=180)
    parser.add_argument('--min-duration', type=float, default=5)
    parser.add_argument('--min-fps', type=float, default=25)
    parser.add_argument('--width', type=int)
    parser.add_argument('--height', type=int)
    parser.add_argument('--require-audio', action='store_true')
    parser.add_argument('--max-gop', type=float)
    parser.add_argument('--tls', action='store_true', help='RTMPS with an ephemeral test CA trusted only by the publisher process')
    parser.add_argument('--outage-after', type=float, help='seconds after first media bytes to interrupt ingest')
    parser.add_argument('--outage-duration', type=float, default=2)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        parser.error('a product publisher command is required after --')
    for binary in ['ffmpeg', 'ffprobe']:
        if not shutil.which(binary):
            parser.error(f'{binary} is required')
    args.output.mkdir(parents=True, exist_ok=True)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        port = reservation.getsockname()[1]
    endpoint = f'rtmp://127.0.0.1:{port}/live/nanocodex-test'
    publish_endpoint = endpoint
    tls_listener = None
    tls_peers = []
    if args.tls:
        cert, key = args.output.resolve() / 'test-cert.pem', args.output.resolve() / 'test-key.pem'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
            '-subj', '/CN=127.0.0.1', '-addext', 'subjectAltName=IP:127.0.0.1',
            '-keyout', str(key), '-out', str(cert)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        key.chmod(0o600)
        tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls_context.load_cert_chain(cert, key)
        tls_listener = socket.socket()
        tls_listener.bind(('127.0.0.1', 0)); tls_listener.listen()
        publish_endpoint = f'rtmps://127.0.0.1:{tls_listener.getsockname()[1]}/live/nanocodex-test'
        def pump(source, sink):
            try:
                while data := source.recv(65536):
                    sink.sendall(data)
            except (OSError, ssl.SSLError):
                pass
            finally:
                for peer in (source, sink):
                    try: peer.shutdown(socket.SHUT_RDWR)
                    except OSError: pass
        def accept_tls():
            while True:
                try:
                    connection, _ = tls_listener.accept()
                    secured = tls_context.wrap_socket(connection, server_side=True)
                    upstream = socket.create_connection(('127.0.0.1', port))
                    tls_peers.extend([secured, upstream])
                    threading.Thread(target=pump, args=(secured, upstream), daemon=True).start()
                    threading.Thread(target=pump, args=(upstream, secured), daemon=True).start()
                except ssl.SSLError:
                    connection.close()
                except OSError:
                    return
        threading.Thread(target=accept_tls, daemon=True).start()
    recording = args.output / 'received.flv'
    env = dict(os.environ, NANOCODEX_RTMP_TEST_URL=publish_endpoint)
    if args.tls:
        env['SSL_CERT_FILE'] = str(cert)
    started = time.monotonic()
    recovery_test = args.outage_after is not None
    if recovery_test:
        env['NANOCODEX_RTMP_TEST_SECONDS'] = '25'
    else:
        env.setdefault('NANOCODEX_RTMP_TEST_SECONDS', '10')
    with (args.output / 'receiver.log').open('w') as log:
        receiver = subprocess.Popen(['ffmpeg', '-hide_banner', '-loglevel', 'warning',
            '-listen', '1', '-timeout', '30', '-i', endpoint, '-map', '0', '-c', 'copy',
            '-y', str(recording)], stdout=subprocess.DEVNULL, stderr=log)
        publisher = None
        try:
            # A TCP readiness probe would consume FFmpeg's single RTMP accept.
            time.sleep(0.5)
            if receiver.poll() is not None:
                raise RuntimeError('RTMP receiver failed before publisher start')
            with (args.output / 'publisher.log').open('w') as output:
                publisher = subprocess.Popen(command, env=env, stdout=output, stderr=subprocess.STDOUT)
                try:
                    if recovery_test:
                        deadline = time.monotonic() + args.timeout
                        while not recording.exists() or recording.stat().st_size < 4096:
                            if publisher.poll() is not None or receiver.poll() is not None or time.monotonic() > deadline:
                                raise RuntimeError('no media before planned outage')
                            time.sleep(0.1)
                        time.sleep(args.outage_after)
                        receiver.kill(); receiver.wait()
                        recording.rename(args.output / 'before-outage.flv')
                        time.sleep(args.outage_duration)
                        receiver = subprocess.Popen(['ffmpeg', '-hide_banner', '-loglevel', 'warning',
                            '-listen', '1', '-timeout', '30', '-i', endpoint, '-map', '0', '-c', 'copy',
                            '-y', str(recording)], stdout=subprocess.DEVNULL, stderr=log)
                    code = publisher.wait(timeout=args.timeout)
                except subprocess.TimeoutExpired:
                    raise RuntimeError('publisher failed to finish within timeout')
                if code:
                    raise RuntimeError(f'publisher exited {code}; see publisher.log')
            try:
                receiver.wait(timeout=10)
            except subprocess.TimeoutExpired:
                receiver.terminate()
                receiver.wait(timeout=5)
        finally:
            for process in [publisher, receiver]:
                if process is not None and process.poll() is None:
                    process.kill()
                    process.wait()
    if tls_listener:
        tls_listener.close()
        for peer in tls_peers: peer.close()
        key.unlink(missing_ok=True)
    probe = json.loads(run(['ffprobe', '-v', 'error', '-show_streams', '-show_format',
                           '-show_packets', '-of', 'json', str(recording)]))
    streams = probe['streams']
    video = next((s for s in streams if s['codec_type'] == 'video'), None)
    audio = next((s for s in streams if s['codec_type'] == 'audio'), None)
    failures = []
    if not video or video['codec_name'] != 'h264':
        failures.append('missing H.264 video')
    if args.require_audio and (not audio or audio['codec_name'] != 'aac'):
        failures.append('missing AAC audio')
    metrics = []
    for stream in streams:
        packets = [p for p in probe['packets'] if p['stream_index'] == stream['index']]
        timestamps = [float(p['dts_time']) for p in packets if 'dts_time' in p]
        if not timestamps:
            failures.append(f'no timestamps for stream {stream["index"]}')
            continue
        duration = timestamps[-1] - timestamps[0]
        if duration < args.min_duration:
            failures.append(f'{stream["codec_type"]} duration {duration:.3f}s below minimum')
        if any(b < a for a, b in zip(timestamps, timestamps[1:])):
            failures.append(f'non-monotonic {stream["codec_type"]} DTS')
        fps = (len(timestamps) - 1) / duration if duration else 0
        item = {'type': stream['codec_type'], 'codec': stream['codec_name'],
                'packets': len(packets), 'start_seconds': timestamps[0],
                'end_seconds': timestamps[-1], 'duration_seconds': duration}
        if stream['codec_type'] == 'video':
            item.update(width=stream['width'], height=stream['height'], measured_fps=fps)
            keyframes = [float(p['dts_time']) for p in packets if 'K' in p.get('flags', '') and 'dts_time' in p]
            max_gop = max((b - a for a, b in zip(keyframes, keyframes[1:])), default=0)
            item['maximum_keyframe_interval_seconds'] = max_gop
            if args.max_gop is not None and (len(keyframes) < 2 or max_gop > args.max_gop + 0.05):
                failures.append(f'keyframe interval {max_gop:.3f}s exceeds requirement or insufficient keyframes')
            if fps < args.min_fps:
                failures.append(f'measured fps {fps:.3f} below minimum')
            for key in ['width', 'height']:
                expected = getattr(args, key)
                if expected and stream[key] != expected:
                    failures.append(f'{key}: expected {expected}, received {stream[key]}')
        if stream['codec_type'] == 'audio':
            item.update(sample_rate=int(stream['sample_rate']), channels=stream['channels'])
        metrics.append(item)
    if video and audio:
        vm = next(m for m in metrics if m['type'] == 'video')
        am = next(m for m in metrics if m['type'] == 'audio')
        # End skew measures drift as well as premature loss of one track.
        skew = abs(vm['end_seconds'] - am['end_seconds'])
        if skew > 0.5:
            failures.append(f'audio/video end skew {skew:.3f}s exceeds 500ms')
    else:
        skew = None
    decode = subprocess.run(['ffmpeg', '-v', 'error', '-xerror', '-i', str(recording),
                             '-map', '0', '-f', 'null', '-'], capture_output=True, text=True)
    if decode.returncode:
        failures.append('received media failed full decode')
    (args.output / 'decode.log').write_text(decode.stderr)
    report = {'passed': not failures, 'transport': 'rtmps' if args.tls else 'rtmp', 'recovery_test': recovery_test, 'wall_seconds': time.monotonic() - started,
              'streams': metrics, 'av_end_skew_seconds': skew, 'failures': failures,
              'validation': 'supplied publisher command -> loopback RTMP -> recorded FLV -> ffprobe + full decode'}
    (args.output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return 1 if failures else 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
