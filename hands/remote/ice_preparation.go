package main

import (
	"context"
	"errors"
	"sync"
	"time"
)

// Keep credentials only briefly, measured from request start rather than receipt.
const icePreparationTTL = 2 * time.Minute
const icePreparationTimeout = 10 * time.Second

var errICEPreparationExpired = errors.New("ICE preparation expired")

type icePreparationCall[T any] struct {
	done    chan struct{}
	value   T
	err     error
	expires time.Time
}

// icePreparation belongs to a single host session/principal. Its owner context
// cancels shared work; a viewer context cancels only that viewer's wait. T is
// treated as immutable, allowing focused tests without a media stack.
type icePreparation[T any] struct {
	owner context.Context
	fetch func(context.Context) (T, error)
	now   func() time.Time
	mu    sync.Mutex
	call  *icePreparationCall[T]
}

func newICEPreparation[T any](owner context.Context, fetch func(context.Context) (T, error)) *icePreparation[T] {
	return &icePreparation[T]{owner: owner, fetch: fetch, now: time.Now}
}

// prefetch never blocks the host event loop.
func (p *icePreparation[T]) prefetch() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.owner.Err() == nil {
		p.startLocked()
	}
}

func (p *icePreparation[T]) startLocked() *icePreparationCall[T] {
	if p.call != nil {
		select {
		case <-p.call.done:
			if p.call.err == nil && p.now().Before(p.call.expires) {
				return p.call
			}
		default:
			return p.call
		}
	}
	call := &icePreparationCall[T]{done: make(chan struct{}), expires: p.now().Add(icePreparationTTL)}
	p.call = call
	go func() {
		ctx, cancel := context.WithTimeout(p.owner, icePreparationTimeout)
		defer cancel()
		value, err := p.fetch(ctx)
		if err == nil {
			err = ctx.Err()
		}
		if err == nil && !p.now().Before(call.expires) {
			err = errICEPreparationExpired
		}
		p.mu.Lock()
		defer p.mu.Unlock()
		if err == nil {
			call.value = value
		}
		call.err = err
		close(call.done)
		if err != nil {
			p.call = nil
		}
	}()
	return call
}

func (p *icePreparation[T]) get(ctx context.Context) (T, error) {
	var zero T
	if err := ctx.Err(); err != nil {
		return zero, err
	}
	if err := p.owner.Err(); err != nil {
		return zero, err
	}
	p.mu.Lock()
	call := p.startLocked()
	p.mu.Unlock()
	select {
	case <-ctx.Done():
		return zero, ctx.Err()
	case <-p.owner.Done():
		return zero, p.owner.Err()
	case <-call.done:
		if err := ctx.Err(); err != nil {
			return zero, err
		}
		if err := p.owner.Err(); err != nil {
			return zero, err
		}
		if call.err != nil {
			return zero, call.err
		}
		if !p.now().Before(call.expires) {
			return zero, errICEPreparationExpired
		}
		return call.value, nil
	}
}
