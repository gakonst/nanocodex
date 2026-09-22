package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"math"
	"os"
	"sync"
	"time"
)

// Coordinates use screen convention: negative Y is up. Every packet is a full
// snapshot so a lost release cannot leave a button held after the next packet.
type gamepadState struct {
	LeftX        float64  `json:"leftX"`
	LeftY        float64  `json:"leftY"`
	RightX       float64  `json:"rightX"`
	RightY       float64  `json:"rightY"`
	LeftTrigger  float64  `json:"leftTrigger"`
	RightTrigger float64  `json:"rightTrigger"`
	Buttons      []string `json:"buttons"`
}

var gamepadButtons = []string{"a", "b", "x", "y", "dpadUp", "dpadDown", "dpadLeft", "dpadRight", "leftShoulder", "rightShoulder", "leftStick", "rightStick", "back", "start"}

func (s *gamepadState) UnmarshalJSON(data []byte) error {
	// Decode keys explicitly: encoding/json otherwise accepts duplicate and
	// case-insensitive struct fields, which make a full snapshot ambiguous.
	fields := make(map[string]json.RawMessage)
	keys := json.NewDecoder(bytes.NewReader(data))
	if token, err := keys.Token(); err != nil || token != json.Delim('{') {
		return errors.New("invalid gamepad snapshot")
	}
	for keys.More() {
		token, err := keys.Token()
		if err != nil {
			return err
		}
		key, ok := token.(string)
		if !ok {
			return errors.New("invalid gamepad field")
		}
		if _, duplicate := fields[key]; duplicate {
			return errors.New("duplicate gamepad field")
		}
		var value json.RawMessage
		if err := keys.Decode(&value); err != nil {
			return err
		}
		fields[key] = value
	}
	if _, err := keys.Token(); err != nil {
		return err
	}
	if len(fields) != 7 {
		return errors.New("invalid gamepad snapshot fields")
	}
	for _, key := range []string{"leftX", "leftY", "rightX", "rightY", "leftTrigger", "rightTrigger", "buttons"} {
		if value, ok := fields[key]; !ok || bytes.Equal(bytes.TrimSpace(value), []byte("null")) {
			return errors.New("incomplete gamepad snapshot")
		}
	}
	type plain gamepadState
	var value plain
	d := json.NewDecoder(bytes.NewReader(data))
	d.DisallowUnknownFields()
	if err := d.Decode(&value); err != nil {
		return err
	}
	*s = gamepadState(value)
	return s.validate()
}
func (s gamepadState) validate() error {
	for i, v := range []float64{s.LeftX, s.LeftY, s.RightX, s.RightY, s.LeftTrigger, s.RightTrigger} {
		if math.IsNaN(v) || math.IsInf(v, 0) || v > 1 || (i < 4 && v < -1) || (i >= 4 && v < 0) {
			return errors.New("invalid gamepad axis")
		}
	}
	seen := map[string]bool{}
	for _, button := range s.Buttons {
		valid := false
		for _, allowed := range gamepadButtons {
			if button == allowed {
				valid = true
				break
			}
		}
		if !valid || seen[button] {
			return errors.New("invalid gamepad button")
		}
		seen[button] = true
	}
	return nil
}

type gamepadDevice interface {
	writeState(gamepadState) error
	close() error
}

// The watchdog bounds held movement when a reliable channel stalls before its
// control lease expires. Viewers must refresh held state at least every 250ms.
type gamepadController struct {
	mu         sync.Mutex
	device     gamepadDevice
	timer      *time.Timer
	closed     bool
	failure    error
	generation uint64
}

const gamepadIdleTimeout = 500 * time.Millisecond

func newGamepadController(device gamepadDevice) *gamepadController {
	return &gamepadController{device: device}
}
func (g *gamepadController) apply(s gamepadState) error {
	if err := s.validate(); err != nil {
		return err
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.closed {
		return errors.New("gamepad closed")
	}
	if g.failure != nil {
		return g.failure
	}
	if err := g.device.writeState(s); err != nil {
		g.failure = err
		_ = g.device.close()
		g.closed = true
		return err
	}
	if g.timer != nil {
		g.timer.Stop()
	}
	// A generation prevents a timer already waiting on the mutex from releasing
	// a newer snapshot. time.AfterFunc.Stop alone cannot provide that guarantee.
	g.generation++
	generation := g.generation
	g.timer = time.AfterFunc(gamepadIdleTimeout, func() { g.expire(generation) })
	return nil
}
func (g *gamepadController) expire(generation uint64) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if !g.closed && generation == g.generation {
		g.failure = g.device.writeState(gamepadState{})
		if g.failure != nil {
			_ = g.device.close()
			g.closed = true
		}
	}
}
func (g *gamepadController) release() error {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.timer != nil {
		g.timer.Stop()
	}
	g.generation++
	if g.closed {
		return g.failure
	}
	err := g.device.writeState(gamepadState{})
	if err != nil {
		g.failure = err
		_ = g.device.close()
		g.closed = true
	}
	return err
}
func (g *gamepadController) close() {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.timer != nil {
		g.timer.Stop()
	}
	if !g.closed {
		_ = g.device.writeState(gamepadState{})
		_ = g.device.close()
		g.closed = true
	}
}
func (g *gamepadController) available() bool {
	if g == nil {
		return false
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	return !g.closed && g.failure == nil
}

// Device creation is opt-in even when this process can access uinput.
func configuredGamepad(open func() (gamepadDevice, error)) (*gamepadController, error) {
	if os.Getenv("NANOCODEX_VIRTUAL_GAMEPAD") != "1" {
		return nil, nil
	}
	device, err := open()
	if err != nil {
		return nil, err
	}
	if device == nil {
		return nil, errors.New("gamepad opener returned no device")
	}
	return newGamepadController(device), nil
}
