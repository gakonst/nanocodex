package main

import "context"

// A single worker preserves request order while process shutdown waits outside
// the host's input/lease loop. The worker owns final broadcast cleanup too.
type hostBroadcaster interface {
	start(context.Context, string, string, string, int, int) broadcastResult
	stop()
	status() broadcastResult
}

const hostBroadcastQueueSize = 8

type hostBroadcast struct {
	ctx      context.Context
	cancel   context.CancelFunc
	requests chan remoteMessage
	done     chan struct{}
}

func newHostBroadcast(parent context.Context, backend hostBroadcaster, config hostConfig, emit func(hostEvent)) *hostBroadcast {
	ctx, cancel := context.WithCancel(parent)
	worker := &hostBroadcast{ctx: ctx, cancel: cancel, requests: make(chan remoteMessage, hostBroadcastQueueSize), done: make(chan struct{})}
	go func() {
		defer close(worker.done)
		defer backend.stop()
		for {
			select {
			case <-ctx.Done():
				return
			case request := <-worker.requests:
				// Cancellation must not drain queued starts after authorization ends.
				if ctx.Err() != nil {
					return
				}
				var result broadcastResult
				switch request.Action {
				case "start":
					result = backend.start(ctx, config.Waymote, request.URL, request.Preset, config.Width, config.Height)
				case "stop":
					backend.stop()
					result = backend.status()
				case "status":
					result = backend.status()
				}
				if ctx.Err() != nil {
					return
				}
				emit(hostEvent{broadcast: &remoteMessage{Type: "broadcast_result", ViewerID: request.ViewerID, RequestID: request.RequestID, BroadcastResult: &result}})
			}
		}
	}()
	return worker
}

func (worker *hostBroadcast) enqueue(request remoteMessage) bool {
	if worker.ctx.Err() != nil {
		return false
	}
	select {
	case <-worker.ctx.Done():
		return false
	case worker.requests <- request:
		return true
	default:
		return false
	}
}

func (worker *hostBroadcast) close() {
	worker.cancel()
	<-worker.done
}
