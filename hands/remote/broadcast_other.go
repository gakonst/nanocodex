//go:build !linux

package main

import "os/exec"

func configureBroadcastEncoder(command *exec.Cmd) {}
