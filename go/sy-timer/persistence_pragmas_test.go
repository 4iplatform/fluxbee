package main

import (
	"context"
	"path/filepath"
	"testing"
)

// The scheduler writes while requests are served, so the pool holds several connections: every one
// of them must wait for the lock (busy_timeout), run in WAL and have the 16 MiB page cache
// (cache_size, since 1.7, FINDINGS A-326: SQLite's default is 2,000 KiB, which a large
// TIMER_PURGE_OWNER outgrows), not just the first.
func TestEveryConnectionGetsThePragmas(t *testing.T) {
	db, _, err := openTimerDB(filepath.Join(t.TempDir(), "timers.db"))
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
		// A negative cache_size is the cache in KiB, whatever the page size.
		var cache int
		if err := conn.QueryRowContext(ctx, "PRAGMA cache_size;").Scan(&cache); err != nil {
			t.Fatalf("conn %d cache_size: %v", i, err)
		}
		if cache != -16384 {
			t.Fatalf("conn %d: cache_size=%d, want -16384 (16 MiB)", i, cache)
		}
	}
}
