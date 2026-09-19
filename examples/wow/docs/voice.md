# Local speech input

Run `scripts/install-voice.sh` from the repository. It installs whisper.cpp v1.8.2 at pinned commit `4979e04f5dcaccb36057e059bbaed8a2f5288315` and the English `tiny.en` model under `~/.local/share/nanocodex-wow-voice`. No Python packages, API keys, root access, or subscription are needed. Prerequisites: Linux x86_64, C++ compiler, make, git, curl, tar, Python 3, and ffmpeg. If CMake is missing, a checksum-verified official CMake 3.31.6 binary is installed inside that tools directory. Other architectures should provide CMake.

The model SHA-256 is `921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f`, published as the Git LFS object hash in [the upstream Hugging Face tree](https://huggingface.co/api/models/ggerganov/whisper.cpp/tree/main?recursive=false). The installer verifies it before accepting the model. The runtime contains no network requests; recordings and transcripts use private temporary files removed after each request.

`voice.status()` returns availability, engine/model, `maxBytes`, and `maxSeconds`. `voice.transcribe(audio_bytes, mime_type)` returns English text. It accepts WebM, Ogg, MP4, WAV, and MP3 audio, including MediaRecorder MIME codec suffixes. Maximum upload is 12 MiB, maximum recording is 60 seconds. ffmpeg converts to mono 16 kHz PCM with a 25-second deadline; Whisper inference has a 120-second deadline and four CPU threads. Only one inference runs at a time. Invalid input raises `ValueError`; missing tools, busy service, and inference errors raise `RuntimeError`. The HTTP integration should bound request bodies before reading and return `{ "text": "..." }` on success.

Environment overrides: `NANOCODEX_WOW_VOICE_HOME`, `NANOCODEX_WOW_WHISPER` (executable), `NANOCODEX_WOW_WHISPER_MODEL`, and `NANOCODEX_WOW_FFMPEG`. Keep these trusted server configuration, not client-controlled request fields. Set the same tools directory when installing and launching the companion. Installed files are readable/executable by the desktop user.

This provides actual local speech-to-text. It does not implement Nanocodex managed realtime voice or local text-to-speech. The small English model may misrecognize Warcraft names; review the transcript before sending. Speech output, if exposed through browser speech synthesis, depends on available browser/system voices.

## Verification on Omarchy

The installed CPU runtime was tested against whisper.cpp's bundled `samples/jfk.wav` and the same fixture encoded with ffmpeg to Opus WebM. Both returned: “And so my fellow Americans ask not what your country can do for you, ask what you can do for your country.” Both transcriptions plus invalid-input tests completed in 1.83 seconds total. Empty audio, unsupported MIME, and corrupt WebM were rejected. Python compilation passed. This verifies recorded-file transcription; physical microphone capture and browser permission prompts still require an end-to-end UI check.
