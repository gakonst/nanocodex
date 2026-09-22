package main

import (
	"context"
	"errors"
	"sync/atomic"
	"testing"
	"time"
)

func TestICEPreparationPrefetchSingleFlightAndReuse(t *testing.T) {
	owner, cancel := context.WithCancel(context.Background())
	defer cancel()
	started := make(chan struct{})
	release := make(chan struct{})
	var requests atomic.Int32
	p := newICEPreparation(owner, func(ctx context.Context) (string, error) {
		if requests.Add(1) == 1 {
			close(started)
		}
		select {
		case <-release:
			return "synthetic credential", nil
		case <-ctx.Done():
			return "", ctx.Err()
		}
	})
	p.prefetch()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("prefetch not started")
	}
	// Repeated preparation while the real fetch blocks must share the call.
	for i := 0; i < 20; i++ {
		p.prefetch()
	}
	viewer, stop := context.WithCancel(context.Background())
	stop()
	if _, err := p.get(viewer); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled waiter: %v", err)
	}
	results := make(chan error, 20)
	for i := 0; i < 20; i++ {
		go func() {
			value, err := p.get(owner)
			if err == nil && value != "synthetic credential" {
				err = errors.New("wrong credential")
			}
			results <- err
		}()
	}
	close(release)
	for i := 0; i < 20; i++ {
		select {
		case err := <-results:
			if err != nil {
				t.Fatal(err)
			}
		case <-time.After(time.Second):
			t.Fatal("waiter blocked")
		}
	}
	if _, err := p.get(owner); err != nil {
		t.Fatal(err)
	}
	if got := requests.Load(); got != 1 {
		t.Fatalf("fetches = %d, want 1", got)
	}
}

func TestICEPreparationOwnerCancellation(t *testing.T) {
	owner, cancel := context.WithCancel(context.Background())
	defer cancel()
	started := make(chan struct{})
	finished := make(chan struct{})
	p := newICEPreparation(owner, func(ctx context.Context) (int, error) {
		close(started)
		<-ctx.Done()
		close(finished)
		return 99, nil // Even an uncooperative success must not be retained.
	})
	p.prefetch()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("fetch did not start")
	}
	p.mu.Lock()
	call := p.call
	p.mu.Unlock()
	cancel()
	select {
	case <-finished:
	case <-time.After(time.Second):
		t.Fatal("owner did not cancel fetch")
	}
	if _, err := p.get(context.Background()); !errors.Is(err, context.Canceled) {
		t.Fatalf("owner cancellation: %v", err)
	}
	select {
	case <-call.done:
	case <-time.After(time.Second):
		t.Fatal("cancelled preparation did not finish")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.call != nil || call.value != 0 || !errors.Is(call.err, context.Canceled) {
		t.Fatal("cancelled fetch retained credentials")
	}
}

func TestICEPreparationFailureIsNotCached(t *testing.T) {
	var requests atomic.Int32
	failure := errors.New("synthetic failure")
	p := newICEPreparation(context.Background(), func(context.Context) (int, error) {
		if requests.Add(1) == 1 {
			return 99, failure
		}
		return 42, nil
	})
	if value, err := p.get(context.Background()); value != 0 || !errors.Is(err, failure) {
		t.Fatalf("failed value = %d, error = %v", value, err)
	}
	if value, err := p.get(context.Background()); value != 42 || err != nil {
		t.Fatalf("retry value = %d, error = %v", value, err)
	}
	if requests.Load() != 2 {
		t.Fatal("failure was cached")
	}
}

func TestICEPreparationExpiryAndSessionIsolation(t *testing.T) {
	var requests atomic.Int32
	var elapsed atomic.Int64
	start := time.Now()
	fetch := func(context.Context) (int32, error) { return requests.Add(1), nil }
	p := newICEPreparation(context.Background(), fetch)
	p.now = func() time.Time { return start.Add(time.Duration(elapsed.Load())) }
	first, err := p.get(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	elapsed.Store(int64(icePreparationTTL - time.Nanosecond))
	if value, err := p.get(context.Background()); value != first || err != nil {
		t.Fatalf("unexpired = %d, %v", value, err)
	}
	elapsed.Store(int64(icePreparationTTL))
	if value, err := p.get(context.Background()); value == first || err != nil {
		t.Fatalf("expired = %d, %v", value, err)
	}
	other := newICEPreparation(context.Background(), fetch)
	if value, err := other.get(context.Background()); value != 3 || err != nil {
		t.Fatalf("new session = %d, %v", value, err)
	}
}

func TestICEPreparationRejectsExpiredFetch(t *testing.T) {
	var elapsed atomic.Int64
	start := time.Now()
	p := newICEPreparation(context.Background(), func(context.Context) (int, error) {
		elapsed.Store(int64(icePreparationTTL))
		return 42, nil
	})
	p.now = func() time.Time { return start.Add(time.Duration(elapsed.Load())) }
	if value, err := p.get(context.Background()); value != 0 || !errors.Is(err, errICEPreparationExpired) {
		t.Fatalf("expired fetch = %d, %v", value, err)
	}
}

func TestICEPreparationActiveWaiterCancellation(t *testing.T) {
	owner, cancelOwner := context.WithCancel(context.Background())
	defer cancelOwner()
	started := make(chan struct{})
	release := make(chan struct{})
	p := newICEPreparation(owner, func(ctx context.Context) (int, error) {
		deadline, ok := ctx.Deadline()
		if !ok || time.Until(deadline) > icePreparationTimeout {
			return 0, errors.New("unbounded fetch")
		}
		close(started)
		select {
		case <-release:
			return 42, nil
		case <-ctx.Done():
			return 0, ctx.Err()
		}
	})
	viewer, cancelViewer := context.WithCancel(owner)
	defer cancelViewer()
	result := make(chan error, 1)
	go func() { _, err := p.get(viewer); result <- err }()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("fetch did not start")
	}
	cancelViewer()
	select {
	case err := <-result:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("waiter cancellation: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("cancelled waiter blocked")
	}
	close(release)
	if value, err := p.get(owner); value != 42 || err != nil {
		t.Fatalf("remaining viewer = %d, %v", value, err)
	}
	cancelOwner()
	if _, err := p.get(context.Background()); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled cached owner: %v", err)
	}
}
