//go:build linux

package main

import (
	"sync"
	"time"
	"unicode/utf8"
)

// The refusal log (FINDINGS A-258; the operator's call on A-253: logs for everything). A request
// SY.timer refuses is answered to its sender, and gets one line in SY.timer's own log,
// timer_request_refused: the verb, the code, the sender (src_l2_name, the router's stamp) and the
// request's trace_id. Not the refusal's message, which can quote what the request sent (FINDINGS
// A-232), except a TIMER_STORAGE_ERROR's, the line's error: its text is the database's, and it says
// why SY.timer could not serve the request (the review of A-258). A message that is no request
// (isInfrastructureMessage) gets no answer, and one line too, timer_message_ignored: its name (msg),
// its sender and its trace_id.
//
// The lines are bounded per sender (senderLog), so a node that loops a refused request, or a notice,
// cannot flood the journal: at most senderLogBurst in a window of senderLogWindow from its first line,
// the last of them giving the window's end. The others are counted, and one line,
// timer_request_refusals_not_logged or timer_messages_ignored_not_logged, gives their number when the
// window ends: at most 10 lines a minute per sender of each kind, and the count of the rest. Before,
// a refusal reached its sender only, and every ignored message got a plain line, with no bound.
const (
	senderLogWindow = time.Minute
	senderLogBurst  = 10
	// senderLogSweepInterval is how often the windows that ended are closed (sweep), for the count of
	// a sender that sends no more.
	senderLogSweepInterval = 10 * time.Second
	// maxLoggedVerbBytes is the most of a verb, or of a message's name, a line keeps: an unknown one is
	// whatever its sender sent.
	maxLoggedVerbBytes = 64
)

// senderLog holds each sender's window, for the senders logged in the last senderLogWindow. Its zero
// value is ready. refusalLog and ignoredLog are its two logs, each with its own windows and events.
type senderLog struct {
	mu      sync.Mutex
	senders map[string]*senderWindow
}

// senderWindow is a sender's lines since start: the ones logged, and the ones counted without a line,
// with what the count keeps of the last of them (last, each key as last_<key>).
type senderWindow struct {
	start     time.Time
	logged    int
	notLogged int
	last      map[string]any
}

// add logs a line of sender at now, its fields under event, unless the sender's window has had its
// senderLogBurst lines: then it is counted, and last is kept for the count. A window that ended is
// closed first (its count under countEvent), and a new one starts.
func (l *senderLog) add(now time.Time, sender, event, countEvent string, fields, last map[string]any) {
	l.mu.Lock()
	defer l.mu.Unlock()
	window := l.senders[sender]
	if window != nil && !now.Before(window.end()) {
		window.close(sender, countEvent)
		window = nil
	}
	if window == nil {
		if l.senders == nil {
			l.senders = map[string]*senderWindow{}
		}
		window = &senderWindow{start: now}
		l.senders[sender] = window
	}
	if window.logged >= senderLogBurst {
		window.notLogged++
		window.last = last
		return
	}
	window.logged++
	fields["src_l2_name"] = sender
	if window.logged == senderLogBurst {
		fields["not_logged_until_utc_ms"] = window.end().UnixMilli()
	}
	logEvent(event, fields)
}

// closeEnded closes the windows that ended by now: each logs the lines it counted, if any, under
// countEvent, and leaves the map.
func (l *senderLog) closeEnded(now time.Time, countEvent string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	for sender, window := range l.senders {
		if !now.Before(window.end()) {
			window.close(sender, countEvent)
			delete(l.senders, sender)
		}
	}
}

func (w *senderWindow) end() time.Time {
	return w.start.Add(senderLogWindow)
}

// close logs, under countEvent, the lines of sender the window counted without a line, if any.
func (w *senderWindow) close(sender, countEvent string) {
	if w.notLogged == 0 {
		return
	}
	fields := map[string]any{
		"src_l2_name":  sender,
		"count":        w.notLogged,
		"since_utc_ms": w.start.UnixMilli(),
		"until_utc_ms": w.end().UnixMilli(),
	}
	for key, value := range w.last {
		fields["last_"+key] = value
	}
	logEvent(countEvent, fields)
}

// refusalLog logs the requests SY.timer refuses (sendTimerError). Its zero value is ready.
type refusalLog struct {
	senderLog
}

const (
	refusalEvent      = "timer_request_refused"
	refusalCountEvent = "timer_request_refusals_not_logged"
)

// note logs a refusal of sender at now: its verb, its code and its trace_id, and message as its error
// for a TIMER_STORAGE_ERROR only (above).
func (l *refusalLog) note(now time.Time, sender, verb, code, traceID, message string) {
	verb = verbForLog(verb)
	fields := map[string]any{
		"verb":       verb,
		"error_code": code,
		"trace_id":   traceID,
	}
	if code == "TIMER_STORAGE_ERROR" {
		fields["error"] = message
	}
	l.add(now, sender, refusalEvent, refusalCountEvent, fields, map[string]any{"verb": verb, "error_code": code})
}

// sweep closes the refusal windows that ended by now.
func (l *refusalLog) sweep(now time.Time) {
	l.closeEnded(now, refusalCountEvent)
}

// ignoredLog logs the messages SY.timer answers nothing because they are no request
// (isInfrastructureMessage). Its zero value is ready.
type ignoredLog struct {
	senderLog
}

const (
	ignoredEvent      = "timer_message_ignored"
	ignoredCountEvent = "timer_messages_ignored_not_logged"
)

// note logs a message of sender at now that is no request: its name and its trace_id. A message the
// router made has no sender: those share the sender "".
func (l *ignoredLog) note(now time.Time, sender, name, traceID string) {
	name = verbForLog(name)
	l.add(now, sender, ignoredEvent, ignoredCountEvent, map[string]any{"msg": name, "trace_id": traceID}, map[string]any{"msg": name})
}

// sweep closes the windows of ignored messages that ended by now.
func (l *ignoredLog) sweep(now time.Time) {
	l.closeEnded(now, ignoredCountEvent)
}

// sweepLogsEvery sweeps the refusal log and the log of ignored messages every interval, for the life
// of the process (main).
func (s *Service) sweepLogsEvery(interval time.Duration) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for now := range ticker.C {
		s.refusals.sweep(now)
		s.ignored.sweep(now)
	}
}

// verbForLog is verb, or a message's name, cut to maxLoggedVerbBytes at a character's start, and
// marked "..." when cut.
func verbForLog(verb string) string {
	if len(verb) <= maxLoggedVerbBytes {
		return verb
	}
	cut := maxLoggedVerbBytes
	for cut > 0 && !utf8.RuneStart(verb[cut]) {
		cut--
	}
	return verb[:cut] + "..."
}
