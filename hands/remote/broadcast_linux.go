//go:build linux

package main

import (
	"os/exec"
	"syscall"
)

// Waymote force-kills helpers during capture reconfiguration. Ensure their
// encoders cannot retain the old stream or playback monitor after that kill.
func configureBroadcastEncoder(command *exec.Cmd) {
	command.SysProcAttr = &syscall.SysProcAttr{Pdeathsig: syscall.SIGKILL}
}
