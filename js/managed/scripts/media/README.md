# FFmpeg in the durable brain

Managed Just Bash registers `ffmpeg` and `ffprobe` when its Worker Loader is available.
A command copies one local input from the authorized workspace into a fresh Worker
isolate, executes real single-threaded FFmpeg WebAssembly, and writes a successful
output back through the same workspace. No sandbox Hand, native process, network
access, account credential, or full workspace mount is passed to the media isolate.

```sh
ffprobe -v error -show_entries format=duration:stream=codec_type,codec_name,width,height -of json /brain/clip.mov
ffmpeg -hide_banner -loglevel error -i /brain/clip.mov -vf 'fps=1/2,scale=640:-1,tile=3x2' -frames:v 1 /brain/sheet.jpg
ffmpeg -hide_banner -loglevel error -i /brain/clip.mov -vn -ac 1 -ar 16000 /brain/audio.wav
```

The wrapper supports one input and, for conversion, one output. Use `--help` for the
supported options. It refuses URLs, pipes, playlists, filter scripts, image sequences,
and access outside the workspace. Cloudflare enforces runtime memory and CPU limits;
there are no additional media-specific file, diagnostic, argument, filter-count,
or CPU caps. WASM memory grows as needed using the toolchain default maximum,
which exceeds the Worker memory limit. Operations outside the shipped codec/filter
set still require a native Hand. Native Hands retain their normal FFmpeg executable.

The core decodes H.264/HEVC/MPEG-4/MJPEG video and AAC/MP3/PCM/FLAC/Vorbis/Opus audio;
it writes JPEG and PCM WAV. See `build.sh` for the exact demuxer, encoder, and filter
configuration. This is media inspection, not a general-purpose transcoding build.

## Rebuilding

The generated core assets are checked in so ordinary installs, tests, and deployments
do not download a compiler or build FFmpeg. They are LGPL-2.1-or-later, independently
of the TypeScript wrapper's license. `src/media/generated/COPYING.LGPLv2.1` contains
the license; `build.sh` contains the pinned source URL/hash, flags, and toolchain.

Activate Emscripten **3.1.74**, then run:

```sh
EMSDK=/path/to/emsdk bash js/managed/scripts/media/build.sh
pnpm --filter nanocodex-managed-service run test:media
```

FFmpeg **5.1.10** is the maintained 5.1 release (2026-06-21). Newer CLI branches
require pthreads even for a single stream; this build uses the maintained CLI that
can run without SharedArrayBuffer/worker threads in workerd. It does not enable GPL,
nonfree components, native assembly, or network protocols. Update the pinned source
hash and regenerate/retest both cores when updating the release.

The fixture is synthetic H.264/AAC color bars and a sine wave, generated with:

```sh
ffmpeg -f lavfi -i 'testsrc2=size=160x120:rate=4:duration=2' -f lavfi -i 'sine=frequency=440:sample_rate=16000:duration=2' -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest -movflags +faststart color-bars.mov
```

`runtime.test.mjs` runs the actual generated cores and managed executor through
Miniflare/workerd and the Worker Loader, including metadata, JPEG contact sheets,
WAV output, malformed media, and isolation between invocations. No native FFmpeg is
needed to run these tests.
