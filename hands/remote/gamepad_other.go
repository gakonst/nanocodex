//go:build !linux

package main

import "errors"

func openGamepad() (gamepadDevice, error) {
	return nil, errors.New("native virtual gamepad requires Linux uinput")
}
