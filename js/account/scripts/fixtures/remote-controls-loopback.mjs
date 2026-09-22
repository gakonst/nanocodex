// Install into an isolated page with Playwright addInitScript. The viewer uses
// real RTCPeerConnection/MediaStream/getUserMedia. Only the account signaling
// socket and synthetic publisher are supplied by this localhost-only fixture.
export function installRemoteControlsLoopback() {
  window.loopback = { captures: [], captureRequests: 0, microphoneRequests: [], channels: [], errors: [] };
  const capture = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
  navigator.mediaDevices.getUserMedia = async constraints => {
    window.loopback.captureRequests++;
    const stream = await capture(constraints);
    window.loopback.captures.push(stream);
    return stream;
  };
  window.WebSocket = class {
    static OPEN = 1;
    constructor(url) {
      if (new URL(url).host !== location.host) throw new Error('External socket denied');
      this.readyState = 1;
      this.peer = new RTCPeerConnection({ iceServers: [] });
      window.loopback.peer = this.peer;
      this.queue = Promise.resolve(); this.candidates = [];
      const emit = value => this.onmessage?.({ data: JSON.stringify(value) });
      this.peer.onicecandidate = ({ candidate }) => {
        if (candidate) emit({ type: 'signal', signal: { type: 'candidate', candidate: candidate.candidate, sdpMid: candidate.sdpMid, sdpMLineIndex: candidate.sdpMLineIndex } });
      };
      const reliable = this.peer.createDataChannel('remote-control-v1', { ordered: true });
      this.peer.createDataChannel('remote-motion-v1', { ordered: false, maxRetransmits: 0 });
      reliable.onmessage = ({ data }) => {
        const value = JSON.parse(data); window.loopback.channels.push(value);
        if (value.type === 'acquire') reliable.send(JSON.stringify({ type: 'granted', generation: 'loopback-lease', relativePointer: true, microphone: true }));
        else if (value.type === 'release') reliable.send(JSON.stringify({ type: 'revoked', generation: value.generation }));
        else if (value.type === 'microphone') {
          window.loopback.microphoneRequests.push(value);
          // The test explicitly releases this ACK to prove capture waits for it.
          if (value.enabled) window.loopback.ackMicrophone = () => reliable.send(JSON.stringify(value));
        }
      };
      this.audio = new AudioContext(); window.loopback.audio = this.audio;
      const destination = this.audio.createMediaStreamDestination();
      const oscillator = this.audio.createOscillator(); oscillator.connect(destination); oscillator.start();
      const audioTrack = destination.stream.getAudioTracks()[0];
      this.peer.addTransceiver(audioTrack, { direction: 'sendrecv', streams: [destination.stream] });
      const canvas = document.createElement('canvas'); canvas.width = 960; canvas.height = 540;
      const context = canvas.getContext('2d');
      context.fillStyle = '#192d3c'; context.fillRect(0, 0, 960, 540);
      const video = canvas.captureStream(5); this.peer.addTrack(video.getVideoTracks()[0], video);
      this.frameTimer = setInterval(() => context.fillRect(0, 0, 960, 540), 100);
      this.peer.ontrack = ({ track }) => {
        if (track.kind !== 'audio') return;
        window.loopback.returnTrack = track;
        // Keep decoded return audio flowing through a media sink. Chromium's
        // default Playwright --mute-audio flag prevents physical playback.
        const sink = document.createElement('audio'); sink.srcObject = new MediaStream([track]);
        sink.autoplay = true; document.body.append(sink);
        window.loopback.returnSink = sink;
        void sink.play().catch(error => window.loopback.errors.push(String(error)));
      };
      window.loopback.returnAudioStats = async () => [...(await this.peer.getStats()).values()]
        .filter(value => value.type === 'inbound-rtp' && value.kind === 'audio')
        .map(value => ({ bytesReceived: value.bytesReceived, packetsReceived: value.packetsReceived, totalAudioEnergy: value.totalAudioEnergy, totalSamplesReceived: value.totalSamplesReceived, totalSamplesDuration: value.totalSamplesDuration, concealedSamples: value.concealedSamples, audioLevel: value.audioLevel }));
      setTimeout(async () => {
        try {
          emit({ type: 'ready', connection_id: 'loopback-connection' });
          const offer = await this.peer.createOffer(); await this.peer.setLocalDescription(offer);
          emit({ type: 'signal', signal: { type: 'offer', sdp: offer.sdp } });
        } catch (error) { window.loopback.errors.push(String(error)); }
      }, 0);
    }
    send(raw) {
      const message = JSON.parse(raw);
      if (message.type !== 'signal') return;
      this.queue = this.queue.then(async () => {
        const value = message.signal;
        if (value.type === 'answer') {
          await this.peer.setRemoteDescription(value);
          for (const candidate of this.candidates.splice(0)) await this.peer.addIceCandidate(candidate);
        } else if (value.type === 'candidate') {
          if (this.peer.remoteDescription) await this.peer.addIceCandidate(value);
          else this.candidates.push(value);
        }
      }).catch(error => { window.loopback.errors.push(String(error)); });
    }
    close() {
      this.readyState = 3; clearInterval(this.frameTimer); this.peer.close(); void this.audio.close();
      if (window.loopback.returnSink) {
        window.loopback.returnSink.srcObject = null; window.loopback.returnSink.remove();
      }
    }
  };
}
