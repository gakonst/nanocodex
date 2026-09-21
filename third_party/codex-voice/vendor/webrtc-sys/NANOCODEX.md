# Local patch to webrtc-sys 0.3.45

This directory is the crates.io 0.3.45 source (LiveKit, Apache-2.0). The original
NOTICE.md and crate metadata are retained. The linked native archive remains
webrtc-89d790b; no WebRTC ABI or provider transport is replaced.

Changes:

- `src/nanocodex_pcm.cpp`, `include/livekit/nanocodex_pcm.h`: bounded mono PCM
  source/mixer decorator and private C ABI used only by webrtc-host.
- `src/peer_connection_factory.cpp`: consume the explicitly prepared thread-local
  source when creating that factory's `dependencies.audio_mixer`.
- `build.rs`: compile the added C++ translation unit.

The decorator forwards all WebRTC sources to AudioMixerImpl and adds PCM to its
48 kHz result. A silent 48 kHz source prevents the default mixer from selecting
8 kHz when all remote tracks are muted. The same AudioTransportImpl then feeds
reverse audio processing and the ADM. Only a factory immediately following
`nanocodex_pcm_create` on the same thread gets this mixer; unrelated factories
retain their original default. State lifetime is shared between the mixer and
opaque ingress handle, so factory teardown cannot leave a dangling callback.

The callback uses a fixed 9,600-sample queue and a try-lock (no waiting, copying
heap buffers or allocation in the added callback). Command writes hold a bounded
short lock; a contended callback emits the existing provider mix for that block.
Cancellation cannot retract a block already returned to AudioTransportImpl.
`nanocodex_pcm_test_render` exercises the actual factory-attached mixer without starting device streams.
Its test-only caller must retain the factory throughout the synchronous call.
