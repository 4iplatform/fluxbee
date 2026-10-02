package node

import (
	"context"
	"path/filepath"
	"testing"
)

// Every pooled connection must wait for the lock (busy_timeout) and run with the same journal and
// sync settings, not just the first one.
func TestEveryStoreConnectionGetsThePragmas(t *testing.T) {
	store, err := OpenStore(filepath.Join(t.TempDir(), "wf.db"))
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer store.Close()

	ctx := context.Background()
	for i := 0; i < 3; i++ {
		conn, err := store.db.Conn(ctx) // held, so the pool must open a new one each time
		if err != nil {
			t.Fatalf("conn %d: %v", i, err)
		}
		defer conn.Close()
		var timeout, synchronous int
		var mode string
		if err := conn.QueryRowContext(ctx, "PRAGMA busy_timeout;").Scan(&timeout); err != nil {
			t.Fatalf("conn %d busy_timeout: %v", i, err)
		}
		if err := conn.QueryRowContext(ctx, "PRAGMA journal_mode;").Scan(&mode); err != nil {
			t.Fatalf("conn %d journal_mode: %v", i, err)
		}
		if err := conn.QueryRowContext(ctx, "PRAGMA synchronous;").Scan(&synchronous); err != nil {
			t.Fatalf("conn %d synchronous: %v", i, err)
		}
		// synchronous: 1 = NORMAL.
		if timeout != 5000 || mode != "wal" || synchronous != 1 {
			t.Fatalf("conn %d: busy_timeout=%d journal_mode=%s synchronous=%d", i, timeout, mode, synchronous)
		}
	}
}
