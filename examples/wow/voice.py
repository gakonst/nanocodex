"""Local, bounded Whisper CPU transcription. No network or account credentials."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import wave

MAX_AUDIO_BYTES = 12 * 1024 * 1024
MAX_SECONDS = 60
_LOCK = threading.Lock()
_MIMES = {'audio/webm', 'video/webm', 'audio/ogg', 'audio/mp4', 'audio/wav', 'audio/x-wav', 'audio/mpeg'}


def _paths():
    root = Path(os.environ.get('NANOCODEX_WOW_VOICE_HOME', str(Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share')) / 'nanocodex-wow-voice')))
    return (os.environ.get('NANOCODEX_WOW_WHISPER', str(root / 'whisper.cpp/build/bin/whisper-cli')),
            os.environ.get('NANOCODEX_WOW_WHISPER_MODEL', str(root / 'ggml-tiny.en.bin')),
            os.environ.get('NANOCODEX_WOW_FFMPEG', shutil.which('ffmpeg') or ''))


def status():
    binary, model, ffmpeg = _paths()
    available = bool(ffmpeg and os.access(ffmpeg, os.X_OK) and os.access(binary, os.X_OK) and os.path.isfile(model))
    return {'available': available, 'engine': 'whisper.cpp', 'model': Path(model).name,
            'local': True, 'maxSeconds': MAX_SECONDS, 'maxBytes': MAX_AUDIO_BYTES,
            'message': 'Local speech ready' if available else 'Run scripts/install-voice.sh to install local speech'}


def transcribe(audio: bytes, mime: str) -> str:
    if not isinstance(audio, bytes) or not audio or len(audio) > MAX_AUDIO_BYTES:
        raise ValueError('Audio must contain 1 byte to 12 MiB')
    if not isinstance(mime, str) or mime.split(';', 1)[0].strip().lower() not in _MIMES:
        raise ValueError('Unsupported audio format')
    if not status()['available']:
        raise RuntimeError('Local speech is unavailable; run scripts/install-voice.sh')
    if not _LOCK.acquire(blocking=False):
        raise RuntimeError('Another transcription is running; try again shortly')
    binary, model, ffmpeg = _paths()
    try:
        with tempfile.TemporaryDirectory(prefix='nanocodex-wow-voice-') as directory:
            source = Path(directory) / 'recording'
            wav = Path(directory) / 'audio.wav'
            output = Path(directory) / 'transcript'
            source.write_bytes(audio)
            # Decode at most 61 seconds so oversized recordings are rejected, not silently truncated.
            converted = subprocess.run([ffmpeg, '-nostdin', '-hide_banner', '-loglevel', 'error',
                '-protocol_whitelist', 'file,pipe', '-format_whitelist', 'matroska,webm,ogg,mov,wav,mp3', '-i', str(source), '-t', str(MAX_SECONDS + 1),
                '-vn', '-ac', '1', '-ar', '16000', '-c:a', 'pcm_s16le', '-y', str(wav)],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=25)
            if converted.returncode:
                raise ValueError('Audio could not be decoded')
            with wave.open(str(wav), 'rb') as stream:
                duration = stream.getnframes() / stream.getframerate()
            if duration > MAX_SECONDS:
                raise ValueError('Recording exceeds the 60 second limit')
            if duration < 0.1:
                raise ValueError('Recording is too short')
            result = subprocess.run([binary, '-m', model, '-f', str(wav), '-l', 'en', '-t', '4',
                '-otxt', '-of', str(output), '-nt', '-np'], stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120)
            transcript = output.with_suffix('.txt')
            if result.returncode or not transcript.is_file():
                raise RuntimeError('Local transcription failed')
            if transcript.stat().st_size > 65536:
                raise RuntimeError('Transcript exceeded the output limit')
            return transcript.read_text(encoding='utf-8').strip()
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError('Local transcription timed out') from exc
    finally:
        _LOCK.release()
