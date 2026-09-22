// Nanocodex external PCM ingress. Apache-2.0.
#include "livekit/nanocodex_pcm.h"
#include "modules/audio_mixer/audio_mixer_impl.h"
#include "rtc_base/ref_counted_object.h"
#include <array>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <memory>
#include <mutex>

namespace {
constexpr size_t kCapacity = 9600, kBlock = 480, kRamp = 240;
int64_t now_ns() {
  return std::chrono::duration_cast<std::chrono::nanoseconds>(
      std::chrono::steady_clock::now().time_since_epoch()).count();
}
// One serialized producer/control side and one audio callback consumer.
// Release/acquire positions own slots; controls never reset either position.
// Old generations occupy capacity until the next callback discards them.
struct State {
  struct Sample { int16_t value; uint64_t generation; };
  std::mutex producer_mutex; // Never acquired by the audio callback.
  std::array<Sample, kCapacity> samples{};
  std::atomic<uint64_t> read{0}, completed{0}, written{0}, active_generation{0};
  std::atomic<uint64_t> finish_generation{0}, audible_end{0};
  uint64_t generation = 0, producer_audible_end = 0; // Producer only.
  std::atomic<uint16_t> peak{0};
  std::atomic<webrtc::AudioMixer*> attached_mixer{nullptr};
  std::atomic<int64_t> rendered_until{0};
};
static_assert(std::atomic<uint64_t>::is_always_lock_free);
static_assert(std::atomic<uint16_t>::is_always_lock_free);
thread_local std::shared_ptr<State> pending;
thread_local int* completion_probe = nullptr; // Synchronous test-render thread only.
int status(const State& state) {
  // Acquire completion before the deadline it publishes. Ring capacity is freed
  // earlier, but drain must not report completion while a callback owns a block.
  const auto completed = state.completed.load(std::memory_order_acquire);
  return completed == state.written.load(std::memory_order_acquire) &&
      now_ns() >= state.rendered_until.load(std::memory_order_acquire) ? 0 : 1;
}
class Mixer : public webrtc::AudioMixer, public webrtc::AudioMixer::Source {
 public:
  explicit Mixer(std::shared_ptr<State> state)
      : state_(std::move(state)), mixer_(webrtc::AudioMixerImpl::Create()) {
    mixer_->AddSource(this);
    state_->attached_mixer.store(this);
  }
  ~Mixer() override {
    mixer_->RemoveSource(this);
    state_->attached_mixer.store(nullptr);
  }
  int Ssrc() const override { return -1; }
  int PreferredSampleRate() const override { return 48000; }
  AudioFrameInfo GetAudioFrameWithInfo(int rate, webrtc::AudioFrame* frame) override {
    frame->UpdateFrame(0, nullptr, rate / 100, rate, webrtc::AudioFrame::kNormalSpeech, webrtc::AudioFrame::kVadUnknown, 1);
    return AudioFrameInfo::kMuted;
  }
  bool AddSource(webrtc::AudioMixer::Source* source) override { return mixer_->AddSource(source); }
  void RemoveSource(webrtc::AudioMixer::Source* source) override { mixer_->RemoveSource(source); }
  void Mix(size_t channels, webrtc::AudioFrame* frame) override {
    mixer_->Mix(channels, frame);
    auto read = state_->read.load(std::memory_order_relaxed);
    const auto written = state_->written.load(std::memory_order_acquire);
    // Read control AFTER written: seeing a published slot also observes its
    // preceding begin. Never discard a newer generation using an older snapshot.
    const uint64_t generation = state_->active_generation.load(std::memory_order_acquire);
    // At most 200ms of stale samples; the producer cannot overwrite these slots
    // until the release below. This also works when cancel is followed by begin.
    while (read < written && state_->samples[read % kCapacity].generation != generation) ++read;
    state_->read.store(read, std::memory_order_release);
    state_->completed.store(read, std::memory_order_release);
    if (generation != render_generation_) {
      render_generation_ = generation;
      playing_ = false;
      waiting_blocks_ = attack_samples_ = 0;
    }
    if (!generation || read == written) return;
    // Only consume this generation, even if begin raced the initial snapshot.
    size_t queued = 0;
    while (read + queued < written && state_->samples[(read + queued) % kCapacity].generation == generation) ++queued;
    const bool finished = state_->finish_generation.load(std::memory_order_acquire) == generation;
    if (!playing_) {
      if (!finished && queued < 2880 && waiting_blocks_++ < 6) return;
      playing_ = true;
      waiting_blocks_ = attack_samples_ = 0;
    }
    const size_t available = std::min(queued, kBlock);
    // A producer may publish EOF metadata before its final written position.
    // Never taper toward samples outside this callback's acquired snapshot.
    const auto audible_end = finished ? std::min(read + queued, state_->audible_end.load(std::memory_order_relaxed)) : read + queued;
    std::array<int16_t, kBlock> block{};
    uint16_t peak = 0;
    for (size_t i = 0; i < available; ++i) {
      const int value = state_->samples[(read + i) % kCapacity].value;
      const size_t attack = std::min(kRamp, attack_samples_ + i);
      const size_t release = read + i < audible_end ? std::min(uint64_t(kRamp), audible_end - read - i - 1) : 0;
      block[i] = static_cast<int16_t>(value * int(std::min(attack, release)) / int(kRamp));
      peak = std::max(peak, static_cast<uint16_t>(std::abs(int(block[i]))));
    }
    state_->read.store(read + available, std::memory_order_release);
    if (completion_probe) *completion_probe = status(*state_);
    attack_samples_ = std::min(kRamp, attack_samples_ + available);
    if (available == queued) playing_ = false;
    // Cancel/begin fences queued samples. As with the ADM itself, a device block
    // already submitted/in flight cannot be retracted (at most this 10ms block).
    if (state_->active_generation.load(std::memory_order_acquire) != generation) return;
    const size_t frames = frame->samples_per_channel();
    auto* data = frame->mutable_data();
    for (size_t i = 0; i < frames; ++i) {
      const int value = block[i * kBlock / frames];
      for (size_t c = 0; c < channels; ++c) {
        const int mixed = int(data[i * channels + c]) + value;
        data[i * channels + c] = static_cast<int16_t>(std::max(-32768, std::min(32767, mixed)));
      }
    }
    // Single callback writer; exchange by the host may only reset peak to zero.
    auto previous = state_->peak.load(std::memory_order_relaxed);
    if (previous < peak && !state_->peak.compare_exchange_strong(previous, peak, std::memory_order_relaxed)) {
      state_->peak.store(peak, std::memory_order_relaxed);
    }
    state_->rendered_until.store(now_ns() + 10000000, std::memory_order_release);
    state_->completed.store(read + available, std::memory_order_release);
  }
 private:
  std::shared_ptr<State> state_;
  webrtc::scoped_refptr<webrtc::AudioMixer> mixer_;
  uint64_t render_generation_ = 0;
  bool playing_ = false;
  size_t waiting_blocks_ = 0, attack_samples_ = 0;
};
}
webrtc::scoped_refptr<webrtc::AudioMixer> nanocodex_take_pcm_mixer() {
  if (!pending) return nullptr;
  auto state = std::move(pending);
  return webrtc::make_ref_counted<Mixer>(std::move(state));
}
extern "C" {
void* nanocodex_pcm_create() {
  pending = std::make_shared<State>();
  return new std::shared_ptr<State>(pending);
}
void nanocodex_pcm_destroy(void* handle) {
  auto* state = static_cast<std::shared_ptr<State>*>(handle);
  if (pending == *state) pending.reset();
  delete state;
}
uint16_t nanocodex_pcm_peak(void* handle) {
  return (*static_cast<std::shared_ptr<State>*>(handle))->peak.exchange(0);
}
bool nanocodex_pcm_test_attached(void* handle) {
  return (*static_cast<std::shared_ptr<State>*>(handle))->attached_mixer.load() != nullptr;
}
// Test caller MUST retain factory; no concurrent device rendering.
size_t nanocodex_pcm_test_render(void* handle, int16_t* data, size_t capacity) {
  auto state = *static_cast<std::shared_ptr<State>*>(handle);
  auto* mixer = state->attached_mixer.load();
  if (!mixer) return 0;
  webrtc::AudioFrame frame;
  mixer->Mix(1, &frame);
  const size_t count = std::min(capacity, frame.samples_per_channel());
  std::copy(frame.data(), frame.data() + count, data);
  return count;
}
int nanocodex_pcm_test_render_completion(void* handle) {
  int observed = -1;
  completion_probe = &observed;
  std::array<int16_t, kBlock> output{};
  nanocodex_pcm_test_render(handle, output.data(), output.size());
  completion_probe = nullptr;
  return observed;
}
// Deterministic producer-preemption harness. Call only with the producer idle.
void nanocodex_pcm_test_lock_producer(void* handle) {
  (*static_cast<std::shared_ptr<State>*>(handle))->producer_mutex.lock();
}
void nanocodex_pcm_test_unlock_producer(void* handle) {
  (*static_cast<std::shared_ptr<State>*>(handle))->producer_mutex.unlock();
}
int nanocodex_pcm_begin(void* handle, uint64_t generation) {
  auto state = *static_cast<std::shared_ptr<State>*>(handle);
  std::lock_guard<std::mutex> lock(state->producer_mutex);
  if (!generation || generation <= state->generation) return -1;
  state->generation = generation;
  state->producer_audible_end = state->written.load(std::memory_order_relaxed);
  state->peak.store(0);
  state->active_generation.store(generation, std::memory_order_release);
  return 0;
}
int nanocodex_pcm_write(void* handle, uint64_t generation, const int16_t* data, size_t length, bool finish) {
  auto state = *static_cast<std::shared_ptr<State>*>(handle);
  std::lock_guard<std::mutex> lock(state->producer_mutex);
  if (!generation || state->active_generation.load() != generation || state->finish_generation.load() == generation) return -1;
  const auto written = state->written.load(std::memory_order_relaxed);
  const auto read = state->read.load(std::memory_order_acquire);
  if (length > kCapacity - (written - read)) return 1;
  for (size_t i = 0; i < length; ++i) {
    state->samples[(written + i) % kCapacity] = {data[i], generation};
    if (data[i]) state->producer_audible_end = written + i + 1;
  }
  // EOF metadata precedes publication of padding: a callback observing the
  // final samples must also observe their true audible endpoint.
  if (finish) {
    state->audible_end.store(state->producer_audible_end, std::memory_order_relaxed);
    state->finish_generation.store(generation, std::memory_order_release);
  }
  state->written.store(written + length, std::memory_order_release);
  return 0;
}
size_t nanocodex_pcm_available(void* handle) {
  auto state = *static_cast<std::shared_ptr<State>*>(handle);
  std::lock_guard<std::mutex> lock(state->producer_mutex);
  return kCapacity - (state->written.load() - state->read.load());
}
int nanocodex_pcm_status(void* handle, uint64_t generation) {
  auto state = *static_cast<std::shared_ptr<State>*>(handle);
  if (!generation || state->active_generation.load() != generation) return -1;
  return status(*state);
}
int nanocodex_pcm_cancel(void* handle, uint64_t generation) {
  auto state = *static_cast<std::shared_ptr<State>*>(handle);
  std::lock_guard<std::mutex> lock(state->producer_mutex);
  if (generation != state->generation) return -1;
  state->active_generation.store(0, std::memory_order_release);
  state->peak.store(0);
  return 0;
}
}
