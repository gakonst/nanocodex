//go:build linux

package main

import "testing"

func TestGamepadLinuxFrames(t *testing.T) {
	events := gamepadEvents(gamepadState{LeftX: -1, LeftY: 1, RightX: 1, RightY: -1, LeftTrigger: 1, RightTrigger: 0.5, Buttons: []string{"a", "start", "dpadUp", "dpadRight"}})
	if len(events) != 19 {
		t.Fatalf("frame size %d", len(events))
	}
	expected := map[uint16]int32{0: -32768, 1: 32767, 2: 255, 3: 32767, 4: -32768, 5: 128, 16: 1, 17: -1}
	for _, e := range events {
		if e.Type == 3 && e.Value != expected[e.Code] {
			t.Fatalf("axis %#v", e)
		}
		if e.Type == 1 {
			v := int32(0)
			if e.Code == 0x130 || e.Code == 0x13b {
				v = 1
			}
			if e.Value != v {
				t.Fatalf("button %#v", e)
			}
		}
	}
	if events[18] != (gamepadEvent{}) {
		t.Fatal("missing SYN_REPORT")
	}
	for _, e := range gamepadEvents(gamepadState{}) {
		if e.Value != 0 {
			t.Fatalf("release retained %#v", e)
		}
	}
	for _, e := range gamepadEvents(gamepadState{Buttons: []string{"dpadUp", "dpadDown", "dpadLeft", "dpadRight"}}) {
		if e.Type == 3 && e.Code >= 16 && e.Value != 0 {
			t.Fatal("opposing dpad")
		}
	}
}
