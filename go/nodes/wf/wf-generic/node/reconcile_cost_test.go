package node

import (
	"context"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// FINDINGS A-333 — a reconcile reads the timer index once, and makes what it writes of the rows it
// passes in one commit: 3,000 timers of 300 instances, held by SY.timer, held and each refused in
// the index, or lost by SY.timer, take one or two transactions on the store. Before, it read each
// row twice (again before it acted on it, and again to settle a timer SY.timer holds) and wrote each
// in a commit of its own: 6,001 transactions for 3,000 timers held, which took 3.6 s on worker1, and
// 9,001 with their refusals.
func TestAReconcileReadsTheIndexOnceAndWritesItInOneCommit(t *testing.T) {
	const instances, keys = 300, 10
	lostRefusal := "TIMER_LIST: SY.timer@motherbee does not have it"
	for _, tc := range []struct {
		name string
		// refused: every timer of the index is refused; lost: SY.timer lists none of them.
		refused, lost bool
		transactions  int64
		refusedAfter  int
		logged        string
	}{
		{name: "held by SY.timer", transactions: 1},
		{name: "held by SY.timer, each refused in the index", refused: true, transactions: 2,
			logged: "taken by SY.timer, sent again 0 time(s) after an earlier refusal"},
		{name: "lost by SY.timer", lost: true, transactions: 2, refusedAfter: instances * keys,
			logged: "missing from SY.timer (fire_at still future): it goes again from the loop"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			path := filepath.Join(t.TempDir(), "wf.db")
			timer := &pagedSYTimer{t: t, frame: wfTimersAPage}
			rt, _ := newTestRuntime(t, path, validWorkflowJSON(), timer)
			pending := pendingTimers(t, rt, instances, keys, fixedClock().Add(time.Hour).UnixMilli())
			if !tc.lost {
				timer.pending = pending
			}
			if tc.refused {
				if _, err := rt.Store.db.ExecContext(ctx, `UPDATE wf_instance_timers SET refusal = 'an earlier refusal', resend_at_ms = 1;`); err != nil {
					t.Fatal(err)
				}
			}
			store := openCountingStore(t, path)
			rt.Store, rt.ActCtx.Store = store, store
			if err := rt.loadRunningInstances(ctx); err != nil {
				t.Fatal(err)
			}
			listing, err := rt.listTimers(ctx, recoverOpts)
			if err != nil || !listing.complete {
				t.Fatalf("the listing: complete %v, %v", listing.complete, err)
			}
			before := countingSQLite.transactions.Load()
			logged := nodeLog(t, func() {
				if err := rt.reconcileTimers(ctx, listing, recoverOpts, nowMS(rt.ActCtx.Clock)); err != nil {
					t.Fatal(err)
				}
			})
			if made := countingSQLite.transactions.Load() - before; made != tc.transactions {
				t.Fatalf("the reconcile of %d timers made %d transactions on the store, want %d", instances*keys, made, tc.transactions)
			}
			rows, err := rt.Store.ListAllExpectedTimers(ctx)
			if err != nil || len(rows) != instances*keys {
				t.Fatalf("the index keeps its %d timers: %d (%v)", instances*keys, len(rows), err)
			}
			refused := 0
			for _, row := range rows {
				switch row.Refusal {
				case "":
				case lostRefusal:
					refused++
				default:
					t.Fatalf("timer %s/%s still refused: %q", row.InstanceID, row.TimerKey, row.Refusal)
				}
			}
			if refused != tc.refusedAfter {
				t.Fatalf("%d timers marked to go again, want %d", refused, tc.refusedAfter)
			}
			if tc.logged != "" && strings.Count(logged, tc.logged) != instances*keys {
				t.Fatalf("the log says %q of %d timers, want each of the %d:\n%s", tc.logged, strings.Count(logged, tc.logged), instances*keys,
					logged[:min(len(logged), 2000)])
			}
			if !strings.Contains(logged, "recover: timer reconciliation complete") {
				t.Fatalf("the reconcile is not complete:\n%s", logged[:min(len(logged), 2000)])
			}
		})
	}
}

// refireWorkflowJSON schedules the deadline again (2h) when its reminder fires.
const refireWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "refire",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "waiting",
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'remind'", "target_state": "waiting",
         "actions": [{"type": "schedule_timer", "timer_key": "deadline", "fire_in": "2h"}]}
      ]
    },
    {"name": "done"}
  ]
}`

// FINDINGS A-333 — the writes a reconcile holds are made before it gives a fire, whose transition may
// read or change its instance's rows. The reconcile marks the deadline, which SY.timer lost, to go
// again; then it fires the reminder, which SY.timer holds past its time (a move it did not take),
// and the reminder's transition schedules the deadline again, a new request whose own answer
// decides: the deadline is not refused. Made at the reconcile's end, the mark would land on the new
// schedule, which the loop would send again, the node DEGRADED until SY.timer answered
// already_existed.
func TestAReconcileMakesItsWritesBeforeAFire(t *testing.T) {
	const instanceID = "wfi:refire"
	at := func(d time.Duration) int64 { return fixedClock().Add(d).UnixMilli() }
	remind := timerInfo(timerClientRef(instanceID, "remind"), at(-time.Second))
	timer := &mockTimerLister{timerList: []sdk.TimerInfo{remind}}
	rt, _ := newTestRuntime(t, "", refireWorkflowJSON, timer)
	ctx := context.Background()
	putRunningInstance(t, rt, instanceID, "waiting")
	for key, fireAt := range map[string]int64{"deadline": at(30 * time.Minute), "remind": at(40 * time.Minute)} {
		if err := rt.Store.RegisterTimer(ctx, instanceID, key, 1000, fireAt); err != nil {
			t.Fatal(err)
		}
	}
	if err := rt.Recover(ctx, recoverOpts); err != nil {
		t.Fatal(err)
	}
	if _, kept, _ := rt.Store.GetTimerForInstance(ctx, instanceID, "remind"); kept || !slices.Contains(timer.cancelled, *remind.ClientRef) {
		t.Fatalf("the reminder fired: its row gone (kept %v), and the one SY.timer held cancelled (%v)", kept, timer.cancelled)
	}
	deadline := timerClientRef(instanceID, "deadline")
	if len(timer.scheduled) != 1 || timer.scheduled[0] != deadline {
		t.Fatalf("the reminder's transition scheduled the deadline again: %v", timer.scheduled)
	}
	row, kept, err := rt.Store.GetTimerForInstance(ctx, instanceID, "deadline")
	if err != nil || !kept || row.FireAtMS != at(2*time.Hour) || row.Refusal != "" {
		t.Fatalf("the deadline, scheduled again by the fire and not refused: %+v (kept %v, %v)", row, kept, err)
	}
	if rt.timersPending != nil {
		t.Fatalf("the timers are not reconciled: %v", rt.timersPending)
	}
}

// FINDINGS A-333 — a commit of the reconcile's writes that fails leaves those rows as they were, and
// the reconcile says the timers are not reconciled: the node runs DEGRADED, and the loop reconciles
// them again until the store takes the writes. Before, a mark that failed was logged and the
// reconcile was complete: the timer SY.timer lacked went again only at the node's next start.
func TestAReconcileWhoseWritesFailIsNotDone(t *testing.T) {
	const instanceID, key = "wfi:lost", "invoice_timeout"
	rt, _ := newTestRuntime(t, "", validWorkflowJSON(), &mockTimerLister{})
	ctx := context.Background()
	putRunningInstance(t, rt, instanceID, "collecting_data")
	if err := rt.Store.RegisterTimer(ctx, instanceID, key, 1000, fixedClock().Add(time.Hour).UnixMilli()); err != nil {
		t.Fatal(err)
	}
	if _, err := rt.Store.db.ExecContext(ctx, `
CREATE TRIGGER wf_timers_fail BEFORE UPDATE ON wf_instance_timers BEGIN SELECT RAISE(ABORT, 'the disk failed'); END;`); err != nil {
		t.Fatal(err)
	}
	logged := nodeLog(t, func() {
		if err := rt.Recover(ctx, recoverOpts); err != nil {
			t.Fatal(err)
		}
	})
	if rt.timersPending == nil || !strings.Contains(rt.timersPending.Error(), "the disk failed") {
		t.Fatalf("the timers are reconciled, though their writes failed: %v", rt.timersPending)
	}
	if degraded := rt.degradedBy(ctx); degraded == nil || degraded.Code != errCodeTimersNotReconciled {
		t.Fatalf("the node is not DEGRADED: %+v", degraded)
	}
	if !strings.Contains(logged, "1 write(s) to the timer index not made, those timers stay as they were") ||
		strings.Contains(logged, "it goes again from the loop") {
		t.Fatalf("the log:\n%s", logged)
	}
	if row, _, _ := rt.Store.GetTimerForInstance(ctx, instanceID, key); row.Refusal != "" {
		t.Fatalf("the row changed: %+v", row)
	}

	// The store takes writes again: the loop's next reconcile marks the timer, and the node is
	// reconciled.
	if _, err := rt.Store.db.ExecContext(ctx, `DROP TRIGGER wf_timers_fail;`); err != nil {
		t.Fatal(err)
	}
	rt.takeTimerList(ctx, timerListResult{listing: wholeList(), askedAtMS: nowMS(rt.ActCtx.Clock)}, recoverOpts)
	if rt.timersPending != nil {
		t.Fatalf("the timers are not reconciled: %v", rt.timersPending)
	}
	if row, _, _ := rt.Store.GetTimerForInstance(ctx, instanceID, key); row.Refusal != "TIMER_LIST: SY.timer@motherbee does not have it" {
		t.Fatalf("the timer SY.timer lacks is not marked to go again: %+v", row)
	}
}
