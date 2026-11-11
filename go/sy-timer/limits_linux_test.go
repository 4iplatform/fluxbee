//go:build linux

package main

import (
	"encoding/json"
	"fmt"
	"slices"
	"sort"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// A node schedules for itself: a target_l2_name that names another node is refused, unless a SY node
// asks (A-115).
func TestTimerTargetIsTheRequesterUnlessASYNodeAsks(t *testing.T) {
	for _, tc := range []struct {
		name      string
		verb      string
		requester string
		target    string
		code      string
	}{
		{name: "no target", verb: "TIMER_SCHEDULE", requester: "WF.demo@motherbee"},
		{name: "itself", verb: "TIMER_SCHEDULE", requester: "WF.demo@motherbee", target: "WF.demo@motherbee"},
		{name: "another node", verb: "TIMER_SCHEDULE", requester: "WF.demo@motherbee", target: "AI.other@motherbee", code: "TIMER_FORBIDDEN"},
		{name: "another node, recurring", verb: "TIMER_SCHEDULE_RECURRING", requester: "WF.demo@motherbee", target: "AI.other@motherbee", code: "TIMER_FORBIDDEN"},
		{name: "a SY node for another node", verb: "TIMER_SCHEDULE", requester: "SY.orchestrator@motherbee", target: "AI.other@motherbee"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			request := map[string]any{"fire_in_ms": time.Minute.Milliseconds()}
			if tc.verb == "TIMER_SCHEDULE_RECURRING" {
				request = map[string]any{"cron_spec": "0 9 * * MON"}
			}
			if tc.target != "" {
				request["target_l2_name"] = tc.target
			}
			if err := service.handleMessage(mustBuildSystemMessageAs(t, request, tc.verb, tc.requester)); err != nil {
				t.Fatalf("schedule: %v", err)
			}
			response := mustReadTimerResponse(t, tx)
			if tc.code == "" && !response.OK {
				t.Fatalf("refused: %+v", response.Error)
			}
			if tc.code != "" && (response.OK || response.Error == nil || response.Error.Code != tc.code) {
				t.Fatalf("want %s, got %+v", tc.code, response)
			}
		})
	}
}

// A one-shot fires 60 s to 366 days ahead, scheduled or rescheduled (A-115).
func TestOneShotFireAtHasAMaximum(t *testing.T) {
	day := int64(24 * time.Hour / time.Millisecond)
	for _, tc := range []struct {
		name string
		in   int64
		code string
	}{
		{name: "a year", in: 366 * day},
		{name: "a year and a day", in: 367 * day, code: "TIMER_ABOVE_MAXIMUM"},
		{name: "past what an instant holds", in: 1 << 62, code: "TIMER_ABOVE_MAXIMUM"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := scheduleFireAt(timerScheduleRequest{FireInMS: &tc.in})
			if tc.code == "" && err != nil {
				t.Fatalf("refused: %v", err)
			}
			if tc.code != "" && (err == nil || timerErrorCode(err) != tc.code) {
				t.Fatalf("want %s, got %v", tc.code, err)
			}
			at := time.Now().UTC().UnixMilli() + tc.in
			_, err = rescheduleFireAt(timerRescheduleRequest{NewFireAtUTCMS: &at})
			if (tc.code == "") != (err == nil) || (err != nil && timerErrorCode(err) != tc.code) {
				t.Fatalf("reschedule: want %q, got %v", tc.code, err)
			}
		})
	}
}

// A payload or metadata over 32 KiB, and a timer past its owner's or the hive's pending ones, are
// refused TIMER_LIMIT_EXCEEDED; an existing timer's client_ref is still answered (A-115).
func TestScheduleIsCapped(t *testing.T) {
	big := strings.Repeat("x", maxTimerDataBytes)
	t.Run("payload", func(t *testing.T) {
		service, tx := newTestService(t)
		service.scheduler = newTimerScheduler(service)
		msg := mustBuildSystemMessage(t, map[string]any{"fire_in_ms": time.Minute.Milliseconds(), "payload": map[string]any{"text": big}}, "TIMER_SCHEDULE")
		if err := service.respondTimerSchedule(msg); err != nil {
			t.Fatalf("schedule: %v", err)
		}
		if response := mustReadTimerResponse(t, tx); response.OK || response.Error == nil || response.Error.Code != "TIMER_LIMIT_EXCEEDED" {
			t.Fatalf("want TIMER_LIMIT_EXCEEDED, got %+v", response)
		}
	})
	t.Run("metadata", func(t *testing.T) {
		service, tx := newTestService(t)
		service.scheduler = newTimerScheduler(service)
		msg := mustBuildSystemMessage(t, map[string]any{"cron_spec": "0 9 * * MON", "metadata": map[string]any{"why": big}}, "TIMER_SCHEDULE_RECURRING")
		if err := service.respondTimerScheduleRecurring(msg); err != nil {
			t.Fatalf("schedule: %v", err)
		}
		if response := mustReadTimerResponse(t, tx); response.OK || response.Error == nil || response.Error.Code != "TIMER_LIMIT_EXCEEDED" {
			t.Fatalf("want TIMER_LIMIT_EXCEEDED, got %+v", response)
		}
	})
	for _, tc := range []struct {
		name            string
		perOwner, total int
		other           bool
	}{
		{name: "per owner", perOwner: 1, total: 100},
		{name: "per hive", perOwner: 100, total: 1, other: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			savedOwner, savedTotal := maxPendingTimersPerOwner, maxPendingTimers
			maxPendingTimersPerOwner, maxPendingTimers = tc.perOwner, tc.total
			t.Cleanup(func() { maxPendingTimersPerOwner, maxPendingTimers = savedOwner, savedTotal })
			service, tx := newTestService(t)
			service.scheduler = newTimerScheduler(service)
			schedule := func(requester, clientRef string) *fluxbeesdk.TimerResponse {
				request := map[string]any{"fire_in_ms": time.Minute.Milliseconds(), "client_ref": clientRef}
				if err := service.respondTimerSchedule(mustBuildSystemMessageAs(t, request, "TIMER_SCHEDULE", requester)); err != nil {
					t.Fatalf("schedule: %v", err)
				}
				return mustReadTimerResponse(t, tx)
			}
			if first := schedule("WF.demo@motherbee", "a"); !first.OK {
				t.Fatalf("first refused: %+v", first.Error)
			}
			second := "WF.demo@motherbee"
			if tc.other {
				second = "WF.other@motherbee"
			}
			if refused := schedule(second, "b"); refused.OK || refused.Error == nil || refused.Error.Code != "TIMER_LIMIT_EXCEEDED" {
				t.Fatalf("want TIMER_LIMIT_EXCEEDED, got %+v", refused)
			}
			if again := schedule("WF.demo@motherbee", "a"); !again.OK || again.AlreadyExisted == nil || !*again.AlreadyExisted {
				t.Fatalf("an existing timer's client_ref must still be answered: %+v", again)
			}
		})
	}
}

// A TIMER_LIST longer than a frame answers in pages (FINDINGS A-151): each page fits, a page with
// more after it says where they start (next_cursor), and the pages list every timer once, by fire
// time and then uuid. A cursor that is not one is refused.
func TestTimerListAnswersInPagesThatFitAFrame(t *testing.T) {
	service, tx := newTestService(t)
	const owner = "WF.demo@motherbee"
	const total = 300
	payload := `{"note":"` + strings.Repeat("x", 900) + `"}`
	base := time.Now().UTC().Add(time.Hour).UnixMilli()
	for i := 0; i < total; i++ {
		insertOwnedTimer(t, service.db, timerRow{
			UUID:         fmt.Sprintf("timer-%03d", i),
			OwnerL2Name:  owner,
			TargetL2Name: owner,
			Kind:         "oneshot",
			FireAtUTC:    base + int64(i/2), // two by two at one time: the uuid orders them
			MissedPolicy: "fire",
			Payload:      payload,
			Status:       "pending",
			CreatedAtUTC: base,
		})
	}

	type page struct {
		OK     bool `json:"ok"`
		Count  int  `json:"count"`
		Timers []struct {
			UUID string `json:"uuid"`
		} `json:"timers"`
		NextCursor string `json:"next_cursor"`
		Error      *struct {
			Code string `json:"code"`
		} `json:"error"`
	}
	list := func(cursor string) (page, int) {
		t.Helper()
		request := map[string]any{"limit": 1000}
		if cursor != "" {
			request["cursor"] = cursor
		}
		if err := service.respondTimerList(mustBuildSystemMessageAs(t, request, "TIMER_LIST", owner)); err != nil {
			t.Fatalf("list timers: %v", err)
		}
		raw := mustReadResponsePayload(t, tx)
		var answer page
		if err := json.Unmarshal(raw, &answer); err != nil {
			t.Fatalf("decode a page: %v", err)
		}
		return answer, len(raw)
	}

	var listed []string
	cursor := ""
	for pages := 1; ; pages++ {
		answer, size := list(cursor)
		if !answer.OK || answer.Count != len(answer.Timers) || size > maxTimerListPageBytes+1024 {
			t.Fatalf("page %d: ok=%v count=%d of %d timers, %d bytes", pages, answer.OK, answer.Count, len(answer.Timers), size)
		}
		for _, timer := range answer.Timers {
			listed = append(listed, timer.UUID)
		}
		if answer.NextCursor == "" {
			if pages < 2 {
				t.Fatalf("%d timers of about 1 KiB fit one page", total)
			}
			break
		}
		cursor = answer.NextCursor
	}
	if len(listed) != total {
		t.Fatalf("the pages listed %d timers, want %d", len(listed), total)
	}
	for i, uuid := range listed {
		if want := fmt.Sprintf("timer-%03d", i); uuid != want {
			t.Fatalf("timer %d is %s, want %s: every timer once, in order", i, uuid, want)
		}
	}

	if answer, _ := list("nonsense"); answer.OK || answer.Error == nil || answer.Error.Code != "TIMER_INVALID_PAYLOAD" {
		t.Fatalf("a cursor that is not one: %+v", answer)
	}
}

// timerListPage is a TIMER_LIST answer; NextCursor is nil when it has none.
type timerListPage struct {
	OK     bool `json:"ok"`
	Count  int  `json:"count"`
	Timers []struct {
		UUID        string `json:"uuid"`
		FireAtUTCMS int64  `json:"fire_at_utc_ms"`
	} `json:"timers"`
	NextCursor *string `json:"next_cursor"`
	Error      *struct {
		Code string `json:"code"`
	} `json:"error"`
}

// askTimerList sends request as requester's TIMER_LIST, and reads the page it answers.
func askTimerList(t *testing.T, service *Service, tx chan []byte, requester string, request map[string]any) timerListPage {
	t.Helper()
	if err := service.respondTimerList(mustBuildSystemMessageAs(t, request, "TIMER_LIST", requester)); err != nil {
		t.Fatalf("list timers: %v", err)
	}
	var page timerListPage
	if err := json.Unmarshal(mustReadResponsePayload(t, tx), &page); err != nil {
		t.Fatalf("decode a page: %v", err)
	}
	if !page.OK || page.Count != len(page.Timers) {
		t.Fatalf("a page: ok=%v count=%d of %d timers, error %+v", page.OK, page.Count, len(page.Timers), page.Error)
	}
	return page
}

// uuids are the uuids of the page's timers, in its order, joined by commas.
func (p timerListPage) uuids() string {
	uuids := make([]string, 0, len(p.Timers))
	for _, timer := range p.Timers {
		uuids = append(uuids, timer.UUID)
	}
	return strings.Join(uuids, ",")
}

// A TIMER_LIST page its limit cuts says so (FINDINGS A-266): it answers next_cursor, the place of its
// last timer, as a page the frame cuts does, and the same request with that cursor lists the timers
// right after that one. The page that lists the rest of the list answers none, a limit of exactly the
// rest included. For every status_filter, as a node listing its own, as SY.admin listing one owner's
// and the whole hive's, with timers of both owners and of every status at one fire time, which the
// uuid orders.
func TestATimerListPageItsLimitCutsSaysWhereTheNextOneStarts(t *testing.T) {
	service, tx := newTestService(t)
	const owner, other = "WF.demo@motherbee", "WF.other@motherbee"
	base := time.Now().UTC().Add(time.Hour).UnixMilli()
	type stored struct {
		uuid, owner, status string
		fireAt              int64
	}
	timers := []stored{
		{"t-k", owner, "pending", 0}, {"t-c", other, "pending", 0}, {"t-h", owner, "fired", 0},
		{"t-a", owner, "canceled", 1}, {"t-m", owner, "pending", 1},
		{"t-e", other, "fired", 2}, {"t-b", owner, "fired", 2}, {"t-l", owner, "canceled", 2},
		{"t-g", other, "canceled", 3}, {"t-d", owner, "pending", 3},
		{"t-j", owner, "fired", 4}, {"t-f", owner, "canceled", 4},
		{"t-i", other, "pending", 5}, {"t-n", owner, "pending", 5},
	}
	for i := range timers {
		timers[i].fireAt += base
		insertOwnedTimer(t, service.db, timerRow{
			UUID:         timers[i].uuid,
			OwnerL2Name:  timers[i].owner,
			TargetL2Name: timers[i].owner,
			Kind:         "oneshot",
			FireAtUTC:    timers[i].fireAt,
			MissedPolicy: "fire",
			Payload:      `{}`,
			Status:       timers[i].status,
			CreatedAtUTC: base,
		})
	}
	// listOf is what a list of ownerFilter's timers (the hive's for none) in status holds, by fire
	// time and then uuid.
	listOf := func(ownerFilter, status string) []string {
		var rows []stored
		for _, timer := range timers {
			if (ownerFilter == "" || timer.owner == ownerFilter) && (status == "all" || timer.status == status) {
				rows = append(rows, timer)
			}
		}
		sort.Slice(rows, func(i, j int) bool {
			if rows[i].fireAt != rows[j].fireAt {
				return rows[i].fireAt < rows[j].fireAt
			}
			return rows[i].uuid < rows[j].uuid
		})
		uuids := make([]string, 0, len(rows))
		for _, row := range rows {
			uuids = append(uuids, row.uuid)
		}
		return uuids
	}

	for _, scope := range []struct {
		name      string
		requester string
		asks      string // the owner_l2_name it sends
		lists     string // whose timers it lists, the hive's for none
	}{
		{name: "a node's own", requester: owner, lists: owner},
		{name: "SY.admin, one owner's", requester: fluxbeesdk.PrimaryAdminNode, asks: other, lists: other},
		{name: "SY.admin, the hive's", requester: fluxbeesdk.PrimaryAdminNode},
	} {
		for _, status := range []string{"", "pending", "fired", "canceled", "all"} {
			filter := status
			if filter == "" {
				filter = "pending"
			}
			all := listOf(scope.lists, filter)
			var limits []int
			for _, limit := range []int{1, 2, 3, len(all) - 1, len(all), len(all) + 1} {
				if limit >= 1 && !slices.Contains(limits, limit) {
					limits = append(limits, limit)
				}
			}
			for _, limit := range limits {
				t.Run(fmt.Sprintf("%s, status_filter %q, limit %d", scope.name, status, limit), func(t *testing.T) {
					cursor := ""
					for listed, pages := 0, 1; ; pages++ {
						request := map[string]any{"limit": limit}
						if status != "" {
							request["status_filter"] = status
						}
						if scope.asks != "" {
							request["owner_l2_name"] = scope.asks
						}
						if cursor != "" {
							request["cursor"] = cursor
						}
						page := askTimerList(t, service, tx, scope.requester, request)
						next := min(listed+limit, len(all))
						if got, want := page.uuids(), strings.Join(all[listed:next], ","); got != want {
							t.Fatalf("page %d lists %q, want %q: the timers right after the last one listed, in order", pages, got, want)
						}
						listed = next
						if listed == len(all) {
							if page.NextCursor != nil {
								t.Fatalf("page %d lists the rest of the list and answers next_cursor %q", pages, *page.NextCursor)
							}
							return
						}
						if page.NextCursor == nil {
							t.Fatalf("page %d, cut by its limit with %d timers after it, answers no next_cursor", pages, len(all)-listed)
						}
						last := page.Timers[len(page.Timers)-1]
						if want := fmt.Sprintf("%d:%s", last.FireAtUTCMS, last.UUID); *page.NextCursor != want {
							t.Fatalf("page %d answers next_cursor %q, want its last timer's place %q", pages, *page.NextCursor, want)
						}
						cursor = *page.NextCursor
					}
				})
			}
		}
	}
}

// The frame's cut still answers its cursor, and the two cuts give one (FINDINGS A-151, A-266): a page
// ends at its limit or at what fits a frame, whichever comes first, and either way next_cursor is the
// place of its last timer, where the next page starts. The SDKs' way, the limit lowered by what came,
// ends on a page the limit cuts after one the frame cut, and that page says more follow.
func TestTheFramesCutAndTheLimitsGiveOneCursor(t *testing.T) {
	service, tx := newTestService(t)
	const owner = "WF.demo@motherbee"
	const total = 200
	payload := `{"note":"` + strings.Repeat("x", 900) + `"}`
	base := time.Now().UTC().Add(time.Hour).UnixMilli()
	name := func(i int) string { return fmt.Sprintf("timer-%03d", i) }
	fireAt := func(i int) int64 { return base + int64(i/2) } // two by two at one time: the uuid orders them
	insertPendingTimers(t, service.db, owner, "timer-%03d", total, base, 2, payload)
	placeOf := func(i int) string { return fmt.Sprintf("%d:%s", fireAt(i), name(i)) }
	list := func(limit int, cursor string) timerListPage {
		t.Helper()
		request := map[string]any{"limit": limit}
		if cursor != "" {
			request["cursor"] = cursor
		}
		return askTimerList(t, service, tx, owner, request)
	}
	// expect checks that page lists count timers from the first, and answers the place of the timer
	// next names as its next_cursor, or none for -1.
	expect := func(what string, page timerListPage, first, count, next int) {
		t.Helper()
		want := make([]string, 0, count)
		for i := first; i < first+count; i++ {
			want = append(want, name(i))
		}
		if got := page.uuids(); got != strings.Join(want, ",") {
			t.Fatalf("%s: lists %d timers (%.40s...), want %d from %s", what, len(page.Timers), got, count, name(first))
		}
		switch {
		case next < 0 && page.NextCursor != nil:
			t.Fatalf("%s: next_cursor %q, want none", what, *page.NextCursor)
		case next >= 0 && page.NextCursor == nil:
			t.Fatalf("%s: no next_cursor, want %q", what, placeOf(next))
		case next >= 0 && *page.NextCursor != placeOf(next):
			t.Fatalf("%s: next_cursor %q, want %q", what, *page.NextCursor, placeOf(next))
		}
	}

	// A limit past what fits: the frame cuts the page.
	first := list(maxListLimit, "")
	fit := len(first.Timers)
	if fit < 3 || 2*fit >= total {
		t.Fatalf("%d timers of about 1 KiB fit a page: the test wants 3 or more, and less than half of %d", fit, total)
	}
	expect("the frame's cut", first, 0, fit, fit-1)
	// A limit that cuts the page where the frame does, or before it: the cursor of its last timer.
	expect("a limit of what fits", list(fit, ""), 0, fit, fit-1)
	expect("a limit one past what fits", list(fit+1, ""), 0, fit, fit-1)
	expect("a limit one short of what fits", list(fit-1, ""), 0, fit-1, fit-2)
	// The next page starts right after it, whichever cut gave it.
	expect("the page after the frame's cut", list(fit, placeOf(fit-1)), fit, fit, 2*fit-1)
	expect("the page after the limit's cut", list(1, placeOf(fit-2)), fit-1, 1, fit-1)

	// The SDKs' way: fit + fit/2 timers asked, then the limit lowered by what came. The frame cuts the
	// first page, the limit the second, and the second says more follow.
	asked := fit + fit/2
	expect("the first of the SDKs' pages", list(asked, ""), 0, fit, fit-1)
	second := list(asked-fit, placeOf(fit-1))
	expect("the second of the SDKs' pages", second, fit, asked-fit, asked-1)
	expect("the page after the SDKs' last", list(1, *second.NextCursor), asked, 1, asked)
	// The page that lists the rest answers none.
	expect("the rest", list(maxListLimit, placeOf(total-3)), total-2, 2, -1)
}

// A cursor TIMER_LIST cannot read is refused naming the field, never the value it was sent
// (FINDINGS A-232).
func TestATimerListCursorRefusalNamesNoValue(t *testing.T) {
	for _, cursor := range []string{"3f1c0e9a8b7d6c5e", "1730000000000", "notanumber:abc", "731:"} {
		_, err := parseTimerListCursor(cursor)
		if err == nil {
			t.Fatalf("cursor %q: no refusal", cursor)
		}
		if err.Error() != "cursor must be a next_cursor of TIMER_LIST" {
			t.Fatalf("cursor %q: refusal %q", cursor, err.Error())
		}
	}
	if got, err := parseTimerListCursor("1730000000000:4b1e"); err != nil || got == nil {
		t.Fatalf("a next_cursor: %v %v", got, err)
	}
}
