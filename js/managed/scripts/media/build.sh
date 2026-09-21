#!/usr/bin/env bash
# Rebuild the LGPL FFmpeg cores; no native executable is used by deployed commands.
set -euo pipefail
: "${EMSDK:?Set EMSDK to an activated Emscripten 3.1.74 SDK}"
source "$EMSDK/emsdk_env.sh"
case "$(emcc --version | head -n 1)" in *3.1.74*) ;; *) echo 'Emscripten 3.1.74 is required' >&2; exit 1;; esac
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BUILD="${MEDIA_BUILD_DIR:-$ROOT/.media-build}"
mkdir -p "$BUILD" "$ROOT/src/media/generated"
ARCHIVE="$BUILD/ffmpeg-5.1.10.tar.xz"
if [[ ! -f "$ARCHIVE" ]]; then curl --fail --location --max-time 120 https://ffmpeg.org/releases/ffmpeg-5.1.10.tar.xz --output "$ARCHIVE"; fi
# The release archive is verified before any source is executed.
python3 - "$ARCHIVE" <<'PY'
import hashlib,sys
expected='392306d6fc45dab0e9e0ea55381e071842e83a2fb31d320aeda40477a7766293'
assert hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest() == expected, 'FFmpeg source checksum mismatch'
PY
if [[ ! -d "$BUILD/ffmpeg-5.1.10" ]]; then tar -xf "$ARCHIVE" -C "$BUILD"; fi
cd "$BUILD/ffmpeg-5.1.10"
CFLAGS='-Oz'
LDFLAGS='-O0 -sMODULARIZE=1 -sEXPORT_ES6=1 -sENVIRONMENT=web,worker -sDYNAMIC_EXECUTION=0 -sALLOW_MEMORY_GROWTH=1 -sINITIAL_MEMORY=16777216 -sMAXIMUM_MEMORY=100663296 -sSTACK_SIZE=1048576 -sEXIT_RUNTIME=1 -sINVOKE_RUN=0 -sEXPORTED_RUNTIME_METHODS=FS,callMain -sFORCE_FILESYSTEM=1'
emconfigure ./configure \
 --target-os=none --arch=wasm32 --enable-cross-compile --disable-asm \
 --disable-stripping --disable-doc --disable-debug --disable-autodetect \
 --disable-runtime-cpudetect --disable-pthreads --disable-w32threads --disable-os2threads \
 --disable-network --disable-everything --disable-programs --enable-ffmpeg --enable-ffprobe \
 --enable-small --disable-avdevice --disable-postproc --disable-iconv \
 --enable-protocol=file,pipe \
 --enable-demuxer=mov,matroska,wav,mp3,aac,ogg,flac,image2 \
 --enable-muxer=image2,wav,null \
 --enable-decoder=h264,hevc,mpeg4,mjpeg,aac,mp3,pcm_s16le,pcm_s24le,pcm_f32le,flac,vorbis,opus \
 --enable-encoder=mjpeg,pcm_s16le,wrapped_avframe \
 --enable-parser=h264,hevc,mpeg4video,mjpeg,aac,mpegaudio,flac,opus,vorbis \
 --enable-filter=fps,scale,tile,select,thumbnail,format,transpose,hflip,vflip,crop,aresample,aformat,anull,null \
 --cc=emcc --cxx=em++ --ar=emar --nm=emnm --ranlib=emranlib \
 --extra-cflags="$CFLAGS" --extra-ldflags="$LDFLAGS"
# Avoid running wasm-opt for configure's hundreds of tiny link probes.
python3 - <<'PYOPT'
p='ffbuild/config.mak'
s=open(p).read().replace('-O0','-Oz')
open(p,'w').write(s)
PYOPT
emmake make -j "${MEDIA_BUILD_JOBS:-4}" EXESUF=.mjs ffmpeg.mjs ffprobe.mjs
for program in ffmpeg ffprobe; do
 python3 - "$program.mjs" "$ROOT/src/media/generated/$program.js.txt" <<'PYGLUE'
import pathlib,sys
source=pathlib.Path(sys.argv[1]).read_text()
# workerd has WorkerGlobalScope but no self.location. instantiateWasm supplies
# the compiled module; no URL-based WASM loading is permitted or needed.
needle='scriptDirectory=self.location.href'
assert source.count(needle)==1, 'Emscripten loader changed; review worker adaptation'
pathlib.Path(sys.argv[2]).write_text(source.replace(needle,'scriptDirectory=""'))
PYGLUE
 cp "${program}_g.wasm" "$ROOT/src/media/generated/$program.wasm.bin"
 chmod 644 "$ROOT/src/media/generated/$program.wasm.bin"
done
cp COPYING.LGPLv2.1 "$ROOT/src/media/generated/COPYING.LGPLv2.1"
