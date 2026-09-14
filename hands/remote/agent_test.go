package main

import (
	"bytes"
	"io"
	"math"
	"testing"
	"time"
)

func TestAgentActionsValidateBeforeInput(t *testing.T) {
	invalid := []agentInput{
		{Action: "drag", X: pointer(0.0), Y: pointer(0.0), EndX: pointer(math.NaN()), EndY: pointer(1.0)},
		{Action: "drag", X: pointer(0.0), Y: pointer(0.0), EndX: pointer(1.0), EndY: pointer(1.0), DurationMS: pointer(1501)},
		{Action: "click", X: pointer(-0.1), Y: pointer(0.5)},
		{Action: "key", Key: pointer(uint16(40)), Modifiers: []uint16{224, 224}},
		{Action: "key", Key: pointer(uint16(232))},
		{Action: "type", Text: pointer(string(bytes.Repeat([]byte{'a'}, 4097)))},
	}
	for _, input := range invalid {
		if _, err := input.steps("lease"); err == nil {
			t.Fatalf("accepted invalid %s", input.Action)
		}
	}
	steps, err := (agentInput{Action: "key", Key: pointer(uint16(4)), Modifiers: []uint16{227}}).steps("lease")
	if err != nil || len(steps) != 4 || *steps[0].input.Key != 227 || *steps[3].input.Down {
		t.Fatal("modifier was not released", err)
	}
	drag, err := (agentInput{Action: "drag", X: pointer(0.1), Y: pointer(0.2), EndX: pointer(0.8), EndY: pointer(0.9)}).steps("lease")
	var duration time.Duration
	for index, step := range drag {
		duration += step.delay
		if step.input.Sequence != uint64(index+1) {
			t.Fatal("invalid input ordering")
		}
	}
	if err != nil || duration < 250*time.Millisecond || duration > 300*time.Millisecond || *drag[len(drag)-1].input.Down {
		t.Fatal("unbounded drag", err)
	}
}

func TestAgentSnapshotPipeIsBounded(t *testing.T) {
	var output boundedSnapshot
	// io.Copy must not bypass the cap through bytes.Buffer.ReadFrom.
	if _, err := io.Copy(&output, bytes.NewReader(make([]byte, 500_001))); err == nil || output.buffer.Len() > 500_000 {
		t.Fatal("snapshot escaped output bound")
	}
}
