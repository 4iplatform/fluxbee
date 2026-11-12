package node

import (
	"context"
	"fmt"
	"log"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// SY.wf-rules takes its requests one at a time, in the order they came, and each change gets its
// request's 60 s (wfRulesRequestBudget) from its turn. A change queued behind others starts late:
// while a hive is not read, a purge waits up to 35 s a removal, and a second caller's change could
// end past SY.admin's 75 s, which then answers TIMEOUT for a change that may still complete. So a
// change whose turn comes with less than about 20 s of SY.admin's wait left is refused at once, BUSY,
// retryable, nothing changed (FINDINGS A-307, the operator's call of 2026-11-12). A request's own
// 60 s still start at its turn: bounded by its arrival, a change that ran out mid-flight would still
// send its next step (the Go SDK sends a request whose context has expired) and answer a failure
// for a step SY.admin then made.

// turnRule is when a change's turn comes too late: SY.admin waits adminWait for SY.wf-rules' answer
// (wf_rules_request_timeout, 75 s unless JSR_ADMIN_WF_RULES_TIMEOUT_SECS says otherwise), counted
// here from when the request reached SY.wf-rules, and a change needs minLeft of it left at its turn.
type turnRule struct {
	adminWait, minLeft time.Duration
}

var defaultTurnRule = turnRule{adminWait: 75 * time.Second, minLeft: 20 * time.Second}

// late refuses a change that waited for its turn, BUSY and retryable, when less than minLeft of
// SY.admin's wait is left; nil when it may run.
func (r turnRule) late(node string, waited time.Duration) error {
	left := r.adminWait - waited
	if left >= r.minLeft {
		return nil
	}
	return WfRulesError{Code: "BUSY", Retryable: true, Detail: fmt.Sprintf(
		"%s takes one request at a time, and this change's turn came %s after it arrived, with %s left of SY.admin's %s wait for it, less than the %s a change is given: it was refused at once, and nothing was changed. Ask again",
		node, waited.Round(time.Millisecond), max(left, 0).Round(time.Millisecond), r.adminWait, r.minLeft)}
}

// changingCommands and changingOperations are the requests that change a workflow: its compile,
// its apply, its rollback, its compile and apply in one, and its delete. The reads (a check, a
// query, CONFIG_GET, NODE_STATUS_GET) are never refused for their turn.
var (
	changingCommands   = map[string]bool{"compile_workflow": true, "apply_workflow": true, "rollback_workflow": true, "delete_workflow": true}
	changingOperations = map[string]bool{"compile": true, "apply": true, "rollback": true, "compile_apply": true}
)

// lateChange is the refusal of a change (what, its command or operation) whose turn came after it
// waited, nil when it may run; the log names each refusal.
func (s *Service) lateChange(msg fluxbeesdk.Message, what string, waited time.Duration) error {
	err := s.turns.late(s.nodeName(), waited)
	if err != nil {
		log.Printf("%s refused BUSY (trace_id=%s): its turn came %s after it arrived", what, msg.Routing.TraceID, waited.Round(time.Millisecond))
	}
	return err
}

// request is a message SY.wf-rules took, and when (time.Now, its monotonic reading): how long it
// waited for its turn is counted from then (takeRequests).
type request struct {
	msg       fluxbeesdk.Message
	arrivedAt time.Time
}

// requestBacklog bounds the requests that wait for their turn once taken, as the SDK's own channel
// bounds those not taken yet.
const requestBacklog = 64

// takeRequests takes each message as it reaches SY.wf-rules, with when it came, into turns, where
// the requests wait for their turn in the order they came; it closes turns when incoming closes. A
// wait is counted from here: one spent before (the process stopped, or requestBacklog requests
// already waiting) is not, and SY.admin has waited that much longer.
func (s *Service) takeRequests(ctx context.Context, turns chan<- request) {
	defer close(turns)
	for {
		select {
		case <-ctx.Done():
			return
		case msg, ok := <-s.incoming:
			if !ok {
				return
			}
			select {
			case turns <- request{msg: msg, arrivedAt: time.Now()}:
			case <-ctx.Done():
				return
			}
		}
	}
}
