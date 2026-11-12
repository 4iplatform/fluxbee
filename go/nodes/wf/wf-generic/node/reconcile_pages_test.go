package node

import (
	"context"
	"fmt"
	"slices"
	"sort"
	"strconv"
	"strings"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// wfTimersAPage is how many of a WF's timers fit one page of SY.timer's TIMER_LIST: 96 KiB of timers
// (maxTimerListPageBytes, go/sy-timer), each about 430 bytes listed.
const wfTimersAPage = 228

// pagedSYTimer is SY.timer answering TIMER_LIST in pages, as respondTimerList does (go/sy-timer): its
// pending timers by fire time, then uuid, from the request's cursor on, frame of them at the most
// (what fits one frame) and the request's limit, and next_cursor, "<fire_at_utc_ms>:<uuid>" of the
// page's last timer, while timers follow. beforeV16 is SY.timer before 1.6 (FINDINGS A-266), which
// answers next_cursor only when the frame ended the page, never its limit. onPage, when set, runs
// before page n (from 1) is answered: timers fire, or are scheduled, meanwhile. odd, when set,
// changes page n before it goes: SY.timer answering what it never does.
type pagedSYTimer struct {
	mockTimerSender
	t         *testing.T
	pending   []sdk.TimerInfo
	frame     int
	beforeV16 bool
	onPage    func(n int)
	odd       func(n int, page sdk.TimerListPage) sdk.TimerListPage
	asked     []sdk.ListFilter
}

func (p *pagedSYTimer) ListPage(_ context.Context, filter sdk.ListFilter) (sdk.TimerListPage, error) {
	p.asked = append(p.asked, filter)
	n := len(p.asked)
	if n > 2*maxTimerListPages {
		p.t.Fatalf("the reconcile asked %d pages: its list does not end", n)
	}
	if p.onPage != nil {
		p.onPage(n)
	}
	sort.Slice(p.pending, func(i, j int) bool { return listedBefore(p.pending[i], p.pending[j]) })
	start := 0
	if filter.Cursor != "" {
		at := placeOf(p.t, filter.Cursor)
		start = sort.Search(len(p.pending), func(i int) bool { return listedBefore(at, p.pending[i]) })
	}
	limit := filter.Limit
	if limit <= 0 {
		limit = 100
	}
	read := min(limit, len(p.pending)-start)
	listed := min(read, p.frame)
	page := sdk.TimerListPage{Timers: append([]sdk.TimerInfo(nil), p.pending[start:start+listed]...)}
	if start+listed < len(p.pending) && (!p.beforeV16 || listed < read) {
		page.NextCursor = cursorOf(page.Timers[listed-1])
	}
	if p.odd != nil {
		page = p.odd(n, page)
	}
	return page, nil
}

// listedBefore reports whether SY.timer lists a before b: by fire time, then uuid.
func listedBefore(a, b sdk.TimerInfo) bool {
	if a.FireAtUTCMS != b.FireAtUTCMS {
		return a.FireAtUTCMS < b.FireAtUTCMS
	}
	return a.UUID < b.UUID
}

// cursorOf is SY.timer's next_cursor after timer: its place in the order.
func cursorOf(timer sdk.TimerInfo) string {
	return fmt.Sprintf("%d:%s", timer.FireAtUTCMS, timer.UUID)
}

// placeOf is the place a cursor names, as a timer at it.
func placeOf(t *testing.T, cursor string) sdk.TimerInfo {
	t.Helper()
	fireAt, uuid, ok := strings.Cut(cursor, ":")
	ms, err := strconv.ParseInt(fireAt, 10, 64)
	if !ok || err != nil {
		t.Fatalf("not a cursor of SY.timer's: %q", cursor)
	}
	return sdk.TimerInfo{FireAtUTCMS: ms, UUID: uuid}
}

// pendingTimers stores instances running instances of rt's current definition, each with keys timers
// in the index, the first at firstMS and each a second after the one before, in one transaction, and
// returns them as SY.timer lists them pending, by fire time (each at the index's time, as SY.timer
// holds a timer scheduled at a time).
func pendingTimers(t *testing.T, rt *NodeRuntime, instances, keys int, firstMS int64) []sdk.TimerInfo {
	t.Helper()
	ctx := context.Background()
	tx, err := rt.Store.db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	var timers []sdk.TimerInfo
	for i := range instances {
		id := fmt.Sprintf("wfi:paged-%05d", i)
		row := WFInstanceRow{InstanceID: id, ThreadID: id, WorkflowType: rt.Def.WorkflowType, DefinitionHash: rt.DefHash,
			Status: "running", CurrentState: "collecting_data", InputJSON: `{"customer_id":"c1"}`, StateJSON: `{}`,
			CreatedAtMS: 1000, UpdatedAtMS: 1000}
		if _, err := tx.ExecContext(ctx, `INSERT INTO wf_instances (`+instanceColumns+`) VALUES (?`+strings.Repeat(", ?", 14)+`);`,
			instanceValues(row)...); err != nil {
			t.Fatal(err)
		}
		for k := range keys {
			n := i*keys + k
			key := fmt.Sprintf("remind_%02d", k)
			fireAt := firstMS + int64(n)*1000
			if _, err := tx.ExecContext(ctx, `
INSERT INTO wf_instance_timers (instance_id, timer_key, scheduled_at_ms, fire_at_ms) VALUES (?, ?, ?, ?);`,
				id, key, 1000, fireAt); err != nil {
				t.Fatal(err)
			}
			timer := timerInfo(timerClientRef(id, key), fireAt)
			timer.OwnerL2Name, timer.TargetL2Name = testNodeName, testNodeName
			timer.UUID = fmt.Sprintf("%08d-0000-4000-8000-000000000000", n)
			timers = append(timers, timer)
		}
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	return timers
}

// without is timers but the one at i: a timer SY.timer lost.
func without(timers []sdk.TimerInfo, i int) []sdk.TimerInfo {
	return append(append([]sdk.TimerInfo(nil), timers[:i]...), timers[i+1:]...)
}

// takenAsMissing returns the timers of the index the reconcile took as missing, marked refused
// ("TIMER_LIST: SY.timer@motherbee does not have it"), by client_ref, and checks the rest: the node is
// not DEGRADED, the reconcile sent no schedule itself, the index keeps its indexed timers, and the
// loop's tick then sends those again and nothing else, nothing cancelled nor moved.
func takenAsMissing(t *testing.T, rt *NodeRuntime, timer *pagedSYTimer, indexed int) []string {
	t.Helper()
	ctx := context.Background()
	if rt.timersPending != nil {
		t.Fatalf("the timers are not reconciled: %v", rt.timersPending)
	}
	if len(timer.scheduled) != 0 {
		t.Fatalf("the reconcile sent %d schedules itself, not the loop: %v", len(timer.scheduled), timer.scheduled[:min(5, len(timer.scheduled))])
	}
	var missing []string
	rows, err := rt.Store.ListAllExpectedTimers(ctx)
	if err != nil || len(rows) != indexed {
		t.Fatalf("the index keeps its %d timers: %d (%v)", indexed, len(rows), err)
	}
	for _, row := range rows {
		if row.Refusal != "" {
			if want := "TIMER_LIST: SY.timer@motherbee does not have it"; row.Refusal != want {
				t.Fatalf("timer %s/%s refused as %q, want %q", row.InstanceID, row.TimerKey, row.Refusal, want)
			}
			missing = append(missing, timerClientRef(row.InstanceID, row.TimerKey))
		}
	}
	rt.tickTimerRequests(ctx)
	sentAgain := len(timer.scheduled) == min(len(missing), maxTimerRequestsPerTick)
	for _, ref := range timer.scheduled {
		sentAgain = sentAgain && slices.Contains(missing, ref)
	}
	if !sentAgain || len(timer.cancelled) != 0 || len(timer.rescheduled) != 0 {
		t.Fatalf("the loop's tick sends again what was taken as missing, %d timer(s), and nothing else: scheduled %v, cancelled %v, rescheduled %v",
			len(missing), timer.scheduled, timer.cancelled, timer.rescheduled)
	}
	return missing
}

// askedFrom checks the pages the reconcile asked: pages of them, for the node's pending timers, the
// limit 1000, the first from the list's start and each other from the cursor the page before gave.
func askedFrom(t *testing.T, timer *pagedSYTimer, pages int) {
	t.Helper()
	if len(timer.asked) != pages {
		t.Fatalf("the reconcile asked %d pages, want %d", len(timer.asked), pages)
	}
	for i, filter := range timer.asked {
		if filter.OwnerL2Name != testNodeName || filter.StatusFilter != "pending" || filter.Limit != sdk.MaxTimerListLimit {
			t.Fatalf("page %d asked as %+v", i+1, filter)
		}
		if (i == 0) != (filter.Cursor == "") {
			t.Fatalf("page %d asked from cursor %q", i+1, filter.Cursor)
		}
	}
}

// FINDINGS A-278 — a WF with more than 1000 pending timers reconciles them all: the reconcile reads
// SY.timer's list page after page, each from the cursor the page before gave, until a page gives
// none, and takes none as missing. Before, it read 1000 at the most, and took the 1,500 past the
// 1000th as missing: each was refused ("TIMER_LIST: SY.timer@<hive> does not have it") and sent
// again, 32 a second, the node DEGRADED meanwhile, SY.timer answering each already_existed.
func TestAReconcileReadsEveryPageOfItsPendingTimers(t *testing.T) {
	for _, tc := range []struct {
		name         string
		frame, pages int
	}{
		{"pages of 1000", 1000, 3},
		{"pages of what fits a frame", wfTimersAPage, 11},
	} {
		t.Run(tc.name, func(t *testing.T) {
			timer := &pagedSYTimer{t: t, frame: tc.frame}
			rt, _ := newTestRuntime(t, "", validWorkflowJSON(), timer)
			timer.pending = pendingTimers(t, rt, 250, 10, fixedClock().Add(time.Hour).UnixMilli())
			if err := rt.Recover(context.Background(), recoverOpts); err != nil {
				t.Fatal(err)
			}
			if rt.Registry.Count() != 250 {
				t.Fatalf("running instances: %d", rt.Registry.Count())
			}
			if missing := takenAsMissing(t, rt, timer, 2500); len(missing) != 0 {
				t.Fatalf("%d of the 2,500 timers SY.timer has taken as missing, the first %s", len(missing), missing[0])
			}
			askedFrom(t, timer, tc.pages)
		})
	}
}

// FINDINGS A-278 — an SY.timer before 1.6 (FINDINGS A-266), as a hive mid-update may run, answers
// next_cursor only when the frame ends a page. The frame ends each page of the reconcile's, of the
// limit 1000 (about 228 of a WF's timers fit it), so the reconcile reads them all the same. Were the
// limit to end a page, 1000 timers in one frame, the page would give no next_cursor with timers
// after it: the reconcile stops there, and takes none past the last one listed as missing, while one
// before it that SY.timer does not have still goes again.
func TestAReconcileWithAnSYTimerBefore16(t *testing.T) {
	t.Run("its frame ends each page", func(t *testing.T) {
		timer := &pagedSYTimer{t: t, frame: wfTimersAPage, beforeV16: true}
		rt, _ := newTestRuntime(t, "", validWorkflowJSON(), timer)
		timer.pending = pendingTimers(t, rt, 250, 10, fixedClock().Add(time.Hour).UnixMilli())
		if err := rt.Recover(context.Background(), recoverOpts); err != nil {
			t.Fatal(err)
		}
		if missing := takenAsMissing(t, rt, timer, 2500); len(missing) != 0 {
			t.Fatalf("%d timers SY.timer has taken as missing, the first %s", len(missing), missing[0])
		}
		askedFrom(t, timer, 11)
	})
	t.Run("its limit ends a page", func(t *testing.T) {
		timer := &pagedSYTimer{t: t, frame: sdk.MaxTimerListLimit, beforeV16: true}
		rt, _ := newTestRuntime(t, "", validWorkflowJSON(), timer)
		pending := pendingTimers(t, rt, 250, 10, fixedClock().Add(time.Hour).UnixMilli())
		timer.pending = without(pending, 500)
		if err := rt.Recover(context.Background(), recoverOpts); err != nil {
			t.Fatal(err)
		}
		if missing := takenAsMissing(t, rt, timer, 2500); len(missing) != 1 || missing[0] != *pending[500].ClientRef {
			t.Fatalf("only the timer SY.timer lost among the 1000 listed is taken as missing, %s: %d taken, the first %v",
				*pending[500].ClientRef, len(missing), missing[:min(3, len(missing))])
		}
		askedFrom(t, timer, 1)
	})
}

// FINDINGS A-278 — the reconcile reads maxTimerListPages pages at the most: there the list stops,
// and the timers of the index past the last one listed are left as they are, none taken as missing,
// while one before it that SY.timer does not have still goes again. An SY.timer that lists right
// needs 44 pages for the 10,000 timers it keeps for an owner at the most.
func TestAReconcileReadsItsBoundOfPagesAtTheMost(t *testing.T) {
	timer := &pagedSYTimer{t: t, frame: 10}
	rt, _ := newTestRuntime(t, "", validWorkflowJSON(), timer)
	pending := pendingTimers(t, rt, 250, 10, fixedClock().Add(time.Hour).UnixMilli())
	timer.pending = without(pending, 100)
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	if missing := takenAsMissing(t, rt, timer, 2500); len(missing) != 1 || missing[0] != *pending[100].ClientRef {
		t.Fatalf("only the timer SY.timer lost among the 2,000 listed is taken as missing, %s: %d taken, the first %v",
			*pending[100].ClientRef, len(missing), missing[:min(3, len(missing))])
	}
	askedFrom(t, timer, maxTimerListPages)
}

// FINDINGS A-278 — the reconcile's list ends whatever SY.timer answers: a page that holds no timer and
// gives a next_cursor, or a next_cursor a page before gave (an SY.timer that takes no cursor answers
// its first page again), stops it there, and the timers of the index past the last one listed are
// left as they are, none taken as missing; one before it that SY.timer does not have still goes again.
func TestAReconcileStopsAtAPageItCannotGoOnFrom(t *testing.T) {
	var first sdk.TimerListPage
	for _, tc := range []struct {
		name  string
		odd   func(n int, page sdk.TimerListPage) sdk.TimerListPage
		pages int
	}{
		{"a page of no timer with a next_cursor", func(n int, page sdk.TimerListPage) sdk.TimerListPage {
			if n == 2 {
				return sdk.TimerListPage{NextCursor: page.NextCursor}
			}
			return page
		}, 2},
		{"a next_cursor a page before gave", func(n int, page sdk.TimerListPage) sdk.TimerListPage {
			if n == 1 {
				first = page
			}
			return first
		}, 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			timer := &pagedSYTimer{t: t, frame: 500, odd: tc.odd}
			rt, _ := newTestRuntime(t, "", validWorkflowJSON(), timer)
			pending := pendingTimers(t, rt, 250, 10, fixedClock().Add(time.Hour).UnixMilli())
			timer.pending = without(pending, 100)
			if err := rt.Recover(context.Background(), recoverOpts); err != nil {
				t.Fatal(err)
			}
			if missing := takenAsMissing(t, rt, timer, 2500); len(missing) != 1 || missing[0] != *pending[100].ClientRef {
				t.Fatalf("only the timer SY.timer lost among the 500 listed is taken as missing, %s: %d taken, the first %v",
					*pending[100].ClientRef, len(missing), missing[:min(3, len(missing))])
			}
			askedFrom(t, timer, tc.pages)
		})
	}
}

// FINDINGS A-278 — SY.timer's list in pages is no snapshot (docs/sy-timer.md §7.6): its pages are
// read one after another while timers fire, and while the receive loop runs (a reconcile it asks
// for), instances schedule and move them. None of those is taken as missing:
//   - one SY.timer fires after its page was read is listed pending, past its time: the instance takes
//     its fire now, and the pending one is cancelled (SY.timer answers it fired already);
//   - one SY.timer fires before its page is read is not listed, past its time: the instance takes its
//     fire now, as one lost while the node was down;
//   - one an instance schedules, or moves, after the list was asked is left alone, listed or not.
func TestTimersThatChangeWhileThePagesAreReadAreNotTakenAsMissing(t *testing.T) {
	timer := &pagedSYTimer{t: t, frame: 2}
	rt, _ := newTestRuntime(t, "", validWorkflowJSON(), timer)
	ctx := context.Background()
	const instanceID = "wfi:walk"
	putRunningInstance(t, rt, instanceID, "collecting_data")
	at := func(d time.Duration) int64 { return fixedClock().Add(d).UnixMilli() }
	held := func(key string, fireAtMS int64) sdk.TimerInfo {
		info := timerInfo(timerClientRef(instanceID, key), fireAtMS)
		info.UUID = "uuid-" + key
		return info
	}
	for _, timer := range []struct {
		key string
		at  time.Duration
	}{{"listed_fired", -3 * time.Minute}, {"fired_after_its_page", -2 * time.Minute}, {"fired_before_its_page", -time.Minute},
		{"ahead_1", time.Hour}, {"ahead_2", 2 * time.Hour}, {"moved", 3 * time.Hour}} {
		if err := rt.Store.RegisterTimer(ctx, instanceID, timer.key, 1000, at(timer.at)); err != nil {
			t.Fatal(err)
		}
	}
	timer.pending = []sdk.TimerInfo{held("listed_fired", at(-3*time.Minute)), held("fired_after_its_page", at(-2*time.Minute)),
		held("fired_before_its_page", at(-time.Minute)), held("ahead_1", at(time.Hour)), held("ahead_2", at(2*time.Hour)),
		held("moved", at(3*time.Hour))}
	// SY.timer fires what is due as the pages go; the instance's later requests, after the list was
	// asked, go to the index and to SY.timer at once.
	fire := func(key string) {
		for i, pending := range timer.pending {
			if pending.UUID == "uuid-"+key {
				timer.pending = without(timer.pending, i)
				return
			}
		}
		t.Fatalf("no pending timer %s to fire", key)
	}
	later := nowMS(fixedClock) + 1000
	timer.onPage = func(n int) {
		switch n {
		case 2:
			fire("fired_after_its_page") // listed on page 1
			fire("fired_before_its_page")
		case 3:
			// Moved behind the cursor, before ahead_2: no page lists it. Scheduled behind it and
			// after it: one listed, one not.
			if err := rt.Store.MoveTimer(ctx, instanceID, "moved", later, at(90*time.Minute)); err != nil {
				t.Fatal(err)
			}
			for i := range timer.pending {
				if timer.pending[i].UUID == "uuid-moved" {
					timer.pending[i].FireAtUTCMS = at(90 * time.Minute)
				}
			}
			for key, d := range map[string]time.Duration{"scheduled_behind": 30 * time.Minute, "scheduled_ahead": 4 * time.Hour} {
				if err := rt.Store.RegisterTimer(ctx, instanceID, key, later, at(d)); err != nil {
					t.Fatal(err)
				}
				timer.pending = append(timer.pending, held(key, at(d)))
			}
		}
	}
	if err := rt.Recover(ctx, recoverOpts); err != nil {
		t.Fatal(err)
	}
	// The three past their time took their fire, and their rows went with it; the two SY.timer listed
	// pending are cancelled in it.
	if want := []string{timerClientRef(instanceID, "listed_fired"), timerClientRef(instanceID, "fired_after_its_page")}; fmt.Sprint(timer.cancelled) != fmt.Sprint(want) {
		t.Fatalf("cancelled in SY.timer: %v, want %v", timer.cancelled, want)
	}
	timer.cancelled = nil
	logs, err := rt.Store.GetRecentLog(ctx, instanceID, 10)
	if err != nil {
		t.Fatal(err)
	}
	fires := 0
	for _, entry := range logs {
		if entry.Summary == "unhandled event TIMER_FIRED in state collecting_data" {
			fires++
		}
	}
	if fires != 3 {
		t.Fatalf("the instance takes the 3 fires due, once each: %d (%+v)", fires, logs)
	}
	if missing := takenAsMissing(t, rt, timer, 5); len(missing) != 0 {
		t.Fatalf("taken as missing: %v", missing)
	}
	rows, _ := rt.Store.ListTimersForInstance(ctx, instanceID)
	keys := []string{}
	for _, row := range rows {
		keys = append(keys, row.TimerKey)
	}
	sort.Strings(keys)
	if want := "[ahead_1 ahead_2 moved scheduled_ahead scheduled_behind]"; fmt.Sprint(keys) != want {
		t.Fatalf("the index keeps %v, want %s", keys, want)
	}
	askedFrom(t, timer, 3)
}
