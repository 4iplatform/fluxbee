package node

import (
	"context"
	"log"
	"time"
)

// StartGC launches a background goroutine that periodically deletes terminated
// instances older than retentionDays. It runs every interval until ctx is done.
func StartGC(ctx context.Context, store *Store, retentionDays int, interval time.Duration, clock ClockFunc) {
	if retentionDays <= 0 {
		retentionDays = 7
	}
	if interval <= 0 {
		interval = time.Hour
	}
	go func() {
		for {
			select {
			case <-ctx.Done():
				return
			case <-time.After(interval):
				runGC(ctx, store, retentionDays, clock)
			}
		}
	}()
}

func runGC(ctx context.Context, store *Store, retentionDays int, clock ClockFunc) {
	cutoff := clock().Add(-time.Duration(retentionDays) * 24 * time.Hour).UnixMilli()
	n, err := store.DeleteTerminatedBefore(ctx, cutoff)
	if err != nil {
		log.Printf("wf gc: error: %v", err)
		return
	}
	if n > 0 {
		log.Printf("wf gc: deleted %d terminated instances older than %d days", n, retentionDays)
	}
}
