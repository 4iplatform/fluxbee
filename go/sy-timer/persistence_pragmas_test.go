package main

import (
	"context"
	"path/filepath"
	"testing"
)

// The scheduler writes while requests are served, so the pool holds several connections: every one
// of them must wait for the lock (busy_timeout) and run in WAL, not just the first.
func TestEveryConnectionGetsThePragmas(t *testing.T) {
	db, err := openTimerDB(filepath.Join(t.TempDir(), "timers.db"))
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer db.Close()

	ctx := context.Background()
	for i := 0; i < 3; i++ {
		conn, err := db.Conn(ctx) // held, so the pool must open a new one each time
		if err != nil {
			t.Fatalf("conn %d: %v", i, err)
		}
		defer conn.Close()
		var timeout int
		if err := conn.QueryRowContext(ctx, "PRAGMA busy_timeout;").Scan(&timeout); err != nil {
			t.Fatalf("conn %d busy_timeout: %v", i, err)
		}
		var mode string
		if err := conn.QueryRowContext(ctx, "PRAGMA journal_mode;").Scan(&mode); err != nil {
			t.Fatalf("conn %d journal_mode: %v", i, err)
		}
		if timeout != 5000 || mode != "wal" {
			t.Fatalf("conn %d: busy_timeout=%d journal_mode=%s, want 5000 and wal", i, timeout, mode)
		}
	}
}
