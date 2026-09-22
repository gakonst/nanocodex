# Terminal voice controls

The managed `nanocodex2` TUI supports ChatGPT and ElevenLabs speech. Run it from
the directory containing your `.env` (or a child directory); startup loads that
file automatically. Set `ELEVENLABS_API_KEY` there or in your environment. Keep
`.env` untracked. Never paste a key into the composer.

- `/voice` opens the voice menu with start/stop, provider voices, and recording a clone.
- `/voice on` and `/voice off` explicitly start and stop voice.
- `/voice voices` opens the provider menu.
- `/voice voices chatgpt` or `/voice voices elevenlabs` opens a selectable provider catalog.
- `/voice chatgpt cove` selects ChatGPT; `/voice cove` remains supported.
- `/voice elevenlabs VOICE_ID` selects an ElevenLabs catalog or cloned voice.
- `/voice mute`, `/voice unmute`, and Ctrl-X control the microphone.
- `/voice status` shows current status; `/voice help` displays command help.

Selecting a voice starts voice mode. While active, switching stops the old
session and waits for cleanup before reconnecting with the new voice, preserving
microphone mute. The selected voice is remembered when voice is stopped and
started again in the same TUI process. Catalog and clone operations run in the
background and leave the composer responsive. Catalogs, help, and clone results
open a persistent local panel: use arrows, Page Up/Down, or the mouse wheel to
scroll, `c` to copy its text, and Esc to return to your draft.

To record a new sample in place, enter `/voice clone "My voice"`. The recording
modal uses **R** to start, **S** or **Space** to stop, **P** to listen locally,
**U** to upload with the displayed ownership/permission consent, and **Esc** to
cancel and delete. Realtime voice stops and completes cleanup before capture.
Recording is capped at two minutes; audio stays local until you explicitly upload.
See [recording and cloning](tui-cloning.md) for prerequisites and details.

To create an instant clone from an existing local audio sample:

```text
/voice clone "My voice" "recordings/my sample.wav" --consent
```

`--consent` confirms that you own the voice or have permission to clone it.
Quote names and paths containing spaces. Relative paths resolve against the TUI
workspace; `~/` expands to your home directory without shell execution. The result provides the new voice ID and the command to select it;
cloning does not automatically change the active voice. If ElevenLabs requires
verification, complete it there before selecting the clone. Audio is uploaded
directly to ElevenLabs using your local key. The sample, key, and command do not
enter the chat/model prompt. Voice transcription still follows the normal voice
conversation flow.

On macOS, ElevenLabs output streams PCM into the same libWebRTC mixer,
speaker device, and echo-cancellation reference used by ChatGPT voice. The
microphone stays available for interruption unless you explicitly mute it.
Both providers use the native microphone and speaker meters. ElevenLabs speech
output no longer requires ffplay. Each complete assistant reply uses one synthesis
request, then streams its PCM audio. Waiting for the final caption avoids per-sentence pauses, delivery resets,
and fragment queue overflow; it can delay the first spoken word. Repeated and
interrupted captions remain fenced. Native playback buffers up to 60 ms before
starting and smooths audio boundaries to reduce gaps and clicks from uneven delivery.
Typing, stopping voice, or switching voices
cancels pending synthesis and native PCM. Captions from a stopped session are
discarded. This requires the matching native helper shipped with the build.

Voice-clone sample recording and local sample preview remain separate from
conversation audio: macOS recording uses the bundled native recorder and sample
preview uses afplay. Linux recording uses ffmpeg. The clone workflow never uploads without explicit consent.
