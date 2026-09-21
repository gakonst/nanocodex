// PCM enters before AudioTransportImpl's reverse APM processing and the ADM.
#pragma once
#include "api/audio/audio_mixer.h"
#include "api/scoped_refptr.h"
webrtc::scoped_refptr<webrtc::AudioMixer> nanocodex_take_pcm_mixer();
