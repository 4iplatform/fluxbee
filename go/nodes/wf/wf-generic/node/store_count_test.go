package node

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"sync"
	"sync/atomic"
	"testing"
)

// countingSQLite is SQLite's driver, counting the transactions of the stores opened through it
// (openCountingStore): each one begun, and each statement run outside one, which SQLite runs as a
// transaction of its own, with the locks of the store's journal and, for a write, its commit's sync
// to disk (FINDINGS A-333).
var countingSQLite struct {
	register     sync.Once
	transactions atomic.Int64
}

const countingDriverName = "sqlite-counting"

// openCountingStore opens the store at path through countingSQLite.
func openCountingStore(t *testing.T, path string) *Store {
	t.Helper()
	countingSQLite.register.Do(func() {
		probe, err := sql.Open("sqlite", ":memory:")
		if err != nil {
			t.Fatal(err)
		}
		sql.Register(countingDriverName, countingDriver{inner: probe.Driver()})
		_ = probe.Close()
	})
	db, err := sql.Open(countingDriverName, storeDSN(path))
	if err != nil {
		t.Fatal(err)
	}
	if err := ensureWFSchema(db); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return &Store{db: db}
}

type countingDriver struct{ inner driver.Driver }

func (d countingDriver) Open(name string) (driver.Conn, error) {
	conn, err := d.inner.Open(name)
	if err != nil {
		return nil, err
	}
	return &countingConn{inner: conn}, nil
}

// countingConn is one connection of countingSQLite; database/sql uses a connection from one
// goroutine at a time.
type countingConn struct {
	inner driver.Conn
	inTx  bool
}

func (c *countingConn) Prepare(query string) (driver.Stmt, error) { return c.inner.Prepare(query) }
func (c *countingConn) Close() error                              { return c.inner.Close() }
func (c *countingConn) Begin() (driver.Tx, error) {
	return c.BeginTx(context.Background(), driver.TxOptions{})
}

func (c *countingConn) BeginTx(ctx context.Context, opts driver.TxOptions) (driver.Tx, error) {
	tx, err := c.inner.(driver.ConnBeginTx).BeginTx(ctx, opts)
	if err != nil {
		return nil, err
	}
	countingSQLite.transactions.Add(1)
	c.inTx = true
	return countingTx{inner: tx, conn: c}, nil
}

func (c *countingConn) ExecContext(ctx context.Context, query string, args []driver.NamedValue) (driver.Result, error) {
	c.statement()
	return c.inner.(driver.ExecerContext).ExecContext(ctx, query, args)
}

func (c *countingConn) QueryContext(ctx context.Context, query string, args []driver.NamedValue) (driver.Rows, error) {
	c.statement()
	return c.inner.(driver.QueryerContext).QueryContext(ctx, query, args)
}

// statement counts a statement run outside a transaction as one.
func (c *countingConn) statement() {
	if !c.inTx {
		countingSQLite.transactions.Add(1)
	}
}

func (c *countingConn) ResetSession(ctx context.Context) error {
	if resetter, ok := c.inner.(driver.SessionResetter); ok {
		return resetter.ResetSession(ctx)
	}
	return nil
}

func (c *countingConn) IsValid() bool {
	if validator, ok := c.inner.(driver.Validator); ok {
		return validator.IsValid()
	}
	return true
}

type countingTx struct {
	inner driver.Tx
	conn  *countingConn
}

func (tx countingTx) Commit() error {
	tx.conn.inTx = false
	return tx.inner.Commit()
}

func (tx countingTx) Rollback() error {
	tx.conn.inTx = false
	return tx.inner.Rollback()
}
