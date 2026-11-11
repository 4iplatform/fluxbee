//go:build linux

package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"strings"
	"testing"
	"time"
	"unicode/utf8"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// The refusal log (FINDINGS A-258): a request SY.timer refuses gets one line in its own log, with the
// verb, the code and the sender, bounded per sender. Before, only the sender heard of it. A message
// that is no request gets one line too, bounded the same way in windows of its own (the review of
// A-258).

// captureLog sends the log to a buffer for the rest of the test.
func captureLog(t *testing.T) *bytes.Buffer {
	t.Helper()
	var logged bytes.Buffer
	log.SetOutput(&logged)
	t.Cleanup(func() { log.SetOutput(os.Stderr) })
	return &logged
}

// loggedEvents returns the events named name in logged, a log's output (logEvent: one JSON object a
// line, after the log's prefix).
func loggedEvents(t *testing.T, logged *bytes.Buffer, name string) []map[string]any {
	t.Helper()
	var events []map[string]any
	for _, line := range strings.Split(logged.String(), "\n") {
		start := strings.Index(line, "{")
		if start < 0 {
			continue
		}
		var event map[string]any
		if err := json.Unmarshal([]byte(line[start:]), &event); err != nil {
			continue
		}
		if event["event"] == name {
			events = append(events, event)
		}
	}
	return events
}

// Each refusal is logged, with its verb, its code, its sender and its trace_id, whatever refuses it
// (a bound, a permission, a cron_spec, a verb SY.timer does not know); never with its message, which
// quotes what the request sent (a TIMER_STORAGE_ERROR's, the database's, is the one logged: below).
// An answer that is not a refusal, and a notice, which gets none, log no refusal.
func TestARefusalIsLoggedWithItsVerbCodeAndSender(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	logged := captureLog(t)
	const quoted = "pasted-by-mistake-1f2e3d"
	refused := []struct {
		verb, sender, code string
		request            map[string]any
	}{
		{verb: "TIMER_SCHEDULE", sender: "WF.demo@motherbee", code: "TIMER_BELOW_MINIMUM", request: map[string]any{"fire_in_ms": 1000}},
		{verb: "TIMER_LIST", sender: "WF.demo@motherbee", code: "TIMER_FORBIDDEN", request: map[string]any{"owner_l2_name": "AI.other@motherbee"}},
		{verb: "TIMER_SCHEDULE_RECURRING", sender: "AI.other@motherbee", code: "TIMER_INVALID_CRON", request: map[string]any{"cron_spec": "0 9 * * * " + quoted}},
		{verb: "TIMER_PURGE_OWNER", sender: "WF.demo@motherbee", code: "TIMER_FORBIDDEN", request: map[string]any{"owner_l2_name": "AI.other@motherbee"}},
		{verb: "TIMER_SOMETHING", sender: "IO.api@motherbee", code: "TIMER_UNKNOWN_VERB", request: map[string]any{}},
	}
	traceIDs := make([]string, len(refused))
	for i, tc := range refused {
		msg := mustBuildSystemMessageAs(t, tc.request, tc.verb, tc.sender)
		traceIDs[i] = msg.Routing.TraceID
		if err := service.handleMessage(msg); err != nil {
			t.Fatalf("%s: %v", tc.verb, err)
		}
		response := mustReadTimerResponse(t, tx)
		if response.OK || response.Error == nil || response.Error.Code != tc.code {
			t.Fatalf("%s: %+v (error %+v), want %s", tc.verb, response, response.Error, tc.code)
		}
		if tc.code == "TIMER_INVALID_CRON" && !strings.Contains(response.Error.Message, quoted) {
			t.Fatalf("the refusal of a junk cron_spec no longer quotes it, and this test checks nothing: %q", response.Error.Message)
		}
	}
	if err := service.handleMessage(mustBuildSystemMessage(t, map[string]any{}, "TIMER_NOW")); err != nil {
		t.Fatalf("TIMER_NOW: %v", err)
	}
	if response := mustReadTimerResponse(t, tx); !response.OK {
		t.Fatalf("TIMER_NOW: %+v", response.Error)
	}
	if err := service.handleMessage(mustBuildSystemMessage(t, map[string]any{}, fluxbeesdk.MSGHello)); err != nil {
		t.Fatalf("HELLO: %v", err)
	}

	lines := loggedEvents(t, logged, "timer_request_refused")
	if len(lines) != len(refused) {
		t.Fatalf("logged %d refusals, want %d: %s", len(lines), len(refused), logged.String())
	}
	for i, tc := range refused {
		want := map[string]any{"event": "timer_request_refused", "verb": tc.verb, "error_code": tc.code, "src_l2_name": tc.sender, "trace_id": traceIDs[i]}
		if fmt.Sprint(lines[i]) != fmt.Sprint(want) {
			t.Fatalf("refusal %d logged %v, want %v", i, lines[i], want)
		}
	}
	if strings.Contains(logged.String(), quoted) {
		t.Fatalf("a refusal's line quotes what the request sent: %s", logged.String())
	}
}

// A sender's refusal lines are bounded: 10 in a minute from its first refusal, the tenth saying until
// when the next are not logged; those are counted, and one line gives their number, with the last
// one's verb and code, when the window ends, by the sweep or at the sender's next refusal. Each sender
// has its own window, and a window that ended leaves the log's map.
func TestRefusalLinesAreBoundedPerSender(t *testing.T) {
	logged := captureLog(t)
	var refusals refusalLog
	start := time.UnixMilli(1_800_000_000_000)
	at := func(d time.Duration) time.Time { return start.Add(d) }
	const looping, other = "WF.loop@motherbee", "AI.other@motherbee"
	for i := 0; i < 25; i++ {
		code := "TIMER_BELOW_MINIMUM"
		if i == 24 {
			code = "TIMER_LIMIT_EXCEEDED"
		}
		refusals.note(at(time.Duration(i)*time.Second), looping, "TIMER_SCHEDULE", code, fmt.Sprintf("trace-%d", i), "refused")
	}
	for i := 0; i < 3; i++ {
		refusals.note(at(time.Duration(i)*time.Second), other, "TIMER_CANCEL", "TIMER_NOT_OWNER", fmt.Sprintf("trace-other-%d", i), "refused")
	}
	lines := loggedEvents(t, logged, "timer_request_refused")
	perSender := map[string]int{}
	for _, line := range lines {
		perSender[line["src_l2_name"].(string)]++
	}
	if perSender[looping] != 10 || perSender[other] != 3 {
		t.Fatalf("logged %v refusals per sender, want 10 of %s and 3 of %s", perSender, looping, other)
	}
	for _, line := range lines {
		until, marked := line["not_logged_until_utc_ms"]
		if tenth := line["trace_id"] == "trace-9"; marked != tenth || (tenth && until != float64(at(time.Minute).UnixMilli())) {
			t.Fatalf("refusal %v: only the tenth of a sender says until when the next are not logged, the window's end", line)
		}
	}

	refusals.sweep(at(time.Minute - time.Millisecond))
	if counted := loggedEvents(t, logged, "timer_request_refusals_not_logged"); len(counted) != 0 {
		t.Fatalf("a sweep before the window's end counted %v", counted)
	}
	refusals.sweep(at(time.Minute))
	counted := loggedEvents(t, logged, "timer_request_refusals_not_logged")
	want := map[string]any{"event": "timer_request_refusals_not_logged", "src_l2_name": looping, "count": float64(15),
		"since_utc_ms": float64(start.UnixMilli()), "until_utc_ms": float64(at(time.Minute).UnixMilli()),
		"last_verb": "TIMER_SCHEDULE", "last_error_code": "TIMER_LIMIT_EXCEEDED"}
	if len(counted) != 1 || fmt.Sprint(counted[0]) != fmt.Sprint(want) {
		t.Fatalf("the window's end counted %v, want %v", counted, want)
	}
	refusals.mu.Lock()
	kept := len(refusals.senders)
	refusals.mu.Unlock()
	if kept != 0 {
		t.Fatalf("the windows that ended left %d senders in the map", kept)
	}

	// The next window, closed by the sender's next refusal instead of a sweep.
	logged.Reset()
	for i := 0; i < 12; i++ {
		refusals.note(at(2*time.Minute+time.Duration(i)*time.Second), looping, "TIMER_GET", "TIMER_NOT_FOUND", "trace-next", "timer not found")
	}
	refusals.note(at(3*time.Minute+time.Second), looping, "TIMER_GET", "TIMER_NOT_FOUND", "trace-after", "timer not found")
	if lines := loggedEvents(t, logged, "timer_request_refused"); len(lines) != 11 || lines[10]["trace_id"] != "trace-after" {
		t.Fatalf("logged %d refusals (%v), want 10 and the one after the window", len(lines), lines)
	}
	if counted := loggedEvents(t, logged, "timer_request_refusals_not_logged"); len(counted) != 1 || counted[0]["count"] != float64(2) {
		t.Fatalf("the window's end at the next refusal counted %v, want 2", counted)
	}
	if strings.Index(logged.String(), "timer_request_refusals_not_logged") > strings.Index(logged.String(), "trace-after") {
		t.Fatalf("the count of a window came after the refusal that closed it: %s", logged.String())
	}
}

// A verb SY.timer does not know is whatever its sender sent: its line keeps at most 64 bytes of it,
// cut at a character's start.
func TestARefusedUnknownVerbIsCutInItsLine(t *testing.T) {
	logged := captureLog(t)
	var refusals refusalLog
	// 62 bytes, then a 3-byte character (U+20AC) across the 64th.
	verb := strings.Repeat("ab", 31) + "€" + strings.Repeat("x", 4096)
	refusals.note(time.Now(), "WF.demo@motherbee", verb, "TIMER_UNKNOWN_VERB", "trace", "SY.timer does not know the verb")
	lines := loggedEvents(t, logged, "timer_request_refused")
	if len(lines) != 1 {
		t.Fatalf("logged %d refusals, want 1", len(lines))
	}
	if got := lines[0]["verb"].(string); got != strings.Repeat("ab", 31)+"..." || !utf8.ValidString(got) {
		t.Fatalf("logged the verb as %q", got)
	}
	if short := verbForLog("TIMER_SCHEDULE"); short != "TIMER_SCHEDULE" {
		t.Fatalf("cut a known verb: %q", short)
	}
}

// A TIMER_STORAGE_ERROR's line gives its message as error: it is the database's, never what the
// request sent, and it says why SY.timer could not serve the request; no other refusal's line gives
// its message (above). Before, the journal said TIMER_STORAGE_ERROR and never why (the review of
// A-258).
func TestAStorageRefusalIsLoggedWithTheDatabasesError(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	logged := captureLog(t)
	if err := service.db.Close(); err != nil {
		t.Fatalf("close the database: %v", err)
	}
	msg := mustBuildSystemMessageAs(t, map[string]any{"status_filter": "all"}, "TIMER_LIST", "WF.demo@motherbee")
	if err := service.handleMessage(msg); err != nil {
		t.Fatalf("TIMER_LIST: %v", err)
	}
	response := mustReadTimerResponse(t, tx)
	if response.OK || response.Error == nil || response.Error.Code != "TIMER_STORAGE_ERROR" || !strings.Contains(response.Error.Message, "database is closed") {
		t.Fatalf("TIMER_LIST: %+v (error %+v), want the database's TIMER_STORAGE_ERROR", response, response.Error)
	}
	lines := loggedEvents(t, logged, "timer_request_refused")
	want := map[string]any{"event": "timer_request_refused", "verb": "TIMER_LIST", "error_code": "TIMER_STORAGE_ERROR",
		"error": response.Error.Message, "src_l2_name": "WF.demo@motherbee", "trace_id": msg.Routing.TraceID}
	if len(lines) != 1 || fmt.Sprint(lines[0]) != fmt.Sprint(want) {
		t.Fatalf("logged %v, want %v", lines, want)
	}
}

// A message that is no request gets no answer, and a line bounded per sender as a refusal's is, in
// windows of its own: 10 lines a minute per sender, then the count of the rest, with the last one's
// name. Messages the router made have no sender, and share one window. Before, each got a plain line,
// with no bound (the review of A-258).
func TestIgnoredMessagesAreLoggedBoundedPerSender(t *testing.T) {
	service, tx := newTestService(t)
	service.scheduler = newTimerScheduler(service)
	logged := captureLog(t)
	const looping = "WF.loop@motherbee"
	handle := func(msg fluxbeesdk.Message) {
		t.Helper()
		if err := service.handleMessage(msg); err != nil {
			t.Fatalf("handle %s: %v", stringValue(msg.Meta.Msg), err)
		}
	}
	for i := 0; i < 12; i++ {
		name := "TIMER_FIRED_RESPONSE"
		if i == 11 {
			name = fluxbeesdk.MSGHello
		}
		handle(mustBuildSystemMessageAs(t, map[string]any{}, name, looping))
	}
	// The router's UNREACHABLE for no fire in flight: it has no sender.
	routerMade := unreachable(t, "trace-of-no-fire", "NODE_NOT_FOUND")
	handle(routerMade)
	// A refusal of the same sender has its own window.
	handle(mustBuildSystemMessageAs(t, map[string]any{"fire_in_ms": 1000}, "TIMER_SCHEDULE", looping))
	if response := mustReadTimerResponse(t, tx); response.OK || response.Error == nil || response.Error.Code != "TIMER_BELOW_MINIMUM" {
		t.Fatalf("TIMER_SCHEDULE: %+v (error %+v)", response, response.Error)
	}
	if len(tx) != 0 {
		t.Fatal("answered a message that is no request")
	}

	lines := loggedEvents(t, logged, "timer_message_ignored")
	if len(lines) != 11 {
		t.Fatalf("logged %d ignored messages, want 10 of %s and the router's: %v", len(lines), looping, lines)
	}
	for i, line := range lines[:10] {
		want := map[string]any{"event": "timer_message_ignored", "msg": "TIMER_FIRED_RESPONSE", "src_l2_name": looping, "trace_id": line["trace_id"]}
		if i == 9 {
			until, ok := line["not_logged_until_utc_ms"].(float64)
			if !ok || until <= 0 {
				t.Fatalf("the tenth line of %s does not say until when the next are not logged: %v", looping, line)
			}
			want["not_logged_until_utc_ms"] = until
		}
		if line["trace_id"] == "" || fmt.Sprint(line) != fmt.Sprint(want) {
			t.Fatalf("ignored message %d logged %v, want %v", i, line, want)
		}
	}
	if want := map[string]any{"event": "timer_message_ignored", "msg": fluxbeesdk.MSGUnreachable, "src_l2_name": "", "trace_id": "trace-of-no-fire"}; fmt.Sprint(lines[10]) != fmt.Sprint(want) {
		t.Fatalf("the router's message logged %v, want %v", lines[10], want)
	}
	if refused := loggedEvents(t, logged, "timer_request_refused"); len(refused) != 1 || refused[0]["src_l2_name"] != looping {
		t.Fatalf("the refusal of %s logged %v, want one line in a window of its own", looping, refused)
	}

	service.ignored.sweep(time.Now().Add(senderLogWindow))
	counted := loggedEvents(t, logged, "timer_messages_ignored_not_logged")
	if len(counted) != 1 || counted[0]["src_l2_name"] != looping || counted[0]["count"] != float64(2) || counted[0]["last_msg"] != fluxbeesdk.MSGHello {
		t.Fatalf("the windows' end counted %v, want 2 of %s, the last a %s", counted, looping, fluxbeesdk.MSGHello)
	}
	if refusals := loggedEvents(t, logged, "timer_request_refusals_not_logged"); len(refusals) != 0 {
		t.Fatalf("the ignored messages were counted as refusals: %v", refusals)
	}
}

// A handler error's line names who asked, as a refusal's does: an answer over the frame is answered
// TIMER_INTERNAL by sendTimerResponse, not by sendTimerError, and logged as timer_handler_error. Before,
// with its verb and trace_id only (the review of A-258).
func TestAHandlerErrorIsLoggedWithItsSender(t *testing.T) {
	service, tx := newTestService(t)
	logged := captureLog(t)
	service.sender.(*stubSender).refuse = 1
	msg := mustBuildSystemMessageAs(t, map[string]any{}, "TIMER_NOW", "WF.demo@motherbee")
	incoming := make(chan fluxbeesdk.Message, 1)
	incoming <- msg
	close(incoming)
	service.incoming = incoming
	service.run()

	if response := mustReadTimerResponse(t, tx); response.OK || response.Error == nil || response.Error.Code != "TIMER_INTERNAL" {
		t.Fatalf("answered %+v (error %+v), want TIMER_INTERNAL", response, response.Error)
	}
	lines := loggedEvents(t, logged, "timer_handler_error")
	if len(lines) != 1 || lines[0]["src_l2_name"] != "WF.demo@motherbee" || lines[0]["verb"] != "TIMER_NOW" || lines[0]["trace_id"] != msg.Routing.TraceID {
		t.Fatalf("logged %v, want one timer_handler_error of WF.demo@motherbee's TIMER_NOW", lines)
	}
}
