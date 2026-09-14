package main

import (
	"testing"
	"time"
)

func TestControlSequenceAcrossIndependentChannels(t *testing.T) {
	now := time.Now()
	lease := controlLease{}
	generation := lease.acquire("first", now)
	move := remoteInput{Kind: "move", Sequence: 20, Generation: generation}
	if accepted, err := lease.accept("first", move, now); !accepted || err != nil {
		t.Fatal("initial motion", accepted, err)
	}
	// A reliable key-up can arrive after a newer motion datagram.
	keyUp := remoteInput{Kind: "key", Sequence: 19, Generation: generation}
	if accepted, err := lease.accept("first", keyUp, now); !accepted || err != nil {
		t.Fatal("key-up was lost behind motion", accepted, err)
	}
	click := remoteInput{Kind: "button", Sequence: 22, Generation: generation}
	if accepted, err := lease.accept("first", click, now); !accepted || err != nil {
		t.Fatal(accepted, err)
	}
	move.Sequence = 21
	if accepted, err := lease.accept("first", move, now); accepted || err != nil {
		t.Fatal("late motion crossed the click barrier", accepted, err)
	}
	if accepted, err := lease.accept("second", click, now); accepted || err == nil {
		t.Fatal("another viewer injected input")
	}
}

func TestControlExpiryAndGenerationFencing(t *testing.T) {
	now := time.Now()
	lease := controlLease{}
	old := lease.acquire("viewer", now)
	if err := lease.renew("viewer", old, now.Add(10*time.Second)); err == nil {
		t.Fatal("expired lease was revived")
	}
	lease.acquire("viewer", now.Add(11*time.Second))
	if accepted, err := lease.accept("viewer", remoteInput{Kind: "key", Sequence: 1, Generation: old}, now.Add(12*time.Second)); accepted || err == nil {
		t.Fatal("stale generation accepted")
	}
}
