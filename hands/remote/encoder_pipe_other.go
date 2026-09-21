//go:build !linux

package main

import "os"

func prepareScreenEncoderPipe(_ *os.File) {}
