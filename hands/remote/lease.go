package main

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"time"
)

type controlLease struct {
	owner, generation string
	deadline          time.Time
	motion, discrete  uint64
}

func (lease *controlLease) expired(now time.Time) bool {
	return lease.owner != "" && !now.Before(lease.deadline)
}
func (lease *controlLease) acquire(owner string, now time.Time) string {
	var nonce [16]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		panic(err)
	}
	*lease = controlLease{owner: owner, generation: hex.EncodeToString(nonce[:]), deadline: now.Add(10 * time.Second)}
	return lease.generation
}
func (lease *controlLease) renew(owner, generation string, now time.Time) error {
	if owner != lease.owner || generation != lease.generation || lease.expired(now) || lease.owner == "" {
		return errors.New("control lease expired")
	}
	lease.deadline = now.Add(10 * time.Second)
	return nil
}
func (lease *controlLease) accept(owner string, event remoteInput, now time.Time) (bool, error) {
	if owner != lease.owner || event.Generation != lease.generation || lease.expired(now) || lease.owner == "" {
		return false, errors.New("input is not authorized")
	}
	if event.Kind == "move" {
		if event.Sequence <= max(lease.motion, lease.discrete) {
			return false, nil
		}
		lease.motion = event.Sequence
	} else {
		if event.Sequence <= lease.discrete {
			return false, nil
		}
		lease.discrete = event.Sequence
	}
	return true, nil
}
