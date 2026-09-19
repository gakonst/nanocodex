//go:build !linux

package main

import "testing"

func TestGamepadUnsupportedPlatform(t *testing.T) {
	device, err := openGamepad()
	if device != nil || err == nil {
		t.Fatal("unsupported platform advertised device")
	}
}
