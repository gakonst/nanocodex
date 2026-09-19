package main

import (
	"bytes"
	"context"
	_ "embed"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"image"
	"image/jpeg"
	"image/png"
	"os/exec"
	"time"

	"golang.org/x/image/draw"
)

//go:embed capture_policy.json
var capturePolicyJSON []byte

var capturePolicy = func() struct {
	MaxDimension   int   `json:"max_dimension"`
	MaxBase64Bytes int   `json:"max_base64_bytes"`
	JPEGQualities  []int `json:"jpeg_qualities"`
} {
	var policy struct {
		MaxDimension   int   `json:"max_dimension"`
		MaxBase64Bytes int   `json:"max_base64_bytes"`
		JPEGQualities  []int `json:"jpeg_qualities"`
	}
	if err := json.Unmarshal(capturePolicyJSON, &policy); err != nil {
		panic(err)
	}
	return policy
}()

type agentInput struct {
	Context    json.RawMessage `json:"context,omitempty"`
	Action     string          `json:"action"`
	X          *float64        `json:"x,omitempty"`
	Y          *float64        `json:"y,omitempty"`
	EndX       *float64        `json:"endX,omitempty"`
	EndY       *float64        `json:"endY,omitempty"`
	Button     *int            `json:"button,omitempty"`
	Text       *string         `json:"text,omitempty"`
	Key        *uint16         `json:"key,omitempty"`
	Modifiers  []uint16        `json:"modifiers,omitempty"`
	DeltaX     *float64        `json:"deltaX,omitempty"`
	DeltaY     *float64        `json:"deltaY,omitempty"`
	DurationMS *int            `json:"durationMs,omitempty"`
}
type agentStep struct {
	delay time.Duration
	input remoteInput
}
type agentJob struct {
	id, owner, generation string
	deadline, nextAt      time.Time
	steps                 []agentStep
	next                  int
	snapshotStarted       bool
	observationContext    *observationContext
	ctx                   context.Context
	cancel                context.CancelFunc
}
type agentResult struct {
	Observation map[string]any `json:"observation,omitempty"`
	Status      string         `json:"status,omitempty"`
	JPEG        string         `json:"jpeg,omitempty"`
	Width       int            `json:"width,omitempty"`
	Height      int            `json:"height,omitempty"`
}

func pointer[T any](value T) *T { return &value }

func (action agentInput) steps(generation string) ([]agentStep, error) {
	if _, err := action.validateContext(); err != nil {
		return nil, err
	}
	var steps []agentStep
	add := func(event remoteInput, delay time.Duration) {
		event.Generation = generation
		event.Sequence = uint64(len(steps) + 1)
		steps = append(steps, agentStep{delay: delay, input: event})
	}
	switch action.Action {
	case "observe", "release":
	case "click":
		button := action.Button
		if button == nil {
			button = pointer(0)
		}
		for _, down := range []bool{true, false} {
			// Keep a click observable by applications that poll button state.
			// This applies only to synthesized agent clicks, not live viewer input.
			delay := time.Duration(0)
			if !down {
				delay = 50 * time.Millisecond
			}
			add(remoteInput{Kind: "button", X: action.X, Y: action.Y, Button: button, Down: pointer(down)}, delay)
		}
	case "type":
		add(remoteInput{Kind: "text", Text: action.Text}, 0)
	case "key":
		if len(action.Modifiers) > 4 {
			return nil, errors.New("invalid modifiers")
		}
		seen := map[uint16]bool{}
		for _, key := range action.Modifiers {
			if key < 224 || key > 231 || seen[key] {
				return nil, errors.New("invalid modifier")
			}
			seen[key] = true
			add(remoteInput{Kind: "key", Key: pointer(key), Down: pointer(true)}, 0)
		}
		// Applications that poll keyboard state can miss a press and release
		// delivered in the same host tick. Keep a bounded dwell between them.
		add(remoteInput{Kind: "key", Key: action.Key, Down: pointer(true)}, 0)
		add(remoteInput{Kind: "key", Key: action.Key, Down: pointer(false)}, 50*time.Millisecond)
		for i := len(action.Modifiers) - 1; i >= 0; i-- {
			add(remoteInput{Kind: "key", Key: pointer(action.Modifiers[i]), Down: pointer(false)}, 0)
		}
	case "scroll":
		add(remoteInput{Kind: "scroll", X: action.X, Y: action.Y, DeltaX: action.DeltaX, DeltaY: action.DeltaY}, 0)
	case "drag":
		duration := 300
		if action.DurationMS != nil {
			duration = *action.DurationMS
		}
		if duration < 50 || duration > 1500 || action.X == nil || action.Y == nil || action.EndX == nil || action.EndY == nil {
			return nil, errors.New("invalid drag")
		}
		count := max(2, duration/33)
		add(remoteInput{Kind: "button", X: action.X, Y: action.Y, Button: pointer(0), Down: pointer(true)}, 0)
		for i := 1; i <= count; i++ {
			fraction := float64(i) / float64(count)
			add(remoteInput{Kind: "move", X: pointer(*action.X + (*action.EndX-*action.X)*fraction), Y: pointer(*action.Y + (*action.EndY-*action.Y)*fraction)}, time.Duration(duration/count)*time.Millisecond)
		}
		add(remoteInput{Kind: "button", X: action.EndX, Y: action.EndY, Button: pointer(0), Down: pointer(false)}, 0)
	default:
		return nil, errors.New("invalid screen action")
	}
	for _, step := range steps {
		if err := step.input.validate(); err != nil {
			return nil, err
		}
	}
	return steps, nil
}

type boundedSnapshot struct {
	buffer bytes.Buffer
	limit  int
}

func (output *boundedSnapshot) Write(data []byte) (int, error) {
	limit := output.limit
	if limit == 0 {
		limit = capturePolicy.MaxBase64Bytes / 4 * 3
	}
	if output.buffer.Len()+len(data) > limit {
		return 0, errors.New("screen snapshot exceeds limit")
	}
	return output.buffer.Write(data)
}

// The agent requests an observation, not another live encoder. grim captures the
// existing compositor without competing for Waymote's single input method.
func snapshotDesktop(parent context.Context, width, height int) agentResult {
	ctx, cancel := context.WithTimeout(parent, 3*time.Second)
	defer cancel()
	if width < 1 || height < 1 || width > 65536 || height > 65536 {
		return agentResult{Status: "unavailable"}
	}
	// Configured dimensions describe the headless mode, not necessarily the
	// live output. grim -s scales logical geometry, so a physical-pixel ratio
	// here double-downscales HiDPI outputs. Capture all native geometry first.
	for _, format := range []string{"jpeg", "png"} {
		args := []string{"-t", format}
		if format == "jpeg" {
			args = append(args, "-q", fmt.Sprint(capturePolicy.JPEGQualities[0]))
		} else {
			args = append(args, "-l", "1")
		}
		captured := &boundedSnapshot{limit: maxNativeSnapshotBytes}
		command := exec.CommandContext(ctx, "grim", append(args, "-")...)
		command.Stdout = captured
		command.WaitDelay = time.Second
		if command.Run() == nil {
			if result := encodeSnapshot(ctx, captured.buffer.Bytes(), format); result.Status == "ok" {
				return result
			}
		}
		if ctx.Err() != nil {
			break
		}
	}
	return agentResult{Status: "unavailable"}
}

// Bound compressed input and decoded pixel allocation independently. This
// admits an 8K desktop but rejects compressed images with enormous dimensions.
const maxNativeSnapshotBytes = 64 * 1024 * 1024
const maxNativeSnapshotPixels = 32 * 1024 * 1024

func snapshotDimensions(width, height int) (int, int, bool) {
	if width < 1 || height < 1 || width > 65536 || height > 65536 || int64(width)*int64(height) > maxNativeSnapshotPixels {
		return 0, 0, false
	}
	longest := max(width, height)
	if longest <= capturePolicy.MaxDimension {
		return width, height, true
	}
	// Integer rounding gives an exact longest edge without decimal scale
	// truncation. Resize the entire source rectangle, including every corner.
	return max(1, (width*capturePolicy.MaxDimension+longest/2)/longest),
		max(1, (height*capturePolicy.MaxDimension+longest/2)/longest), true
}

func encodeSnapshot(ctx context.Context, data []byte, format string) agentResult {
	unavailable := agentResult{Status: "unavailable"}
	if len(data) > maxNativeSnapshotBytes || ctx.Err() != nil {
		return unavailable
	}
	var config image.Config
	var err error
	switch format {
	case "jpeg":
		config, err = jpeg.DecodeConfig(bytes.NewReader(data))
	case "png":
		config, err = png.DecodeConfig(bytes.NewReader(data))
	default:
		return unavailable
	}
	if err != nil {
		return unavailable
	}
	width, height, valid := snapshotDimensions(config.Width, config.Height)
	if !valid {
		return unavailable
	}
	if format == "jpeg" && width == config.Width && height == config.Height && base64.StdEncoding.EncodedLen(len(data)) <= capturePolicy.MaxBase64Bytes && ctx.Err() == nil {
		return agentResult{Status: "ok", JPEG: base64.StdEncoding.EncodeToString(data), Width: width, Height: height}
	}
	var frame image.Image
	if format == "jpeg" {
		frame, err = jpeg.Decode(bytes.NewReader(data))
	} else {
		frame, err = png.Decode(bytes.NewReader(data))
	}
	if err != nil || ctx.Err() != nil {
		return unavailable
	}
	if width != config.Width || height != config.Height {
		resized := image.NewRGBA(image.Rect(0, 0, width, height))
		draw.BiLinear.Scale(resized, resized.Bounds(), frame, frame.Bounds(), draw.Src, nil)
		frame = resized
	}
	for _, quality := range capturePolicy.JPEGQualities {
		output := &boundedSnapshot{}
		if ctx.Err() != nil {
			break
		}
		if jpeg.Encode(output, frame, &jpeg.Options{Quality: quality}) == nil && ctx.Err() == nil {
			return agentResult{Status: "ok", JPEG: base64.StdEncoding.EncodeToString(output.buffer.Bytes()), Width: width, Height: height}
		}
	}
	return unavailable
}

func (action agentInput) validateContext() (*observationContext, error) {
	if len(action.Context) > 0 && action.Action != "observe" {
		return nil, errors.New("context requires observe")
	}
	return parseObservationContext(action.Context)
}
