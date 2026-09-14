package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"math"
	"strings"
)

type remoteInput struct {
	Kind       string   `json:"kind"`
	Sequence   uint64   `json:"sequence"`
	Generation string   `json:"generation"`
	X          *float64 `json:"x,omitempty"`
	Y          *float64 `json:"y,omitempty"`
	Button     *int     `json:"button,omitempty"`
	Down       *bool    `json:"down,omitempty"`
	Key        *uint16  `json:"key,omitempty"`
	Text       *string  `json:"text,omitempty"`
	DeltaX     *float64 `json:"deltaX,omitempty"`
	DeltaY     *float64 `json:"deltaY,omitempty"`
}

func decodeInput(data []byte) (remoteInput, error) {
	var event remoteInput
	if len(data) > 8192 {
		return event, errors.New("input too large")
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&event); err != nil {
		return event, err
	}
	if err := decoder.Decode(new(any)); err != io.EOF {
		return event, errors.New("trailing input")
	}
	return event, event.validate()
}
func (event remoteInput) validate() error {
	invalid := errors.New("invalid remote input")
	if event.Sequence == 0 || event.Sequence > 9_007_199_254_740_991 || len(event.Generation) == 0 || len(event.Generation) > 128 {
		return invalid
	}
	for _, coordinate := range []*float64{event.X, event.Y} {
		if coordinate != nil && (math.IsNaN(*coordinate) || math.IsInf(*coordinate, 0) || *coordinate < 0 || *coordinate > 1) {
			return invalid
		}
	}
	point := event.X != nil && event.Y != nil
	noPoint := event.X == nil && event.Y == nil
	noDeltas := event.DeltaX == nil && event.DeltaY == nil
	valid := false
	switch event.Kind {
	case "move":
		valid = point && event.Button == nil && event.Down == nil && event.Key == nil && event.Text == nil && noDeltas
	case "button":
		valid = point && event.Button != nil && *event.Button >= 0 && *event.Button <= 2 && event.Down != nil && event.Key == nil && event.Text == nil && noDeltas
	case "scroll":
		valid = point && event.Button == nil && event.Down == nil && event.Key == nil && event.Text == nil && event.DeltaX != nil && event.DeltaY != nil
		if valid {
			for _, d := range []float64{*event.DeltaX, *event.DeltaY} {
				if math.IsNaN(d) || math.IsInf(d, 0) || math.Abs(d) > 4096 {
					valid = false
				}
			}
		}
	case "key":
		_, supported := hidToEvdev[valueOrZero(event.Key)]
		valid = supported && event.Key != nil && event.Down != nil && noPoint && event.Button == nil && event.Text == nil && noDeltas
	case "text":
		valid = event.Text != nil && len(*event.Text) > 0 && len(*event.Text) <= 4096 && !strings.ContainsRune(*event.Text, 0) && noPoint && event.Button == nil && event.Down == nil && event.Key == nil && noDeltas
	case "releaseAll":
		valid = noPoint && event.Button == nil && event.Down == nil && event.Key == nil && event.Text == nil && noDeltas
	}
	if !valid {
		return invalid
	}
	return nil
}
func valueOrZero(value *uint16) uint16 {
	if value == nil {
		return 0
	}
	return *value
}
