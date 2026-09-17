package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"strings"
	"time"

	"github.com/pion/webrtc/v4"
	"github.com/pion/webrtc/v4/pkg/media"
	"github.com/pion/webrtc/v4/pkg/media/oggreader"
)

// Capture only the default playback sink's explicit monitor. Never fall back to
// PulseAudio's default source: it is commonly a microphone.
func desktopMonitor(ctx context.Context) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	name, err := exec.CommandContext(ctx, "pactl", "get-default-sink").Output()
	if err != nil {
		return "", err
	}
	data, err := exec.CommandContext(ctx, "pactl", "--format=json", "list", "sinks").Output()
	if err != nil {
		return "", err
	}
	return monitorSource(data, strings.TrimSpace(string(name)))
}

func monitorSource(data []byte, name string) (string, error) {
	var sinks []struct {
		Name    string `json:"name"`
		Monitor string `json:"monitor_source_name"`
		Source  any    `json:"monitor_source"`
	}
	if err := json.Unmarshal(data, &sinks); err != nil {
		return "", err
	}
	for _, sink := range sinks {
		if sink.Monitor == "" {
			sink.Monitor, _ = sink.Source.(string)
		}
		if sink.Name == name && name != "" && sink.Monitor != "" && len(sink.Monitor) <= 4096 {
			return sink.Monitor, nil
		}
	}
	return "", errors.New("desktop playback monitor unavailable")
}

type audioCapture struct {
	track  *webrtc.TrackLocalStaticSample
	cancel context.CancelFunc
	done   chan struct{}
}

func startDesktopAudio(ctx context.Context, diagnostics io.Writer) (*audioCapture, error) {
	monitor, err := desktopMonitor(ctx)
	if err != nil {
		return nil, err
	}
	return startOpusAudio(ctx, diagnostics, []string{"-f", "pulse", "-fragment_size", "3840", "-i", monitor})
}

func startOpusAudio(parent context.Context, diagnostics io.Writer, input []string) (*audioCapture, error) {
	track, err := webrtc.NewTrackLocalStaticSample(webrtc.RTPCodecCapability{
		MimeType: webrtc.MimeTypeOpus, ClockRate: 48000, Channels: 2,
		SDPFmtpLine: "minptime=10;useinbandfec=1;stereo=1",
	}, "desktop-audio", "nanocodex-hand")
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(parent)
	args := append([]string{"-hide_banner", "-loglevel", "error", "-nostdin"}, input...)
	args = append(args, "-vn", "-ar", "48000", "-ac", "2", "-c:a", "libopus", "-b:a", "128k", "-application", "audio", "-frame_duration", "20", "-page_duration", "20000", "-flush_packets", "1", "-f", "ogg", "pipe:1")
	command := exec.CommandContext(ctx, "ffmpeg", args...)
	command.Stderr = diagnostics
	command.WaitDelay = time.Second
	pipe, err := command.StdoutPipe()
	if err != nil {
		cancel()
		return nil, err
	}
	if err = command.Start(); err != nil {
		cancel()
		_ = pipe.Close()
		return nil, err
	}
	capture := &audioCapture{track: track, cancel: cancel, done: make(chan struct{})}
	ready := make(chan error, 1)
	go func() {
		defer close(capture.done)
		defer cancel()
		defer command.Wait() // Reap the encoder, including cancellation/error paths.
		started := false
		var pending []byte
		for {
			packets, err := readOpusPage(pipe, &pending)
			if err != nil {
				if !started {
					ready <- err
				} else if ctx.Err() == nil {
					fmt.Fprintf(diagnostics, "Desktop audio stopped: %v\n", err)
				}
				cancel()
				return
			}
			for _, packet := range packets {
				if bytes.HasPrefix(packet, []byte("OpusHead")) {
					if len(packet) != 19 || packet[8] != 1 || packet[9] != 2 || packet[18] != 0 {
						ready <- errors.New("invalid desktop Opus header")
						cancel()
						return
					}
					continue
				}
				if bytes.HasPrefix(packet, []byte("OpusTags")) {
					continue
				}
				_ = track.WriteSample(media.Sample{Data: packet, Duration: 20 * time.Millisecond})
				if !started {
					started = true
					ready <- nil
				}
			}
		}
	}()
	select {
	case err = <-ready:
		if err == nil {
			return capture, nil
		}
	case <-time.After(3 * time.Second):
		err = errors.New("desktop audio encoder startup timed out")
	case <-parent.Done():
		err = parent.Err()
	}
	capture.close()
	return nil, fmt.Errorf("desktop audio: %w", err)
}

func (capture *audioCapture) close() { capture.cancel(); <-capture.done }

// Ogg pages may hold several packets when the Pulse clock drifts. Preserve the
// lacing boundaries; forwarding the concatenated page corrupts Opus audio.
func readOpusPage(reader io.Reader, pending *[]byte) ([][]byte, error) {
	var header [27]byte
	if _, err := io.ReadFull(reader, header[:]); err != nil {
		return nil, err
	}
	if string(header[:4]) != "OggS" || header[4] != 0 || (header[5]&1 != 0) != (len(*pending) > 0) {
		return nil, errors.New("invalid desktop Ogg page")
	}
	laces := make([]byte, int(header[26]))
	if _, err := io.ReadFull(reader, laces); err != nil {
		return nil, err
	}
	size := 0
	for _, count := range laces {
		size += int(count)
	}
	payload := make([]byte, size)
	if _, err := io.ReadFull(reader, payload); err != nil {
		return nil, err
	}
	page := append(append(header[:], laces...), payload...)
	checked, err := oggreader.NewWithOptions(bytes.NewReader(page))
	if err != nil {
		return nil, err
	}
	if _, _, err = checked.ParseNextPage(); err != nil {
		return nil, err
	}
	return splitOpusPackets(laces, payload, pending)
}
func splitOpusPackets(laces, payload []byte, pending *[]byte) ([][]byte, error) {
	var packets [][]byte
	offset := 0
	for _, count := range laces {
		end := offset + int(count)
		if end > len(payload) || len(*pending)+int(count) > 4000 {
			return nil, errors.New("desktop Opus packet exceeds bounds")
		}
		*pending = append(*pending, payload[offset:end]...)
		offset = end
		if count < 255 {
			if len(*pending) == 0 {
				return nil, errors.New("empty desktop Opus packet")
			}
			packets = append(packets, *pending)
			*pending = nil
		}
	}
	if offset != len(payload) {
		return nil, errors.New("invalid desktop Opus lacing")
	}
	return packets, nil
}

// Headless desktop images need a playback server as well as FFmpeg's Pulse
// input. PulseAudio creates its auto_null playback sink when hardware is absent.
// Reuse an existing server; only stop a child that this desktop owns.
func startDesktopPlayback(ctx context.Context) func() {
	if _, err := desktopMonitor(ctx); err == nil {
		return func() {}
	}
	audioCtx, cancel := context.WithCancel(ctx)
	command := exec.CommandContext(audioCtx, "pulseaudio", "--daemonize=no", "--exit-idle-time=-1", "--log-target=stderr")
	command.Stdout, command.Stderr = io.Discard, io.Discard
	command.WaitDelay = time.Second
	if command.Start() != nil {
		cancel()
		return func() {}
	}
	done := make(chan struct{})
	go func() { _ = command.Wait(); close(done) }()
	return func() { cancel(); <-done }
}
