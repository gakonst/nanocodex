//go:build linux

package main

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"math"
	"os"

	"golang.org/x/sys/unix"
)

// Linux uinput ABI, from linux/uinput.h and linux/input-event-codes.h.
// A named virtual Xbox-layout controller; no physical device is opened.
const (
	uiSetEVBit   = 0x40045564
	uiSetKeyBit  = 0x40045565
	uiSetAbsBit  = 0x40045567
	uiDevCreate  = 0x5501
	uiDevDestroy = 0x5502
)

var gamepadKeyCodes = map[string]uint16{"a": 0x130, "b": 0x131, "x": 0x133, "y": 0x134, "leftShoulder": 0x136, "rightShoulder": 0x137, "back": 0x13a, "start": 0x13b, "leftStick": 0x13d, "rightStick": 0x13e}
var gamepadAxes = []uint16{0, 1, 2, 3, 4, 5, 16, 17}

type linuxGamepad struct{ file *os.File }

func openGamepad() (gamepadDevice, error) {
	f, err := os.OpenFile("/dev/uinput", os.O_WRONLY|unix.O_NONBLOCK, 0)
	if err != nil {
		return nil, fmt.Errorf("native gamepad requires authorized /dev/uinput access: %w", err)
	}
	ready := false
	defer func() {
		if !ready {
			_ = f.Close()
		}
	}()
	set := func(request uint, value int) error { return unix.IoctlSetInt(int(f.Fd()), request, value) }
	for _, t := range []int{1, 3} {
		if err = set(uiSetEVBit, t); err != nil {
			return nil, err
		}
	}
	for _, code := range gamepadKeyCodes {
		if err = set(uiSetKeyBit, int(code)); err != nil {
			return nil, err
		}
	}
	for _, code := range gamepadAxes {
		if err = set(uiSetAbsBit, int(code)); err != nil {
			return nil, err
		}
	}
	// The stable write-based setup ABI works on old and new uinput kernels.
	var setup struct {
		Name                          [80]byte
		Bus, Vendor, Product, Version uint16
		FFEffectsMax                  uint32
		Max, Min, Fuzz, Flat          [64]int32
	}
	copy(setup.Name[:], "Nanocodex Virtual Xbox Controller")
	setup.Bus, setup.Vendor, setup.Product, setup.Version = 3, 0x045e, 0x028e, 1
	for _, axis := range gamepadAxes {
		if axis == 2 || axis == 5 {
			setup.Max[axis] = 255
		} else if axis >= 16 {
			setup.Min[axis], setup.Max[axis] = -1, 1
		} else {
			setup.Min[axis], setup.Max[axis] = -32768, 32767
			setup.Flat[axis] = 2048
		}
	}
	if err = binary.Write(f, binary.NativeEndian, &setup); err != nil {
		return nil, err
	}
	if err = set(uiDevCreate, 0); err != nil {
		return nil, err
	}
	device := &linuxGamepad{file: f}
	if err = device.writeState(gamepadState{}); err != nil {
		_ = device.close()
		return nil, err
	}
	ready = true
	return device, nil
}

type gamepadEvent struct {
	Type, Code uint16
	Value      int32
}

func gamepadEvents(s gamepadState) []gamepadEvent {
	held := map[string]bool{}
	for _, b := range s.Buttons {
		held[b] = true
	}
	result := make([]gamepadEvent, 0, 19)
	for _, b := range gamepadButtons {
		if code, ok := gamepadKeyCodes[b]; ok {
			value := int32(0)
			if held[b] {
				value = 1
			}
			result = append(result, gamepadEvent{1, code, value})
		}
	}
	stick := func(v float64) int32 {
		if v < 0 {
			return int32(math.Round(v * 32768))
		}
		return int32(math.Round(v * 32767))
	}
	hat := func(negative, positive string) int32 {
		var v int32
		if held[negative] {
			v--
		}
		if held[positive] {
			v++
		}
		return v
	}
	values := []int32{stick(s.LeftX), stick(s.LeftY), int32(math.Round(s.LeftTrigger * 255)), stick(s.RightX), stick(s.RightY), int32(math.Round(s.RightTrigger * 255)), hat("dpadLeft", "dpadRight"), hat("dpadUp", "dpadDown")}
	for i, code := range gamepadAxes {
		result = append(result, gamepadEvent{3, code, values[i]})
	}
	return append(result, gamepadEvent{0, 0, 0}) // SYN_REPORT commits the frame.
}
func (d *linuxGamepad) writeState(s gamepadState) error {
	var out bytes.Buffer
	for _, event := range gamepadEvents(s) {
		// timeval uses the native kernel word size on supported Linux targets.
		_ = binary.Write(&out, binary.NativeEndian, unix.Timeval{})
		_ = binary.Write(&out, binary.NativeEndian, event)
	}
	data := out.Bytes()
	n, err := d.file.Write(data)
	if err == nil && n != len(data) {
		err = io.ErrShortWrite
	}
	return err
}
func (d *linuxGamepad) close() error {
	_ = unix.IoctlSetInt(int(d.file.Fd()), uiDevDestroy, 0)
	return d.file.Close()
}
