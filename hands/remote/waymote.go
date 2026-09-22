package main

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
	"unicode/utf8"

	"github.com/pion/rtp"
	"github.com/pion/webrtc/v4"
)

// Waymote owns the wlroots capture and input protocols. Pion forwards its H.264
// pipe directly into WebRTC: no decoded frames or JPEGs cross the VM boundary.
type waymoteCapture struct {
	gamepad  *gamepadController
	track    *webrtc.TrackLocalStaticRTP
	input    io.WriteCloser
	video    io.ReadCloser
	cancel   context.CancelFunc
	done     chan struct{}
	mu       sync.Mutex
	sequence uint32
	closed   bool
}

func startWaymote(ctx context.Context, executable string) (*waymoteCapture, error) {
	return startWaymoteWithDiagnostics(ctx, executable, os.Stderr)
}

func startWaymoteWithDiagnostics(ctx context.Context, executable string, diagnostics io.Writer) (*waymoteCapture, error) {
	track, err := webrtc.NewTrackLocalStaticRTP(webrtc.RTPCodecCapability{
		MimeType: webrtc.MimeTypeH264, ClockRate: 90000,
		SDPFmtpLine: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e034",
	}, "screen", "nanocodex-hand")
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(ctx)
	// libkrun TSI cannot listen on guest UDP sockets. Waymote's native Annex-B
	// stdout also avoids packet loss and a network hop between local processes.
	helper, err := os.Executable()
	if err != nil {
		cancel()
		return nil, err
	}
	bitrate, err := screenBitrate(os.Getenv("NANOCODEX_SCREEN_BITRATE_KBPS"))
	if err != nil {
		cancel()
		return nil, err
	}
	command := exec.CommandContext(ctx, executable, "--frame-rate", "60", "--bitrate", strconv.Itoa(bitrate), "--xkb-layout", "us", "--ffmpeg", helper)
	command.Env = append(os.Environ(), encoderHelperEnv+"=1")
	command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	command.Cancel = func() error {
		if command.Process == nil {
			return os.ErrProcessDone
		}
		err := syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
		if errors.Is(err, syscall.ESRCH) {
			return os.ErrProcessDone
		}
		return err
	}
	command.WaitDelay = time.Second
	command.Stderr = diagnostics
	video, err := command.StdoutPipe()
	if err != nil {
		cancel()
		return nil, err
	}
	input, err := command.StdinPipe()
	if err != nil {
		cancel()
		video.Close()
		return nil, err
	}
	if err = command.Start(); err != nil {
		cancel()
		video.Close()
		input.Close()
		return nil, err
	}
	capture := &waymoteCapture{track: track, input: input, video: video, cancel: cancel, done: make(chan struct{})}
	if controller, gamepadErr := configuredGamepad(openGamepad); gamepadErr == nil {
		capture.gamepad = controller
	} else {
		fmt.Fprintf(diagnostics, "Native gamepad unavailable: %v\n", gamepadErr)
	}
	go func() {
		forwarder := h264Forwarder{}
		err := forwarder.read(video, func(out *rtp.Packet) error {
			// A viewer can unbind during a write without stopping other viewers.
			_ = track.WriteRTP(out)
			return nil
		})
		if err != nil && ctx.Err() == nil {
			fmt.Fprintf(diagnostics, "Wayland video forwarding failed: %v\n", err)
		}
		cancel()
		_ = command.Wait()
		close(capture.done)
	}()
	return capture, nil
}

func (capture *waymoteCapture) close() {
	capture.mu.Lock()
	if capture.closed {
		capture.mu.Unlock()
		return
	}
	if capture.gamepad != nil {
		capture.gamepad.close()
	}
	_ = capture.record(5, 0, 0, 0, 0)
	capture.closed = true
	_ = capture.input.Close()
	capture.cancel()
	_ = capture.video.Close()
	capture.mu.Unlock()
	<-capture.done
}

func (capture *waymoteCapture) releaseAll() error {
	capture.mu.Lock()
	defer capture.mu.Unlock()
	if capture.closed {
		return nil
	}
	var gamepadErr error
	if capture.gamepad != nil {
		gamepadErr = capture.gamepad.release()
	}
	return errors.Join(gamepadErr, capture.record(5, 0, 0, 0, 0))
}

// Input is already account/control-lease checked by the host session. Keep the
// native daemon boundary typed and range checked too.
func (capture *waymoteCapture) apply(event remoteInput) error {
	if err := event.validate(); err != nil {
		return err
	}
	capture.mu.Lock()
	defer capture.mu.Unlock()
	if capture.closed {
		return errors.New("Wayland capture closed")
	}
	capture.sequence++
	if capture.sequence == 0 {
		capture.sequence = 1
	}
	sequence := capture.sequence
	if event.X != nil {
		if err := capture.record(1, 0, uint32(math.Round(*event.X*65535)), uint32(math.Round(*event.Y*65535)), sequence); err != nil {
			return err
		}
	}
	var down byte
	if event.Down != nil && *event.Down {
		down = 1
	}
	switch event.Kind {
	case "gamepad":
		if capture.gamepad == nil {
			return errors.New("native gamepad unavailable")
		}
		return capture.gamepad.apply(*event.Gamepad)
	case "move":
		return nil
	case "relativeMove":
		return capture.record(8, 0, math.Float32bits(float32(*event.DeltaX)), math.Float32bits(float32(*event.DeltaY)), sequence)
	case "button":
		return capture.record(2, down, 0x110+uint32(*event.Button), 0, sequence)
	case "scroll":
		return capture.record(3, 0, math.Float32bits(float32(-*event.DeltaX)), math.Float32bits(float32(-*event.DeltaY)), sequence)
	case "key":
		key, ok := hidToEvdev[*event.Key]
		if !ok {
			return errors.New("unsupported keyboard usage")
		}
		return capture.record(4, down, key, 0, sequence)
	case "releaseAll":
		var gamepadErr error
		if capture.gamepad != nil {
			gamepadErr = capture.gamepad.release()
		}
		return errors.Join(gamepadErr, capture.record(5, 0, 0, 0, 0))
	case "text":
		if os.Getenv("NANOCODEX_WAYLAND_TEXT_X11") == "1" || os.Getenv("NANOCODEX_WAYLAND_TEXT_WTYPE") == "1" {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			if handled, err := typeWaylandText(ctx, *event.Text); handled || err != nil {
				return err
			}
		}
		// Waymote limits each UTF-8 composition commit to 4000 bytes.
		remaining := []byte(*event.Text)
		for len(remaining) > 0 {
			count := min(4000, len(remaining))
			for count < len(remaining) && !utf8.RuneStart(remaining[count]) {
				count--
			}
			if err := capture.record(10, 0, uint32(count), sequence, 0); err != nil {
				return err
			}
			if err := capture.write(remaining[:count]); err != nil {
				return err
			}
			remaining = remaining[count:]
			if len(remaining) > 0 {
				capture.sequence++
				if capture.sequence == 0 {
					capture.sequence = 1
				}
				sequence = capture.sequence
			}
		}
		return nil
	default:
		return errors.New("unsupported input")
	}
}

// typeWaylandText selects a backend before sending any text. A failed typing
// command may have delivered a prefix, so it must never trigger another backend.
func typeWaylandText(ctx context.Context, text string) (bool, error) {
	if os.Getenv("NANOCODEX_WAYLAND_TEXT_X11") == "1" && os.Getenv("DISPLAY") != "" {
		path, err := exec.LookPath("xdotool")
		if err == nil {
			focused, err := focusedXApplication(ctx, path)
			if err != nil {
				return false, err
			}
			if focused {
				// XTEST follows the live keyboard focus, including changes after the
				// probe. Do not pin a window with XSendEvent or steal focus.
				command := exec.CommandContext(ctx, path, "type", "--clearmodifiers", "--delay", "1", "--file", "-")
				command.Stdin = strings.NewReader(text)
				command.WaitDelay = 25 * time.Millisecond
				return true, command.Run()
			}
		} else if !errors.Is(err, exec.ErrNotFound) {
			return false, err
		}
	}
	// Compositors with an existing input-method owner may reject Waymote's
	// IME commits. Keep the virtual-keyboard fallback independently opt-in.
	if os.Getenv("NANOCODEX_WAYLAND_TEXT_WTYPE") == "1" {
		command := exec.CommandContext(ctx, "wtype", "-")
		command.Stdin = strings.NewReader(text)
		command.WaitDelay = 25 * time.Millisecond
		return true, command.Run()
	}
	return false, nil
}

func focusedXApplication(ctx context.Context, executable string) (bool, error) {
	ctx, cancel := context.WithTimeout(ctx, 250*time.Millisecond)
	defer cancel()
	// Chaining resolves the PID from the actual keyboard focus, rather than
	// the window manager's active-window hint. Root/None have no client PID.
	command := exec.CommandContext(ctx, executable, "getwindowfocus", "getwindowpid")
	var output, diagnostics bytes.Buffer
	command.Stdout = &output
	command.Stderr = &diagnostics
	// A descendant inheriting stdout must not keep the probe blocked.
	command.WaitDelay = 25 * time.Millisecond
	err := command.Run()
	if ctx.Err() != nil {
		return false, fmt.Errorf("X11 focus probe: %w", ctx.Err())
	}
	if err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) && exit.ExitCode() == 1 {
			message := diagnostics.String()
			// Exit 1 alone can also mean a broken display connection. Only
			// recognized absent-focus/PID diagnostics permit another backend.
			if strings.Contains(message, "has no pid associated with it.") ||
				strings.Contains(message, "xdo_focus_window reported an error") ||
				strings.Contains(message, "XGetInputFocus returned the focused window of 1.") {
				return false, nil
			}
		}
		return false, fmt.Errorf("X11 focus probe: %w", err)
	}
	pid, err := strconv.ParseInt(strings.TrimSpace(output.String()), 10, 32)
	if err != nil {
		return false, errors.New("X11 focus probe returned an invalid PID")
	}
	return pid > 0, nil
}

func (capture *waymoteCapture) record(kind, state byte, a, b, sequence uint32) error {
	var wire [16]byte
	wire[0] = 2
	wire[1] = kind
	wire[2] = state
	binary.LittleEndian.PutUint32(wire[4:8], a)
	binary.LittleEndian.PutUint32(wire[8:12], b)
	binary.LittleEndian.PutUint32(wire[12:16], sequence)
	return capture.write(wire[:])
}
func (capture *waymoteCapture) write(data []byte) error {
	if pipe, ok := capture.input.(*os.File); ok {
		_ = pipe.SetWriteDeadline(time.Now().Add(250 * time.Millisecond))
	}
	n, err := capture.input.Write(data)
	if err != nil {
		return err
	}
	if n != len(data) {
		return fmt.Errorf("incomplete input write: %d", n)
	}
	return nil
}

// USB HID page 0x07 -> Linux evdev, matching the Apple and browser wire format.
var hidToEvdev = map[uint16]uint32{
	4: 30, 5: 48, 6: 46, 7: 32, 8: 18, 9: 33, 10: 34, 11: 35, 12: 23, 13: 36, 14: 37, 15: 38,
	16: 50, 17: 49, 18: 24, 19: 25, 20: 16, 21: 19, 22: 31, 23: 20, 24: 22, 25: 47, 26: 17, 27: 45, 28: 21, 29: 44,
	30: 2, 31: 3, 32: 4, 33: 5, 34: 6, 35: 7, 36: 8, 37: 9, 38: 10, 39: 11, 40: 28, 41: 1, 42: 14, 43: 15, 44: 57,
	45: 12, 46: 13, 47: 26, 48: 27, 49: 43, 51: 39, 52: 40, 53: 41, 54: 51, 55: 52, 56: 53, 57: 58,
	58: 59, 59: 60, 60: 61, 61: 62, 62: 63, 63: 64, 64: 65, 65: 66, 66: 67, 67: 68, 68: 87, 69: 88,
	73: 110, 74: 102, 75: 104, 76: 111, 77: 107, 78: 109, 79: 106, 80: 105, 81: 108, 82: 103,
	83: 69, 84: 98, 85: 55, 86: 74, 87: 78, 88: 96, 89: 79, 90: 80, 91: 81, 92: 75, 93: 76, 94: 77, 95: 71, 96: 72, 97: 73, 98: 82, 99: 83, 100: 86, 103: 117,
	224: 29, 225: 42, 226: 56, 227: 125, 228: 97, 229: 54, 230: 100, 231: 126,
}

// Shared desktop quality setting, in kbit/s; bounds avoid accidental unbounded traffic.
func screenBitrate(value string) (int, error) {
	if value == "" {
		return 6000, nil
	}
	bitrate, err := strconv.Atoi(value)
	if err != nil || bitrate < 1000 || bitrate > 100000 {
		return 0, errors.New("NANOCODEX_SCREEN_BITRATE_KBPS must be 1000 through 100000")
	}
	return bitrate, nil
}
