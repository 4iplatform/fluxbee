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
// request's trace_id, never the refusal's message, which can quote what the request sent (FINDINGS
// A-232). The lines are bounded per sender, so a node that loops a refused request cannot flood the
// journal: at most refusalLogBurst in a window of refusalLogWindow from its first refusal, the last
// of them giving the window's end. The others are counted, and one line,
// timer_request_refusals_not_logged, gives their number when the window ends: at most 10 lines a
// minute per sender, and the count of the rest.
const (
	refusalLogWindow = time.Minute
	refusalLogBurst  = 10
	// refusalLogSweepInterval is how often the windows that ended are closed (sweep), for the count of
	// a sender that is refused no more.
	refusalLogSweepInterval = 10 * time.Second
	// maxLoggedVerbBytes is the most of a verb a line keeps: an unknown one is whatever its sender sent.
	maxLoggedVerbBytes = 64
)

// refusalLog holds each sender's window, for the senders refused in the last refusalLogWindow. Its
// zero value is ready.
type refusalLog struct {
	mu      sync.Mutex
	senders map[string]*refusalWindow
}

// refusalWindow is a sender's refusals since start: the ones logged, and the ones counted without a
// line, with the verb and code of the last of them.
type refusalWindow struct {
	start     time.Time
	logged    int
	notLogged int
	lastVerb  string
	lastCode  string
}

// note logs a refusal of sender, at now, unless the sender's window has had its refusalLogBurst lines:
// then it is counted. A window that ended is closed first, and a new one starts.
func (l *refusalLog) note(now time.Time, sender, verb, code, traceID string) {
	verb = verbForLog(verb)
	l.mu.Lock()
	defer l.mu.Unlock()
	window := l.senders[sender]
	if window != nil && !now.Before(window.end()) {
		window.close(sender)
		window = nil
	}
	if window == nil {
		if l.senders == nil {
			l.senders = map[string]*refusalWindow{}
		}
		window = &refusalWindow{start: now}
		l.senders[sender] = window
	}
	if window.logged >= refusalLogBurst {
		window.notLogged++
		window.lastVerb, window.lastCode = verb, code
		return
	}
	window.logged++
	fields := map[string]any{
		"verb":        verb,
		"error_code":  code,
		"src_l2_name": sender,
		"trace_id":    traceID,
	}
	if window.logged == refusalLogBurst {
		fields["not_logged_until_utc_ms"] = window.end().UnixMilli()
	}
	logEvent("timer_request_refused", fields)
}

// sweep closes the windows that ended by now: each logs the refusals it counted without a line, if
// any, and leaves the map.
func (l *refusalLog) sweep(now time.Time) {
	l.mu.Lock()
	defer l.mu.Unlock()
	for sender, window := range l.senders {
		if !now.Before(window.end()) {
			window.close(sender)
			delete(l.senders, sender)
		}
	}
}

// sweepEvery sweeps every interval, for the life of the process (main).
func (l *refusalLog) sweepEvery(interval time.Duration) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for now := range ticker.C {
		l.sweep(now)
	}
}

func (w *refusalWindow) end() time.Time {
	return w.start.Add(refusalLogWindow)
}

// close logs the refusals of sender the window counted without a line, if any.
func (w *refusalWindow) close(sender string) {
	if w.notLogged == 0 {
		return
	}
	logEvent("timer_request_refusals_not_logged", map[string]any{
		"src_l2_name":     sender,
		"count":           w.notLogged,
		"since_utc_ms":    w.start.UnixMilli(),
		"until_utc_ms":    w.end().UnixMilli(),
		"last_verb":       w.lastVerb,
		"last_error_code": w.lastCode,
	})
}

// verbForLog is verb, cut to maxLoggedVerbBytes at a character's start, and marked "..." when cut.
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
