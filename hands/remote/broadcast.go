package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"net/url"
	"os"
	"os/exec"
	"os/signal"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

const broadcastHelperEnv = "NANOCODEX_BROADCAST_HELPER"

type broadcastResult struct {
	Status  string `json:"status"`
	Preset  string `json:"preset,omitempty"`
	Width   int    `json:"width,omitempty"`
	Height  int    `json:"height,omitempty"`
	FPS     int    `json:"fps,omitempty"`
	Bitrate int    `json:"bitrate_kbps,omitempty"`
	Error   string `json:"error,omitempty"`
}

func broadcastSettings(preset string, width, height int) (broadcastResult, error) {
	if preset == "" {
		preset = "source"
	}
	r := broadcastResult{Preset: preset, FPS: 60, Bitrate: 24000}
	w, h := 3840, 2160
	switch preset {
	case "source":
	case "1080p":
		w, h, r.Bitrate = 1920, 1080, 8000
	case "720p":
		w, h, r.Bitrate = 1280, 720, 4500
	case "twitch":
		w, h, r.Bitrate = 1920, 1080, 6000
	case "x":
		w, h, r.FPS, r.Bitrate = 1920, 1080, 30, 9000
	default:
		return r, errors.New("invalid_request")
	}
	if width < 2 || height < 2 || width > 16384 || height > 16384 {
		return r, errors.New("invalid_request")
	}
	scale := math.Min(1, math.Min(float64(w)/float64(width), float64(h)/float64(height)))
	r.Width = max(2, int(float64(width)*scale)/2*2)
	r.Height = max(2, int(float64(height)*scale)/2*2)
	return r, nil
}
func validateBroadcastURL(raw string) error {
	u, err := url.Parse(raw)
	if err != nil || len(raw) > 4096 || strings.ContainsAny(raw, "\x00\r\n\t ") || u == nil || (u.Scheme != "rtmp" && u.Scheme != "rtmps") || u.Hostname() == "" || u.User != nil || u.Fragment != "" || u.Path == "" || u.Path == "/" {
		return errors.New("invalid_request")
	}
	if port := u.Port(); port != "" {
		n, e := strconv.Atoi(port)
		if e != nil || n < 1 || n > 65535 {
			return errors.New("invalid_request")
		}
	}
	return nil
}

// Full-resolution input is independent of preview. Bound both input and mux queues.
func broadcastArgs(video, audio []string, r broadcastResult, destination string) []string {
	args := []string{"-hide_banner", "-loglevel", "error", "-nostdin", "-thread_queue_size", "2"}
	args = append(args, video...)
	args = append(args, "-thread_queue_size", "8")
	args = append(args, audio...)
	gop := r.FPS * 2
	if r.Preset == "x" {
		gop = r.FPS * 3
	}
	args = append(args, "-map", "0:v:0", "-map", "1:a:0", "-vf", fmt.Sprintf("scale=%d:%d", r.Width, r.Height), "-r", strconv.Itoa(r.FPS), "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-profile:v", "high", "-pix_fmt", "yuv420p", "-b:v", fmt.Sprintf("%dk", r.Bitrate), "-maxrate", fmt.Sprintf("%dk", r.Bitrate), "-bufsize", fmt.Sprintf("%dk", r.Bitrate*2), "-g", strconv.Itoa(gop), "-keyint_min", strconv.Itoa(gop), "-sc_threshold", "0", "-bf", "0", "-c:a", "aac", "-b:a", "128k", "-ar", "48000", "-ac", "2", "-af", "aresample=async=1:first_pts=0", "-max_muxing_queue_size", "128", "-rw_timeout", "10000000")
	if strings.HasPrefix(destination, "rtmps:") {
		args = append(args, "-tls_verify", "1")
	}
	return append(args, "-progress", "pipe:1", "-stats_period", "0.5", "-f", "flv", "-flvflags", "no_duration_filesize", destination)
}

// Waymote passes full raw dimensions before applying its preview scale. Ignore
// its output options and capture exclusively the default playback sink monitor.
func runBroadcastEncoder() error {
	// Linux parent-death signals follow the spawning thread. Keep it alive
	// until FFmpeg exits, including Waymote replacing this helper on resize.
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGTERM, os.Interrupt)
	defer cancel()
	destination := os.Getenv("NANOCODEX_BROADCAST_URL")
	if err := validateBroadcastURL(destination); err != nil {
		return err
	}
	var size string
	for i := 1; i+1 < len(os.Args); i++ {
		if os.Args[i] == "-video_size" {
			size = os.Args[i+1]
		}
	}
	var width, height int
	if _, err := fmt.Sscanf(size, "%dx%d", &width, &height); err != nil {
		return errors.New("capture_failed")
	}
	r, err := broadcastSettings(os.Getenv("NANOCODEX_BROADCAST_PRESET"), width, height)
	if err != nil {
		return err
	}
	monitor, err := desktopMonitor(ctx)
	if err != nil {
		return errors.New("capture_failed")
	}
	video := []string{"-f", "rawvideo", "-pixel_format", "bgra", "-video_size", size, "-framerate", strconv.Itoa(r.FPS), "-i", "pipe:0"}
	audio := []string{"-f", "pulse", "-fragment_size", "3840", "-i", monitor}
	metadata, _ := json.Marshal(r)
	fmt.Fprintf(os.Stdout, "settings=%s\n", metadata)
	command := exec.CommandContext(ctx, "ffmpeg", broadcastArgs(video, audio, r, destination)...)
	configureBroadcastEncoder(command)
	command.Stdin, command.Stdout, command.Stderr = os.Stdin, os.Stdout, io.Discard
	command.WaitDelay = time.Second
	if command.Run() != nil {
		return errors.New("broadcast_failed")
	}
	return nil
}

type desktopBroadcast struct {
	lifecycle sync.Mutex
	mu        sync.Mutex
	result    broadcastResult
	cancel    context.CancelFunc
	done      chan struct{}
}

func newDesktopBroadcast() *desktopBroadcast {
	return &desktopBroadcast{result: broadcastResult{Status: "idle"}}
}
func (b *desktopBroadcast) status() broadcastResult {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.result
}
func (b *desktopBroadcast) update(fn func(*broadcastResult)) {
	b.mu.Lock()
	defer b.mu.Unlock()
	fn(&b.result)
}
func (b *desktopBroadcast) stop() {
	b.lifecycle.Lock()
	defer b.lifecycle.Unlock()
	if b.cancel != nil {
		b.cancel()
		<-b.done
		b.cancel = nil
		b.done = nil
	}
	b.update(func(r *broadcastResult) { r.Status = "stopped"; r.Error = "" })
}
func (b *desktopBroadcast) start(parent context.Context, waymote, destination, preset string, width, height int) broadcastResult {
	b.lifecycle.Lock()
	defer b.lifecycle.Unlock()
	r, err := broadcastSettings(preset, width, height)
	if err != nil || validateBroadcastURL(destination) != nil {
		return broadcastResult{Status: "failed", Error: "invalid_request"}
	}
	if b.cancel != nil {
		select {
		case <-b.done:
			b.cancel()
			b.cancel, b.done = nil, nil
		default:
			return broadcastResult{Status: "failed", Error: "busy"}
		}
	}
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		return broadcastResult{Status: "failed", Error: "unavailable"}
	}
	if _, err := exec.LookPath(waymote); err != nil {
		return broadcastResult{Status: "failed", Error: "unavailable"}
	}
	helper, err := os.Executable()
	if err != nil {
		return broadcastResult{Status: "failed", Error: "unavailable"}
	}
	ctx, cancel := context.WithCancel(parent)
	b.cancel = cancel
	b.done = make(chan struct{})
	r.Status = "starting"
	b.update(func(state *broadcastResult) { *state = r })
	done := b.done
	go func() {
		defer close(done)
		defer b.update(func(s *broadcastResult) { s.Status = "stopped"; s.Error = "" })
		delay := time.Second
		for ctx.Err() == nil {
			attempt, stop := context.WithCancel(ctx)
			command := exec.CommandContext(attempt, waymote, "--frame-rate", strconv.Itoa(r.FPS), "--bitrate", strconv.Itoa(r.Bitrate), "--xkb-layout", "us", "--ffmpeg", helper)
			command.Env = append(os.Environ(), broadcastHelperEnv+"=1", "NANOCODEX_BROADCAST_URL="+destination, "NANOCODEX_BROADCAST_PRESET="+r.Preset)
			command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
			command.Cancel = func() error { return syscall.Kill(-command.Process.Pid, syscall.SIGTERM) }
			command.WaitDelay = 2 * time.Second
			command.Stderr = io.Discard
			pipe, err := command.StdoutPipe()
			if err == nil {
				err = command.Start()
			}
			if err == nil {
				progress := make(chan struct{}, 1)
				readDone := make(chan struct{})
				go func() {
					defer close(readDone)
					scanner := bufio.NewScanner(pipe)
					scanner.Buffer(make([]byte, 1024), 8192)
					for scanner.Scan() {
						line := scanner.Text()
						if strings.HasPrefix(line, "settings=") {
							var actual broadcastResult
							if json.Unmarshal([]byte(strings.TrimPrefix(line, "settings=")), &actual) == nil {
								b.update(func(s *broadcastResult) { s.Width = actual.Width; s.Height = actual.Height })
							}
						}
						if strings.HasPrefix(line, "out_time_us=") {
							value, _ := strconv.ParseInt(strings.TrimPrefix(line, "out_time_us="), 10, 64)
							if value > 0 {
								select {
								case progress <- struct{}{}:
								default:
								}
							}
						}
					}
				}()
				waited := make(chan error, 1)
				go func() { waited <- command.Wait() }()
				watchdog := time.NewTimer(15 * time.Second)
				running := true
				for running {
					select {
					case <-progress:
						b.update(func(s *broadcastResult) { s.Status = "live"; s.Error = "" })
						if !watchdog.Stop() {
							select {
							case <-watchdog.C:
							default:
							}
						}
						watchdog.Reset(15 * time.Second)
						delay = time.Second
					case <-watchdog.C:
						running = false
					case <-ctx.Done():
						running = false
					case <-waited:
						waited = nil
						running = false
					}
				}
				watchdog.Stop()
				stop()
				if waited != nil {
					select {
					case <-waited:
					case <-time.After(3 * time.Second):
						_ = syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
						<-waited
					}
				}
				_ = syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
				_ = pipe.Close()
				<-readDone
			} else if pipe != nil {
				_ = pipe.Close()
			}
			stop()
			if ctx.Err() != nil {
				break
			}
			b.update(func(s *broadcastResult) { s.Status = "reconnecting"; s.Error = "connection_failed" })
			select {
			case <-ctx.Done():
			case <-time.After(delay):
			}
			delay = min(delay*2, 30*time.Second)
		}
	}()
	return r
}
