package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"image/jpeg"
	"image/png"
	"math"
	"os/exec"
	"time"
)

type agentInput struct {
	Action     string   `json:"action"`
	X          *float64 `json:"x,omitempty"`
	Y          *float64 `json:"y,omitempty"`
	EndX       *float64 `json:"endX,omitempty"`
	EndY       *float64 `json:"endY,omitempty"`
	Button     *int     `json:"button,omitempty"`
	Text       *string  `json:"text,omitempty"`
	Key        *uint16  `json:"key,omitempty"`
	Modifiers  []uint16 `json:"modifiers,omitempty"`
	DeltaX     *float64 `json:"deltaX,omitempty"`
	DeltaY     *float64 `json:"deltaY,omitempty"`
	DurationMS *int     `json:"durationMs,omitempty"`
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
	ctx                   context.Context
	cancel                context.CancelFunc
}
type agentResult struct {
	Status string `json:"status,omitempty"`
	JPEG   string `json:"jpeg,omitempty"`
	Width  int    `json:"width,omitempty"`
	Height int    `json:"height,omitempty"`
}

func pointer[T any](value T) *T { return &value }

func (action agentInput) steps(generation string) ([]agentStep, error) {
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
			add(remoteInput{Kind: "button", X: action.X, Y: action.Y, Button: button, Down: pointer(down)}, 0)
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
		for _, down := range []bool{true, false} {
			add(remoteInput{Kind: "key", Key: action.Key, Down: pointer(down)}, 0)
		}
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
		limit = 500_000
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
	scale := math.Min(1, 1280/float64(max(width, height)))
	// Distribution builds of grim may advertise JPEG while disabling it at
	// compile time. PNG is its baseline format; encode a bounded JPEG here only
	// when the agent asks for an observation. The live H.264 path is unaffected.
	command := exec.CommandContext(ctx, "grim", "-t", "png", "-l", "1", "-s", fmt.Sprintf("%.6f", scale), "-")
	captured := &boundedSnapshot{limit: 8_000_000}
	command.Stdout = captured
	command.WaitDelay = time.Second
	if command.Run() != nil {
		return agentResult{Status: "unavailable"}
	}
	config, err := png.DecodeConfig(bytes.NewReader(captured.buffer.Bytes()))
	if err != nil || config.Width < 1 || config.Height < 1 || config.Width > 1280 || config.Height > 1280 {
		return agentResult{Status: "unavailable"}
	}
	frame, err := png.Decode(bytes.NewReader(captured.buffer.Bytes()))
	if err != nil || ctx.Err() != nil {
		return agentResult{Status: "unavailable"}
	}
	output := &boundedSnapshot{}
	if jpeg.Encode(output, frame, &jpeg.Options{Quality: 65}) != nil || ctx.Err() != nil {
		return agentResult{Status: "unavailable"}
	}
	return agentResult{Status: "ok", JPEG: base64.StdEncoding.EncodeToString(output.buffer.Bytes()), Width: config.Width, Height: config.Height}
}
