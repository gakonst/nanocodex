package main

import (
	"golang.org/x/sys/unix"
	"os"
)

// A 4K BGRA frame is 33 MB. Small pipes cause thousands of wakeups per frame.
// Keep the raw transport below one frame while amortizing those wakeups.
// A quota or unsupported descriptor must never prevent screen sharing.
func prepareScreenEncoderPipe(input *os.File) {
	_, _ = unix.FcntlInt(input.Fd(), unix.F_SETPIPE_SZ, 1024*1024)
}
