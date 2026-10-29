package node

import (
	"context"
	"encoding/json"
	"fmt"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// panickingDispatcher panics on a send to panicAt.
type panickingDispatcher struct {
	mockDispatcher
	panicAt string
}

func (d *panickingDispatcher) SendMsg(msg sdk.Message) error {
	if msg.Routing.Dst.Value() == d.panicAt {
		panic("a send that panics")
	}
	return d.mockDispatcher.SendMsg(msg)
}

func lastLog(t *testing.T, rt *NodeRuntime, instanceID string) WFLogEntry {
	t.Helper()
	logs, err := rt.Store.GetRecentLog(context.Background(), instanceID, 1)
	if err != nil || len(logs) == 0 {
		t.Fatalf("log of %s: %v %v", instanceID, logs, err)
	}
	return logs[0]
}

// FINDINGS A-118: a panic in a transition fails that instance (transition_panic), never the node:
// the next instance takes its events as before, and the event that panicked is not left to panic
// again at the next start.
func TestAPanicInATransitionFailsOnlyThatInstance(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	doomed := startAsking(t, rt, disp, "trace-1")
	healthy := startAsking(t, rt, disp, "trace-2")
	rt.ActCtx.Dispatcher = &panickingDispatcher{panicAt: "AI.panic@motherbee"}

	boom := from("AI.host@motherbee", msgWithName("BOOM"))
	boom.Meta.ThreadID = &doomed
	err := Dispatch(context.Background(), boom, rt)
	if err == nil || !strings.Contains(err.Error(), causeTransitionPanic) {
		t.Fatalf("the panic is the dispatch's error, for the loop's log: %v", err)
	}
	if row := mustGetInstance(t, rt, doomed); row.Status != "failed" || row.CurrentState != "failed" {
		t.Fatalf("the instance that panicked: %+v", row)
	}
	if entry := lastLog(t, rt, doomed); entry.OK || !strings.HasPrefix(entry.ErrorDetail, causeTransitionPanic+": a send that panics") {
		t.Fatalf("its log names the cause: %+v", entry)
	}
	if events, _ := rt.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("the event that panicked is consumed: %+v", events)
	}
	if _, running := rt.Registry.Get(doomed); running {
		t.Fatal("the failed instance left the registry")
	}

	answer := from("AI.host@motherbee", msgWithNameAndPayload("ASK", map[string]any{"text": "ok"}))
	answer.Meta.ThreadID = &healthy
	if err := Dispatch(context.Background(), answer, rt); err != nil {
		t.Fatalf("the other instance goes on: %v", err)
	}
	if row := mustGetInstance(t, rt, healthy); row.CurrentState != "answered" {
		t.Fatalf("the other instance: %+v", row)
	}
}

// FINDINGS A-118: a CEL map literal in set_variable panicked the node when the state was written.
// The state holds plain JSON now, live and after a restart; what JSON cannot hold fails the action
// (and, by its on_error, the instance), never the node.
func TestACELMapIsKeptAsPlainJSON(t *testing.T) {
	const profiling = `{
  "wf_schema_version": "1",
  "workflow_type": "profiling",
  "input_schema": {"type": "object"},
  "initial_state": "collecting",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "collecting",
      "transitions": [
        {"event_match": {"msg": "PROFILE"}, "guard": "true", "target_state": "collected",
         "actions": [{"type": "set_variable", "name": "profile", "value": "{'name': event.payload.name, 'scores': [1, 2.5], 'meta': {'ok': true}}"}]},
        {"event_match": {"msg": "BAD"}, "guard": "true", "target_state": "collected",
         "actions": [{"type": "set_variable", "name": "profile", "value": "{1: 'a'}"}]}
      ]
    },
    {
      "name": "collected",
      "transitions": [
        {"event_match": {"msg": "CHECK"}, "guard": "state.profile.scores[1] > 2.0 && state.profile.meta.ok", "target_state": "done"}
      ]
    },
    {"name": "done"},
    {"name": "failed"}
  ]
}`
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, _ := newTestRuntime(t, path, profiling, nil)
	start := func() string {
		t.Helper()
		return startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	}
	send := func(instanceID, name string, payload map[string]any) {
		t.Helper()
		msg := from("AI.host@motherbee", msgWithNameAndPayload(name, payload))
		msg.Meta.ThreadID = &instanceID
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
	}

	good := start()
	send(good, "PROFILE", map[string]any{"name": "Ana"})
	inst, _ := rt.Registry.Get(good)
	want := map[string]any{"name": "Ana", "scores": []any{int64(1), 2.5}, "meta": map[string]any{"ok": true}}
	if !reflect.DeepEqual(inst.StateVars["profile"], want) {
		t.Fatalf("live: %#v", inst.StateVars["profile"])
	}

	bad := start()
	send(bad, "BAD", nil)
	if row := mustGetInstance(t, rt, bad); row.Status != "failed" || !strings.Contains(lastLog(t, rt, bad).ErrorDetail, "a map key must be a string") {
		t.Fatalf("a value JSON cannot hold fails the instance: %+v %+v", row, lastLog(t, rt, bad))
	}

	restarted, _ := newTestRuntime(t, path, profiling, &mockTimerLister{})
	if err := restarted.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	inst, _ = restarted.Registry.Get(good)
	if !reflect.DeepEqual(inst.StateVars["profile"], want) {
		t.Fatalf("after a restart: %#v", inst.StateVars["profile"])
	}
	rt = restarted
	send(good, "CHECK", nil)
	if row := mustGetInstance(t, rt, good); row.CurrentState != "done" {
		t.Fatalf("the guard reads the kept map: %+v", row)
	}
}

// FINDINGS A-118: an emit_internal_event that emits itself again never stopped, and held the node's
// only receive loop. Past maxInternalEventsPerEvent internal events for one event, the instance
// fails (internal_event_cap) and the node goes on.
func TestTheInternalEventCapFailsTheInstance(t *testing.T) {
	const looping = `{
  "wf_schema_version": "1",
  "workflow_type": "looping",
  "input_schema": {"type": "object"},
  "initial_state": "looping",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "looping",
      "entry_actions": [{"type": "emit_internal_event", "meta": {"msg": "TICK"}}],
      "transitions": [
        {"event_match": {"msg": "TICK"}, "guard": "true", "target_state": "looping",
         "actions": [{"type": "set_variable", "name": "ticks", "value": "has(state.ticks) ? state.ticks + 1 : 1"}]}
      ]
    },
    {"name": "done"}
  ]
}`
	rt, _ := newTestRuntime(t, "", looping, nil)
	err := Dispatch(context.Background(), from("IO.in@motherbee", msgWithName("START")), rt)
	if err != nil {
		t.Fatalf("the cap fails the instance, not the dispatch: %v", err)
	}
	rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0)
	if len(rows) != 1 || rows[0].Status != "failed" {
		t.Fatalf("instances: %+v", rows)
	}
	if rows[0].StateJSON != `{"ticks":100}` {
		t.Fatalf("it ran the most internal events one event may lead to: %s", rows[0].StateJSON)
	}
	if entry := lastLog(t, rt, rows[0].InstanceID); !strings.HasPrefix(entry.ErrorDetail, causeInternalEventCap+": 100 internal events") {
		t.Fatalf("its log names the cause: %+v", entry)
	}
	if queued, _ := rt.Store.ListInternalEvents(context.Background(), rows[0].InstanceID); len(queued) != 0 {
		t.Fatalf("its queue is cleared: %d left", len(queued))
	}
	if rt.Registry.Count() != 0 {
		t.Fatal("the failed instance left the registry")
	}
}

// FINDINGS A-118: numbers were float64 live and int64 after a restart, so a guard could work after a
// restart and fail before it. A JSON integer is an int and any other number a double, in the input,
// the state and the event, live and after a restart.
func TestNumbersKeepOneTypeLiveAndAfterARestart(t *testing.T) {
	const counting = `{
  "wf_schema_version": "1",
  "workflow_type": "numbers",
  "input_schema": {"type": "object", "properties": {"n": {"type": "integer"}, "x": {"type": "number"}}},
  "initial_state": "counting",
  "terminal_states": ["checked"],
  "states": [
    {
      "name": "counting",
      "transitions": [
        {"event_match": {"msg": "SET"}, "guard": "event.payload.step == 1 && event.payload.scale > 1.5", "target_state": "set",
         "actions": [
           {"type": "set_variable", "name": "count", "value": "input.n + event.payload.step"},
           {"type": "set_variable", "name": "ratio", "value": "input.x * event.payload.scale"}
         ]}
      ]
    },
    {
      "name": "set",
      "transitions": [
        {"event_match": {"msg": "CHECK"}, "guard": "input.n > 2 && input.x >= 2.0 && state.count % 2 == 0 && state.ratio > 3.5", "target_state": "checked"}
      ]
    },
    {"name": "checked"}
  ]
}`
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, _ := newTestRuntime(t, path, counting, nil)
	start := func() string {
		t.Helper()
		// Built as the JSON a sender writes: Go's encoder would write 2.0 as 2.
		trigger := from("IO.in@motherbee", msgWithName("START"))
		trigger.Payload = json.RawMessage(`{"n": 3, "x": 2.0}`)
		return startInstance(t, rt, trigger)
	}
	send := func(instanceID, name, payload string) {
		t.Helper()
		msg := from("AI.host@motherbee", msgWithName(name))
		msg.Payload = json.RawMessage(payload)
		msg.Meta.ThreadID = &instanceID
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
	}
	types := func(instanceID string) []string {
		t.Helper()
		inst, ok := rt.Registry.Get(instanceID)
		if !ok {
			t.Fatalf("%s is not running", instanceID)
		}
		return []string{
			reflect.TypeOf(inst.Input["n"]).String(), reflect.TypeOf(inst.Input["x"]).String(),
			reflect.TypeOf(inst.StateVars["count"]).String(), reflect.TypeOf(inst.StateVars["ratio"]).String(),
		}
	}
	want := []string{"int64", "float64", "int64", "float64"}

	live, restarted := start(), start()
	send(live, "SET", `{"step": 1, "scale": 2.0}`)
	send(restarted, "SET", `{"step": 1, "scale": 2.0}`)
	if got := types(live); !reflect.DeepEqual(got, want) {
		t.Fatalf("live: %v", got)
	}
	if row := mustGetInstance(t, rt, live); row.InputJSON != `{"n":3,"x":2.0}` || row.StateJSON != `{"count":4,"ratio":4.0}` {
		t.Fatalf("a double keeps its decimal point in the store: %s %s", row.InputJSON, row.StateJSON)
	}
	send(live, "CHECK", `{}`)
	if row := mustGetInstance(t, rt, live); row.CurrentState != "checked" {
		t.Fatalf("the guard holds live: %+v", row)
	}

	rt, _ = newTestRuntime(t, path, counting, &mockTimerLister{})
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	if got := types(restarted); !reflect.DeepEqual(got, want) {
		t.Fatalf("after a restart: %v", got)
	}
	send(restarted, "CHECK", `{}`)
	if row := mustGetInstance(t, rt, restarted); row.CurrentState != "checked" {
		t.Fatalf("the guard holds after a restart: %+v", row)
	}
}

// A panic outside an instance's transition is logged by the receive loop, which goes on.
func TestThePanicGuardOfTheReceiveLoopKeepsTheNode(t *testing.T) {
	rt, _ := newTestRuntime(t, "", askingWorkflowJSON, nil)
	rt.ActCtx.Dispatcher = &panickingDispatcher{panicAt: "caller-uuid"}
	request := from(sdk.PrimaryAdminNode, msgWithName(MsgWFHelp))
	request.Routing.Src = "caller-uuid"
	err := dispatchGuarded(context.Background(), request, rt)
	if err == nil || !strings.Contains(err.Error(), "a send that panics") {
		t.Fatalf("the panic is the dispatch's error: %v", err)
	}
}

// replayWorkflowJSON works on GO, with a send, and goes on to an internal event, with another.
const replayWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "replay",
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done"],
  "states": [
    {"name": "waiting", "transitions": [
      {"event_match": {"msg": "GO"}, "guard": "true", "target_state": "working",
       "actions": [
         {"type": "send_message", "target": "IO.out@motherbee", "meta": {"msg": "WORK"}, "payload": {"step": "event"}},
         {"type": "emit_internal_event", "meta": {"msg": "NEXT"}}
       ]}
    ]},
    {"name": "working", "transitions": [
      {"event_match": {"msg": "NEXT"}, "guard": "true", "target_state": "done",
       "actions": [{"type": "send_message", "target": "IO.out@motherbee", "meta": {"msg": "WORK"}, "payload": {"step": "internal"}}]}
    ]},
    {"name": "done"}
  ]
}`

// sendHook calls onSend at each send, before the send.
type sendHook struct {
	mockDispatcher
	onSend func(sdk.Message)
}

func (d *sendHook) SendMsg(msg sdk.Message) error {
	d.onSend(msg)
	return d.mockDispatcher.SendMsg(msg)
}

// journalGo journals a GO for instanceID as a start that crashed in its run leaves it, at attempts.
func journalGo(t *testing.T, rt *NodeRuntime, instanceID string, attempts int) {
	t.Helper()
	msg := from("AI.host@motherbee", msgWithName("GO"))
	msg.Meta.ThreadID = &instanceID
	id, err := rt.journal(context.Background(), instanceID, inboundEvent, msg, "")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := rt.Store.db.Exec(`UPDATE wf_inbound_events SET attempts = ? WHERE event_id = ?;`, attempts, id); err != nil {
		t.Fatal(err)
	}
}

// queueNext queues a NEXT for instanceID as a start that crashed in its run leaves it, at attempts.
func queueNext(t *testing.T, rt *NodeRuntime, instanceID string, attempts int) {
	t.Helper()
	if err := rt.Store.EnqueueInternalEvents(context.Background(), []InternalEventRow{{
		InstanceID: instanceID, MsgType: "user", MsgName: "NEXT", PayloadJSON: `{}`, TraceID: "trace-next", CreatedAtMS: 1000,
	}}); err != nil {
		t.Fatal(err)
	}
	if _, err := rt.Store.db.Exec(`UPDATE wf_internal_events SET attempts = ? WHERE instance_id = ?;`, attempts, instanceID); err != nil {
		t.Fatal(err)
	}
}

// restart is a new runtime on rt's store and definition, recovered, with every send seen by onSend.
func restart(t *testing.T, path, definition string, onSend func(sdk.Message)) (*NodeRuntime, *sendHook) {
	t.Helper()
	rt, _ := newTestRuntime(t, path, definition, &mockTimerLister{})
	hook := &sendHook{mockDispatcher: mockDispatcher{l2name: testNodeName, uuid: "wf-uuid"}, onSend: onSend}
	rt.ActCtx.Dispatcher = hook
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	return rt, hook
}

// FINDINGS A-118 (review): an event that takes the node down beyond the panic guard (a fatal error,
// an OOM) was run again at every start, so the node never came up. A journaled event, a creation or
// a leftover internal event already run at maxReplayAttempts starts is not run again: its instance
// fails (replay_exhausted), with no action run, and the node goes on.
func TestAnEventRunAtThreeStartsIsNotRunAgain(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, _ := newTestRuntime(t, path, replayWorkflowJSON, nil)
	putRunningInstance(t, rt, "wfi:poisoned-event", "waiting")
	journalGo(t, rt, "wfi:poisoned-event", maxReplayAttempts)
	putRunningInstance(t, rt, "wfi:poisoned-internal", "working")
	queueNext(t, rt, "wfi:poisoned-internal", maxReplayAttempts)
	putRunningInstance(t, rt, "wfi:healthy", "waiting")
	created, err := rt.journal(context.Background(), "wfi:never-committed", inboundCreate, from("IO.in@motherbee", msgWithName("START")), "")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := rt.Store.db.Exec(`UPDATE wf_inbound_events SET attempts = ? WHERE event_id = ?;`, maxReplayAttempts, created); err != nil {
		t.Fatal(err)
	}

	after, hook := restart(t, path, replayWorkflowJSON, func(sdk.Message) {})
	if len(hook.sent) != 0 {
		t.Fatalf("a poisoned event ran again: %d sends", len(hook.sent))
	}
	for _, id := range []string{"wfi:poisoned-event", "wfi:poisoned-internal"} {
		if row := mustGetInstance(t, after, id); row.Status != "failed" {
			t.Fatalf("%s: %+v", id, row)
		}
		if entry := lastLog(t, after, id); !strings.HasPrefix(entry.ErrorDetail, causeReplayExhausted) {
			t.Fatalf("%s: its log names the cause: %+v", id, entry)
		}
	}
	if _, err := after.Store.GetInstance(context.Background(), "wfi:never-committed"); err == nil {
		t.Fatal("the poisoned creation started its instance")
	}
	events, _ := after.Store.ListInboundEvents(context.Background())
	internal, _ := after.Store.ListAllInternalEvents(context.Background())
	if len(events) != 0 || len(internal) != 0 {
		t.Fatalf("the poisoned events are gone: %d journaled, %d internal", len(events), len(internal))
	}

	// The node goes on.
	healthy := "wfi:healthy"
	goMsg := from("AI.host@motherbee", msgWithName("GO"))
	goMsg.Meta.ThreadID = &healthy
	if err := Dispatch(context.Background(), goMsg, after); err != nil {
		t.Fatal(err)
	}
	if row := mustGetInstance(t, after, healthy); row.CurrentState != "done" || len(hook.sent) != 2 {
		t.Fatalf("the other instance: %+v, %d sends", row, len(hook.sent))
	}
}

// A start counts the attempt of a journaled event or a leftover internal event, in its own commit,
// before it runs it: a crash in the run leaves the count. A live event counts none.
func TestAStartCountsAnAttemptBeforeItRunsAnEventAgain(t *testing.T) {
	t.Run("a journaled event", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "wf.db")
		rt, _ := newTestRuntime(t, path, replayWorkflowJSON, nil)
		putRunningInstance(t, rt, "wfi:again", "waiting")
		journalGo(t, rt, "wfi:again", 1)
		var seen []string
		after, _ := restart(t, path, replayWorkflowJSON, attemptsIn(t, openedStore(t, path), "wfi:again", &seen))
		// The event's own internal event comes of this start's run: it counts none.
		if strings.Join(seen, "; ") != "journaled event: 2; internal NEXT: 0" {
			t.Fatalf("the attempts the store held at each send: %v", seen)
		}
		if row := mustGetInstance(t, after, "wfi:again"); row.CurrentState != "done" {
			t.Fatalf("the event ran: %+v", row)
		}
	})
	t.Run("a leftover internal event", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "wf.db")
		rt, _ := newTestRuntime(t, path, replayWorkflowJSON, nil)
		putRunningInstance(t, rt, "wfi:again", "working")
		queueNext(t, rt, "wfi:again", 2)
		var seen []string
		after, _ := restart(t, path, replayWorkflowJSON, attemptsIn(t, openedStore(t, path), "wfi:again", &seen))
		if strings.Join(seen, "; ") != "internal NEXT: 3" {
			t.Fatalf("the attempts the store held at the send: %v", seen)
		}
		if row := mustGetInstance(t, after, "wfi:again"); row.CurrentState != "done" {
			t.Fatalf("the internal event ran: %+v", row)
		}
	})
	t.Run("a live event", func(t *testing.T) {
		rt, _ := newTestRuntime(t, "", replayWorkflowJSON, nil)
		putRunningInstance(t, rt, "wfi:live", "waiting")
		if err := rt.Recover(context.Background(), recoverOpts); err != nil {
			t.Fatal(err)
		}
		var seen []string
		rt.ActCtx.Dispatcher = &sendHook{mockDispatcher: mockDispatcher{l2name: testNodeName}, onSend: attemptsIn(t, rt.Store, "wfi:live", &seen)}
		live := "wfi:live"
		goMsg := from("AI.host@motherbee", msgWithName("GO"))
		goMsg.Meta.ThreadID = &live
		if err := Dispatch(context.Background(), goMsg, rt); err != nil {
			t.Fatal(err)
		}
		if strings.Join(seen, "; ") != "journaled event: 0; internal NEXT: 0" {
			t.Fatalf("the attempts the store held at each send: %v", seen)
		}
	})
}

// attemptsIn records, at each send, the attempts store holds for instanceID's journaled and internal
// events.
func attemptsIn(t *testing.T, store *Store, instanceID string, seen *[]string) func(sdk.Message) {
	return func(sdk.Message) {
		events, err := store.ListInboundEvents(context.Background())
		if err != nil {
			t.Fatal(err)
		}
		for _, e := range events {
			if e.InstanceID == instanceID {
				*seen = append(*seen, fmt.Sprintf("journaled %s: %d", e.Kind, e.Attempts))
			}
		}
		internal, err := store.ListInternalEvents(context.Background(), instanceID)
		if err != nil {
			t.Fatal(err)
		}
		for _, e := range internal {
			*seen = append(*seen, fmt.Sprintf("internal %s: %d", e.MsgName, e.Attempts))
		}
	}
}

// openedStore opens the store at path a second time, to read it while another connection writes.
func openedStore(t *testing.T, path string) *Store {
	t.Helper()
	store, err := OpenStore(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return store
}

// A commit that failed (a database error) leaves its event journaled while the live instance holds
// what it did; the instance's next commit writes that too, so it consumes every journaled event of
// the instance up to its own. Before, it consumed only its own, and the next start ran the first
// event again on a state that already held it: a third CHARGE for two events.
func TestACommitConsumesTheEarlierEventsItHolds(t *testing.T) {
	const definition = `{
  "wf_schema_version": "1",
  "workflow_type": "inc",
  "input_schema": {"type": "object"},
  "initial_state": "counting",
  "terminal_states": ["done"],
  "states": [
    {"name": "counting", "transitions": [
      {"event_match": {"msg": "INC"}, "guard": "true", "target_state": "counting",
       "actions": [
         {"type": "set_variable", "name": "n", "value": "has(state.n) ? state.n + 1 : 1"},
         {"type": "send_message", "target": "IO.out@motherbee", "meta": {"msg": "CHARGE"}, "payload": {"n": {"$ref": "state.n"}}}
       ]}
    ]},
    {"name": "done"}
  ]
}`
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, disp := newTestRuntime(t, path, definition, nil)
	id := startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	inc := func(trace string) error {
		msg := from("AI.host@motherbee", msgWithName("INC"))
		msg.Meta.ThreadID = &id
		msg.Routing.TraceID = trace
		return Dispatch(context.Background(), msg, rt)
	}
	// The first INC's commit fails once.
	if _, err := rt.Store.db.Exec(`CREATE TRIGGER fail_once BEFORE UPDATE ON wf_instances
WHEN NEW.state_json = '{"n":1}' BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;`); err != nil {
		t.Fatal(err)
	}
	if err := inc("trace-inc-1"); err == nil {
		t.Fatal("the first commit fails")
	}
	if _, err := rt.Store.db.Exec(`DROP TRIGGER fail_once;`); err != nil {
		t.Fatal(err)
	}
	if err := inc("trace-inc-2"); err != nil {
		t.Fatal(err)
	}
	if events, _ := rt.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("the second commit consumed both events: %d left", len(events))
	}
	if len(disp.sent) != 2 {
		t.Fatalf("two charges: %d", len(disp.sent))
	}

	after, hook := restart(t, path, definition, func(sdk.Message) {})
	if row := mustGetInstance(t, after, id); row.StateJSON != `{"n":2}` || len(hook.sent) != 0 {
		t.Fatalf("after a restart: state %s, %d charges", row.StateJSON, len(hook.sent))
	}
}

// A definition's number literals go on the wire as written, in a payload, a meta.context and a $ref's
// default: 3 as 3 (decoded as a float64 it went out as 3.0, which a Rust u64 field refuses), 2.5 as
// 2.5 and 1.0 as 1.0. Its input_schema keeps its numbers as written too.
func TestADefinitionsNumbersGoOnTheWireAsWritten(t *testing.T) {
	const definition = `{
  "wf_schema_version": "1",
  "workflow_type": "numbers",
  "input_schema": {"type": "object", "properties": {"limit": {"type": "integer", "maximum": 10}}},
  "initial_state": "sending",
  "terminal_states": ["sent"],
  "states": [
    {
      "name": "sending",
      "entry_actions": [
        {"type": "send_message", "target": "IO.out@motherbee",
         "meta": {"msg": "NUMBERS", "context": {"retries": 3, "backoff": 1.5}},
         "payload": {"count": 3, "ratio": 2.5, "one": 1.0, "list": [1, 2.0], "limit": {"$ref": "input.limit", "default": 7}}}
      ],
      "transitions": [{"event_match": {"msg": "NUMBERS"}, "guard": "true", "target_state": "sent"}]
    },
    {"name": "sent"}
  ]
}`
	rt, disp := newTestRuntime(t, "", definition, nil)
	startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	startInstance(t, rt, from("IO.in@motherbee", msgWithNameAndPayload("START", map[string]any{"limit": 10})))
	if len(disp.sent) != 2 {
		t.Fatalf("sent %d", len(disp.sent))
	}
	if got := string(disp.sent[0].Payload); got != `{"count":3,"limit":7,"list":[1,2.0],"one":1.0,"ratio":2.5}` {
		t.Fatalf("payload on the wire: %s", got)
	}
	if got := string(disp.sent[0].Meta.Context); got != `{"backoff":1.5,"retries":3}` {
		t.Fatalf("meta.context on the wire: %s", got)
	}
	if got := string(disp.sent[1].Payload); got != `{"count":3,"limit":10,"list":[1,2.0],"one":1.0,"ratio":2.5}` {
		t.Fatalf("payload on the wire: %s", got)
	}
	// The schema's maximum, 10, refuses 11.
	if err := Dispatch(context.Background(), from("IO.in@motherbee", msgWithNameAndPayload("START", map[string]any{"limit": 11})), rt); err != nil {
		t.Fatal(err)
	}
	if rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 2 {
		t.Fatalf("the input_schema refuses 11: %d instances", len(rows))
	}
}

// FINDINGS A-118 (review): a guard past its timeout went on running in its own goroutine, reading
// the instance's state while the transition chosen next wrote it: a fatal concurrent map access, which
// the panic guard does not catch, and, journaled, again at every start. A guard runs on the receive
// loop now, stopped at its timeout.
func TestASlowGuardNeverRacesTheNextTransition(t *testing.T) {
	writes := make([]string, 100)
	for i := range writes {
		writes[i] = fmt.Sprintf(`{"type": "set_variable", "name": "v%d", "value": "%d"}`, i, i)
	}
	definition := `{
  "wf_schema_version": "1",
  "workflow_type": "slowguard",
  "input_schema": {"type": "object"},
  "initial_state": "loading",
  "terminal_states": ["done"],
  "states": [
    {"name": "loading", "transitions": [
      {"event_match": {"msg": "LOAD"}, "guard": "true", "target_state": "deciding",
       "actions": [{"type": "set_variable", "name": "items", "value": "event.payload.items"}]}
    ]},
    {"name": "deciding", "transitions": [
      {"event_match": {"msg": "DECIDE"}, "guard": "state.items.all(a, state.items.all(b, a + b < 100000000)) && false", "target_state": "done"},
      {"event_match": {"msg": "DECIDE"}, "guard": "true", "target_state": "deciding", "actions": [` + strings.Join(writes, ", ") + `]}
    ]},
    {"name": "done"}
  ]
}`
	rt, _ := newTestRuntime(t, "", definition, nil)
	items := make([]int, 1500)
	for i := range items {
		items[i] = i
	}
	id := startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
	send := func(name string, payload any) {
		t.Helper()
		msg := from("AI.host@motherbee", msgWithName(name))
		msg.Meta.ThreadID = &id
		msg.Payload, _ = json.Marshal(payload)
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	send("LOAD", map[string]any{"items": items})
	for i := 0; i < 3; i++ {
		send("DECIDE", map[string]any{})
	}
	if row := mustGetInstance(t, rt, id); row.CurrentState != "deciding" || row.Status != "running" {
		t.Fatalf("the slow guard is false, and the other transition runs: %+v", row)
	}
}
