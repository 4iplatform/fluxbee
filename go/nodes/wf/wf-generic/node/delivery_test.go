package node

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// askingWorkflowJSON asks AI.host@motherbee, gives up when its timer fires, and tells IO.out of the
// answer.
const askingWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "asking",
  "description": "asks an agent; gives up when its timer fires",
  "start_from": ["IO.in@motherbee", "IO.api.billing@motherbee", "WF.upstream@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["answered", "timed_out", "failed"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [
        {"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {"text": "hi"}},
        {"type": "schedule_timer", "timer_key": "ask_timeout", "fire_in": "2m"}
      ],
      "transitions": [
        {"event_match": {"msg": "ASK"}, "guard": "true", "target_state": "answered",
         "actions": [{"type": "send_message", "target": "IO.out@motherbee", "meta": {"msg": "ANSWER"}, "payload": {"$ref": "event.payload"}}]},
        {"event_match": {"msg": "BOOM"}, "guard": "true", "target_state": "answered",
         "actions": [{"type": "send_message", "target": "AI.panic@motherbee", "meta": {"msg": "BOOM"}}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'ask_timeout'", "target_state": "timed_out"}
      ]
    },
    {"name": "answered"},
    {"name": "timed_out"},
    {"name": "failed"}
  ]
}`

// startAsking starts an instance of askingWorkflowJSON on rt, on a conversation of its own, and
// returns its id; threadOf gives its thread.
func startAsking(t *testing.T, rt *NodeRuntime, disp *mockDispatcher, traceID string) string {
	t.Helper()
	trigger := from("IO.in@motherbee", msgWithNameAndPayload("START", map[string]any{"customer_id": "c1"}))
	trigger.Routing.TraceID = traceID
	sent := len(disp.sent)
	instanceID := startInstance(t, rt, trigger)
	if len(disp.sent) != sent+1 {
		t.Fatalf("the creation asks the agent: sent %d", len(disp.sent)-sent)
	}
	return instanceID
}

// realTimerFired is SY.timer's TIMER_FIRED of the timer timerKey of instanceID, which rt's index has
// pending: SY.timer fires it at the fire_at the WF scheduled it for.
func realTimerFired(t *testing.T, rt *NodeRuntime, instanceID, timerKey string) sdk.Message {
	t.Helper()
	timer, pending, err := rt.Store.GetTimerForInstance(context.Background(), instanceID, timerKey)
	if err != nil || !pending {
		t.Fatalf("instance %s has no timer %s pending: %v", instanceID, timerKey, err)
	}
	return timerFiredAt(t, instanceID, timerKey, timer.FireAtMS)
}

// timerFiredAt is SY.timer's TIMER_FIRED as the router delivers it (the shared wire fixture of the
// Go and Rust SDKs, go/fluxbee-go-sdk/testdata/timer_wire), for a WF timer scheduled for scheduledMS:
// SY.timer hands back the client_ref and the payload the WF scheduled it with, and the time it was
// scheduled for (emitTimerFired in go/sy-timer/scheduler.go).
func timerFiredAt(t *testing.T, instanceID, timerKey string, scheduledMS int64) sdk.Message {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "..", "fluxbee-go-sdk", "testdata", "timer_wire", "event_timer_fired.json"))
	if err != nil {
		t.Fatal(err)
	}
	var msg sdk.Message
	if err := json.Unmarshal(raw, &msg); err != nil {
		t.Fatal(err)
	}
	var payload map[string]any
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	payload["owner_l2_name"] = testNodeName
	payload["client_ref"] = timerClientRef(instanceID, timerKey)
	payload["user_payload"] = map[string]any{"instance_id": instanceID, "timer_key": timerKey}
	payload["scheduled_fire_at_utc_ms"] = scheduledMS
	payload["actual_fire_at_utc_ms"] = scheduledMS + 123
	if msg.Payload, err = json.Marshal(payload); err != nil {
		t.Fatal(err)
	}
	msg.Routing.Dst = sdk.UnicastDestination(testNodeName)
	return msg
}

// FINDINGS A-117: SY.timer puts the key at payload.user_payload.timer_key, and no guard of the
// examples or the docs read it there. The engine exposes it as event.timer_key; event.payload stays
// as SY.timer sent it.
func TestARealTimerFiredReachesAnEventTimerKeyGuard(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	fired := realTimerFired(t, rt, instanceID, "ask_timeout")
	if fired.SourceL2Name() != "SY.timer@motherbee" || rawMsgName(fired) != sdk.MsgTimerFired {
		t.Fatalf("fixture: %+v", fired.Routing)
	}

	event := messageToMap(fired)
	payload := event["payload"].(map[string]any)
	userPayload := payload["user_payload"].(map[string]any)
	if event["timer_key"] != "ask_timeout" || userPayload["timer_key"] != "ask_timeout" || payload["timer_key"] != nil {
		t.Fatalf("event = %v", event)
	}

	if err := Dispatch(context.Background(), fired, rt); err != nil {
		t.Fatalf("Dispatch TIMER_FIRED: %v", err)
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "timed_out" || row.Status != "completed" {
		t.Fatalf("the timeout must fire: %+v", row)
	}
}

// FINDINGS A-119: a TIMER_FIRED is taken only from SY.timer of the WF's own hive; anyone else could
// fire an instance's timer by its client_ref. A refused fire is never answered.
func TestAForgedTimerFiredIsRefused(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	sent := len(disp.sent)
	for _, forger := range []string{"AI.evil@motherbee", "SY.timer@worker1", "WF.other@motherbee"} {
		forged := realTimerFired(t, rt, instanceID, "ask_timeout")
		forged.Routing.SrcL2Name = &forger
		if err := Dispatch(context.Background(), forged, rt); err != nil {
			t.Fatalf("Dispatch from %s: %v", forger, err)
		}
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "asking" {
		t.Fatalf("a forged fire moved the instance: %+v", row)
	}
	if len(disp.sent) != sent {
		t.Fatalf("a refused fire is never answered: %v", disp.sent[sent:])
	}
	if events, _ := rt.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("a refused fire is never journaled: %+v", events)
	}
}

// FINDINGS A-119: the engine's control verbs answer only SY.admin@motherbee and SY.wf-rules of the
// WF's own hive; anyone else gets UNAUTHORIZED, and a cancel from them cancels nothing.
func TestTheControlVerbsAnswerOnlyTheAdminAndTheWFRulesOfTheHive(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	request := func(verb, sender string) map[string]any {
		t.Helper()
		msg := from(sender, msgWithNameAndPayload(verb, map[string]any{"instance_id": instanceID}))
		sent := len(disp.sent)
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatalf("%s from %s: %v", verb, sender, err)
		}
		if len(disp.sent) != sent+1 {
			t.Fatalf("%s from %s: %d replies", verb, sender, len(disp.sent)-sent)
		}
		reply := disp.sent[len(disp.sent)-1]
		if got := derefString(reply.Meta.Msg); got != wfControlRequests[verb] {
			t.Fatalf("%s from %s answered %q", verb, sender, got)
		}
		return lastReply(t, disp)
	}
	verbs := []string{MsgWFHelp, MsgWFGetInstance, MsgWFListInstances, MsgWFGetCode, MsgWFCancelInstance}
	for _, verb := range verbs {
		for _, sender := range []string{"AI.host@motherbee", "SY.wf-rules@worker1", "SY.admin@worker1", "SY.orchestrator@motherbee", "WF.other@motherbee"} {
			if reply := request(verb, sender); reply["ok"] != false || reply["error"] != errCodeUnauthorized {
				t.Fatalf("%s from %s = %v", verb, sender, reply)
			}
		}
	}
	if row := mustGetInstance(t, rt, instanceID); row.Status != "running" {
		t.Fatalf("an unauthorized cancel cancelled: %+v", row)
	}
	for _, verb := range verbs[:4] {
		for _, sender := range []string{sdk.PrimaryAdminNode, "SY.wf-rules@motherbee"} {
			if reply := request(verb, sender); reply["ok"] != true {
				t.Fatalf("%s from %s = %v", verb, sender, reply)
			}
		}
	}
	if reply := request(MsgWFCancelInstance, "SY.wf-rules@motherbee"); reply["ok"] != true || reply["status"] != "cancelled" {
		t.Fatalf("cancel = %v", reply)
	}
}

// FINDINGS A-112/A-119: a router notice and a reply (a *_RESPONSE, the engine's NOT_TAKEN, an
// agent's <msg>.error) are never answered, and a reply never starts an instance either, even when the
// schema would take it: two WFs cannot bounce a NOT_TAKEN. A message the schema refuses gets a
// NOT_TAKEN invalid_input: from an IO node, named or not (wf-orchestration-v1 §5.4.6); from anyone
// else, only a named request.
func TestErrorsRepliesAndNoticesAreNeverAnswered(t *testing.T) {
	strict := strings.Replace(askingWorkflowJSON, `"input_schema": {"type": "object"}`, `"input_schema": {"type": "object", "required": ["customer_id"]}`, 1)
	for name, definition := range map[string]string{"permissive": askingWorkflowJSON, "strict": strict} {
		t.Run(name, func(t *testing.T) {
			rt, disp := newTestRuntime(t, "", definition, nil)
			unreachable := sdk.MSGUnreachable
			cases := map[string]sdk.Message{
				"another WF's NOT_TAKEN": onThread("conv-1", from("WF.upstream@motherbee", msgWithNameAndPayload(msgNotTaken, map[string]any{"ok": false, "error": "not_taken", "reason": "invalid_input", "detail": "x"}))),
				"a WF's *_RESPONSE":      onThread("conv-1", from("WF.upstream@motherbee", msgWithNameAndPayload("WF_LIST_INSTANCES_RESPONSE", map[string]any{"ok": false, "error": "UNAUTHORIZED"}))),
				"an IO node's reply":     onThread("conv-1", from("IO.in@motherbee", msgWithNameAndPayload("QUOTE_RESPONSE", map[string]any{"ok": true}))),
				"an agent's error":       onThread("conv-1", from("AI.host@motherbee", msgWithNameAndPayload("ASK.error", map[string]any{"code": "x"}))),
				"a router notice":        {Routing: sdk.Routing{Src: "router-uuid", TraceID: "t-notice"}, Meta: sdk.Meta{MsgType: "system", Msg: &unreachable}, Payload: json.RawMessage(`{"reason":"NODE_NOT_FOUND"}`)},
			}
			for label, msg := range cases {
				if err := Dispatch(context.Background(), msg, rt); err != nil {
					t.Fatalf("%s: %v", label, err)
				}
				if len(disp.sent) != 0 || rt.Registry.Count() != 0 {
					t.Fatalf("%s: answered %v, started %d instances", label, disp.sent, rt.Registry.Count())
				}
			}
			if rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 0 {
				t.Fatalf("a reply started an instance: %+v", rows)
			}

			// A message without a name (an IO node's text) starts an instance, as a named request
			// does. Refused, an IO node's message gets a NOT_TAKEN, named or not, and a WF's only
			// when it is a named request.
			unnamed := func(sender, src, thread string) sdk.Message {
				return onThread(thread, from(sender, sdk.Message{Routing: sdk.Routing{Src: src, TraceID: "t-" + thread}, Meta: sdk.Meta{MsgType: "user"}, Payload: json.RawMessage(`{"text":"hello"}`)}))
			}
			named := func(sender, src, thread string) sdk.Message {
				msg := onThread(thread, from(sender, msgWithNameAndPayload("START", map[string]any{"text": "hello"})))
				msg.Routing.Src = src
				return msg
			}
			for _, msg := range []sdk.Message{
				unnamed("IO.in@motherbee", "io-uuid", "conv-text"),
				named("IO.api.billing@motherbee", "billing-uuid", "conv-named"),
				unnamed("WF.upstream@motherbee", "upstream-text-uuid", "conv-upstream-text"),
				named("WF.upstream@motherbee", "upstream-uuid", "conv-upstream"),
			} {
				if err := Dispatch(context.Background(), msg, rt); err != nil {
					t.Fatal(err)
				}
			}
			if name == "permissive" {
				if rt.Registry.Count() != 4 {
					t.Fatalf("texts and requests start instances: %d", rt.Registry.Count())
				}
				return
			}
			var answered []string
			for _, msg := range disp.sent {
				if notTakenReason(t, msg) != notTakenInvalidInput {
					t.Fatalf("a refused message gets a NOT_TAKEN invalid_input: %+v", msg)
				}
				answered = append(answered, msg.Routing.Dst.Value()+" "+derefString(msg.Meta.ThreadID))
			}
			if rt.Registry.Count() != 0 || strings.Join(answered, ", ") != "io-uuid conv-text, billing-uuid conv-named, upstream-uuid conv-upstream" {
				t.Fatalf("the IO nodes' messages and the WF's named request get a NOT_TAKEN: %v", answered)
			}
		})
	}
}

// FINDINGS A-135: a message with no thread starts no instance, since a WF never makes up a thread. An
// IO node gets a NOT_TAKEN no_thread for any message, and shows it to whoever wrote; any other sender
// only for a named request (wf-orchestration-v1 §5.4.10), the answer on the request's trace. A WF or
// an agent acts on none: a WF drops a system message but a TIMER_FIRED, and ai.generic ignores a
// system message it does not know.
func TestAMessageWithNoThreadStartsNothing(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	unnamed := from("WF.upstream@motherbee", sdk.Message{Routing: sdk.Routing{Src: "upstream-uuid", TraceID: "t-unnamed"}, Meta: sdk.Meta{MsgType: "user"}, Payload: json.RawMessage(`{}`)})
	named := from("WF.upstream@motherbee", msgWithNameAndPayload("START", map[string]any{"customer_id": "c1"}))
	named.Routing.Src = "upstream-uuid"
	agent := from("AI.host@motherbee", msgWithNameAndPayload("ASK", map[string]any{"text": "an answer"}))
	agent.Routing.Src = "agent-uuid"
	text := from("IO.in@motherbee", sdk.Message{Routing: sdk.Routing{Src: "io-uuid", TraceID: "t-text"}, Meta: sdk.Meta{MsgType: "user"}, Payload: json.RawMessage(`{"text":"hi"}`)})
	for _, msg := range []sdk.Message{unnamed, named, agent, text} {
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	var answered []string
	for _, msg := range disp.sent {
		if notTakenReason(t, msg) != notTakenNoThread || msg.Meta.ThreadID != nil {
			t.Fatalf("a NOT_TAKEN no_thread, on no thread: %+v %s", msg, msg.Payload)
		}
		answered = append(answered, msg.Routing.Dst.Value())
	}
	if rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 0 || strings.Join(answered, ", ") != "upstream-uuid, agent-uuid, io-uuid" {
		t.Fatalf("nothing starts, and the named requests and the IO node's text get a NOT_TAKEN: %d instances, answered %v", len(rows), answered)
	}
}

// The default's NOT_TAKEN is no action of the definition: when it cannot go, the node's log says so,
// and the instance stays where it was, with the event consumed.
func TestANotTakenThatCannotGoFailsNothing(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	disp.sendErr = errors.New("sender queue full")
	odd := from("IO.in@motherbee", msgWithName("ODD"))
	odd.Routing.Src = "io-uuid"
	odd.Meta.ThreadID = threadOf(t, rt, instanceID)
	var err error
	logged := nodeLog(t, func() { err = Dispatch(context.Background(), odd, rt) })
	if err != nil || !strings.Contains(logged, "instance "+instanceID+": NOT_TAKEN not sent: sender queue full") {
		t.Fatalf("the node's log says the NOT_TAKEN did not go: %v %s", err, logged)
	}
	if row := mustGetInstance(t, rt, instanceID); row.Status != "running" || row.CurrentState != "asking" {
		t.Fatalf("the instance stays where it was: %+v", row)
	}
	if events, _ := rt.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("the event is consumed: %+v", events)
	}
}

// The default's NOT_TAKEN is a notice, no action of the definition: it goes once the commit consumed
// the event. A commit that fails sends none, and the start that runs the event again sends it once.
// Before, it went ahead of the commit, and a crash between the two sent it twice.
func TestTheDefaultsNotTakenGoesOnceItsEventIsConsumed(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	rt, disp := newTestRuntime(t, path, askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	odd := func(trace string) sdk.Message {
		msg := from("IO.in@motherbee", msgWithName("ODD"))
		msg.Routing.Src, msg.Routing.TraceID = "io-uuid", trace
		msg.Meta.ThreadID = threadOf(t, rt, instanceID)
		return msg
	}
	// journaled records, at each NOT_TAKEN, its trace and how many events the journal holds.
	var journaled []string
	seen := func(store *Store) func(sdk.Message) {
		return func(msg sdk.Message) {
			if notTakenReason(t, msg) != notTakenNotExpected {
				return
			}
			events, err := store.ListInboundEvents(context.Background())
			if err != nil {
				t.Fatal(err)
			}
			journaled = append(journaled, fmt.Sprintf("%s: %d", msg.Routing.TraceID, len(events)))
		}
	}
	rt.ActCtx.Dispatcher = &sendHook{mockDispatcher: mockDispatcher{l2name: testNodeName, uuid: "wf-uuid"}, onSend: seen(rt.Store)}

	if err := Dispatch(context.Background(), odd("trace-odd-1"), rt); err != nil {
		t.Fatal(err)
	}
	// The commit of the next one fails: its event stays journaled, and no NOT_TAKEN goes.
	if _, err := rt.Store.db.Exec(`CREATE TRIGGER fail_commit BEFORE UPDATE ON wf_instances BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;`); err != nil {
		t.Fatal(err)
	}
	if err := Dispatch(context.Background(), odd("trace-odd-2"), rt); err == nil {
		t.Fatal("the commit fails")
	}
	if _, err := rt.Store.db.Exec(`DROP TRIGGER fail_commit;`); err != nil {
		t.Fatal(err)
	}
	if strings.Join(journaled, ", ") != "trace-odd-1: 0" {
		t.Fatalf("a NOT_TAKEN goes only with its event consumed: %v", journaled)
	}

	// The next start runs the event again: its commit goes, and then its NOT_TAKEN, once.
	restart(t, path, askingWorkflowJSON, seen(openedStore(t, path)))
	if strings.Join(journaled, ", ") != "trace-odd-1: 0, trace-odd-2: 0" {
		t.Fatalf("the event run again sends its NOT_TAKEN once, consumed: %v", journaled)
	}
}

// snoozingWorkflowJSON reminds IO.in when its timer remind fires, 5 minutes after the start; SNOOZE
// moves the timer to 10 minutes from then, and STOP cancels it.
const snoozingWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "snoozing",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["reminded"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "remind", "fire_in": "5m"}],
      "transitions": [
        {"event_match": {"msg": "SNOOZE"}, "guard": "true", "actions": [{"type": "reschedule_timer", "timer_key": "remind", "fire_in": "10m"}]},
        {"event_match": {"msg": "STOP"}, "guard": "true", "actions": [{"type": "cancel_timer", "timer_key": "remind"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'remind'", "target_state": "reminded",
         "actions": [{"type": "send_message", "target": "IO.in@motherbee", "meta": {"msg": "REMINDER"}, "payload": {"text": "time"}}]}
      ]
    },
    {"name": "reminded"}
  ]
}`

// TIMER_CANCEL and TIMER_RESCHEDULE go fire-and-forget: a fire on its way when the instance cancelled
// or moved its timer, or one SY.timer sends after a cancel or a reschedule it never got, still comes.
// The engine takes a fire only for the timer the instance has in the index, at the time it has it
// (a little earlier too: SY.timer counts a fire_in from when it gets the schedule). The fire of a
// cancelled timer is dropped, and so is one scheduled for a time the instance moved its timer from:
// that timer is scheduled again at its fire_at, since SY.timer fires a timer once. The node's log says
// why, and the instance's log keeps nothing of a dropped fire.
func TestAFireOfATimerTheInstanceCancelledOrMovedIsDropped(t *testing.T) {
	timer := &mockTimerSender{}
	rt, disp := newTestRuntime(t, "", snoozingWorkflowJSON, timer)
	dispatch := func(msg sdk.Message) {
		t.Helper()
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	start := func(thread string) (string, int64) {
		t.Helper()
		instanceID := startInstance(t, rt, ioMessage(thread, "", `{}`))
		remind, _, err := rt.Store.GetTimerForInstance(context.Background(), instanceID, "remind")
		if err != nil || remind.FireAtMS != fixedClock().Add(5*time.Minute).UnixMilli() {
			t.Fatalf("the timer remind, 5 minutes ahead: %+v %v", remind, err)
		}
		return instanceID, remind.FireAtMS
	}
	// dropped dispatches fired, and checks that the engine dropped it, saying why: the instance stays
	// waiting, its log keeps nothing of the fire, and nothing is sent.
	dropped := func(instanceID string, fired sdk.Message, why string) {
		t.Helper()
		logged, sent := lastLog(t, rt, instanceID), len(disp.sent)
		out := nodeLog(t, func() { dispatch(fired) })
		if !strings.Contains(out, `TIMER_FIRED "remind" for instance `+instanceID+" dropped") || !strings.Contains(out, why) {
			t.Fatalf("the node's log says the fire was dropped (%s): %s", why, out)
		}
		if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "waiting" || lastLog(t, rt, instanceID) != logged || len(disp.sent) != sent {
			t.Fatalf("a dropped fire reached the instance: %+v %+v", row, lastLog(t, rt, instanceID))
		}
	}
	reminded := func(instanceID string) {
		t.Helper()
		if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "reminded" {
			t.Fatalf("the timer's own fire reaches the instance: %+v", row)
		}
	}

	// The timer's own fire.
	onTime, _ := start("conv-on-time")
	dispatch(realTimerFired(t, rt, onTime, "remind"))
	reminded(onTime)

	// Cancelled: the fire SY.timer sends at the time the timer had is dropped.
	cancelled, at := start("conv-cancelled")
	dispatch(ioMessage("conv-cancelled", "STOP", `{}`))
	dropped(cancelled, timerFiredAt(t, cancelled, "remind", at), "the instance has no such timer")

	// Moved: four minutes in, SNOOZE moves the timer to 14 minutes from the start. A fire at the time
	// it had is dropped, and the timer is scheduled again for when it fires now; a fire within the
	// tolerance before that is the timer's own.
	moved, at := start("conv-moved")
	rt.ActCtx.Clock = func() time.Time { return fixedClock().Add(4 * time.Minute) }
	dispatch(ioMessage("conv-moved", "SNOOZE", `{}`))
	schedules := len(timer.scheduled)
	dropped(moved, timerFiredAt(t, moved, "remind", at), "the instance moved the timer to")
	remind, pending, _ := rt.Store.GetTimerForInstance(context.Background(), moved, "remind")
	if !pending || remind.FireAtMS != fixedClock().Add(14*time.Minute).UnixMilli() ||
		len(timer.scheduled) != schedules+1 || timer.scheduled[schedules] != timerClientRef(moved, "remind") {
		t.Fatalf("the moved timer is scheduled again at its fire_at: %+v %v", remind, timer.scheduled)
	}
	dispatch(timerFiredAt(t, moved, "remind", remind.FireAtMS-timerFireTolerance.Milliseconds()))
	reminded(moved)
}

// crashingDispatcher records what it sends, and closes the store's database right after a send to
// crashAt: the node dies between a send and the commit of its transition.
type crashingDispatcher struct {
	mockDispatcher
	store   *Store
	crashAt string
}

func (d *crashingDispatcher) SendMsg(msg sdk.Message, note sdk.TraceNote) error {
	if err := d.mockDispatcher.SendMsg(msg, note); err != nil {
		return err
	}
	if msg.Routing.Dst.Value() == d.crashAt {
		_ = d.store.db.Close()
	}
	return nil
}

// At least once (FINDINGS A-118): an event is journaled before its transition runs, and the
// transition's commit consumes it. A crash in between leaves it, and recovery runs it again: its
// send goes again, with the same trace_id.
func TestAnEventLeftByACrashRunsAgainWithItsTraceID(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	before, disp := newTestRuntime(t, path, askingWorkflowJSON, nil)
	instanceID := startAsking(t, before, disp, "trace-start")
	crash := &crashingDispatcher{store: before.Store, crashAt: "IO.out@motherbee"}
	before.ActCtx.Dispatcher = crash

	answer := from("AI.host@motherbee", msgWithNameAndPayload("ASK", map[string]any{"text": "the answer"}))
	answer.Meta.ThreadID = threadOf(t, before, instanceID)
	answer.Routing.TraceID = "trace-answer"
	if err := Dispatch(context.Background(), answer, before); err == nil {
		t.Fatal("the commit cannot succeed on a closed store")
	}
	if len(crash.sent) != 1 || crash.sent[0].Routing.TraceID != "trace-answer" {
		t.Fatalf("the transition sent before the crash: %+v", crash.sent)
	}

	after, disp := newTestRuntime(t, path, askingWorkflowJSON, &mockTimerLister{})
	if err := after.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	if len(disp.sent) != 1 || disp.sent[0].Routing.Dst.Value() != "IO.out@motherbee" || disp.sent[0].Routing.TraceID != "trace-answer" {
		t.Fatalf("the send goes again with its trace_id: %+v", disp.sent)
	}
	var payload map[string]any
	_ = json.Unmarshal(disp.sent[0].Payload, &payload)
	if payload["text"] != "the answer" {
		t.Fatalf("the event runs again as it came: %s", disp.sent[0].Payload)
	}
	if row := mustGetInstance(t, after, instanceID); row.CurrentState != "answered" {
		t.Fatalf("after recovery: %+v", row)
	}
	if events, _ := after.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("the journal is empty once the transition commits: %+v", events)
	}
}

// At least once, at creation: a crash between the journaling of the trigger and the commit of the
// new instance leaves no half-started instance; recovery creates it again, under the same
// instance_id, and its first send goes again with the trigger's trace_id.
func TestACreationLeftByACrashRunsAgainUnderItsInstanceID(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	before, _ := newTestRuntime(t, path, askingWorkflowJSON, nil)
	crash := &crashingDispatcher{store: before.Store, crashAt: "AI.host@motherbee"}
	before.ActCtx.Dispatcher = crash
	trigger := onThread("conv-1", from("IO.in@motherbee", msgWithNameAndPayload("START", map[string]any{"customer_id": "c1"})))
	trigger.Routing.TraceID = "trace-start"
	if err := Dispatch(context.Background(), trigger, before); err == nil {
		t.Fatal("the creation cannot commit on a closed store")
	}
	if len(crash.sent) != 1 {
		t.Fatalf("the creation asked before the crash: %+v", crash.sent)
	}

	after, disp := newTestRuntime(t, path, askingWorkflowJSON, &mockTimerLister{})
	if rows, _ := after.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 0 {
		t.Fatalf("no instance is half-started: %+v", rows)
	}
	journaled, _ := after.Store.ListInboundEvents(context.Background())
	if len(journaled) != 1 || journaled[0].Kind != inboundCreate {
		t.Fatalf("the creation is journaled: %+v", journaled)
	}
	instanceID := journaled[0].InstanceID
	if err := after.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatalf("Recover: %v", err)
	}
	if len(disp.sent) != 1 || derefString(disp.sent[0].Meta.ThreadID) != "conv-1" || disp.sent[0].Routing.TraceID != "trace-start" {
		t.Fatalf("the creation runs again on its thread, with its trace_id: %+v", disp.sent)
	}
	row := mustGetInstance(t, after, instanceID)
	if row.CurrentState != "asking" || row.Status != "running" || row.DefinitionHash != after.DefHash || row.ThreadID != "conv-1" {
		t.Fatalf("after recovery, under its instance_id: %+v", row)
	}
	if _, ok := after.Registry.Get(instanceID); !ok {
		t.Fatal("the recreated instance is running")
	}
	if events, _ := after.Store.ListInboundEvents(context.Background()); len(events) != 0 {
		t.Fatalf("the journal is empty once the creation commits: %+v", events)
	}
}

// FINDINGS A-118: a fired timer's index row goes when its fire is taken, so a restart (when SY.timer
// has nothing pending) does not fire it a second time.
func TestAFiredTimerDoesNotFireAgainAfterARestart(t *testing.T) {
	const counting = `{
  "wf_schema_version": "1",
  "workflow_type": "counting",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "waiting",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "tick", "fire_in": "2m"}],
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'tick'", "target_state": "fired",
         "actions": [{"type": "set_variable", "name": "fires", "value": "has(state.fires) ? state.fires + 1 : 1"}]}
      ]
    },
    {
      "name": "fired",
      "transitions": [
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'tick'", "target_state": "fired",
         "actions": [{"type": "set_variable", "name": "fires", "value": "state.fires + 1"}]},
        {"event_match": {"msg": "DONE"}, "guard": "true", "target_state": "done"}
      ]
    },
    {"name": "done"}
  ]
}`
	path := filepath.Join(t.TempDir(), "wf.db")
	before, _ := newTestRuntime(t, path, counting, nil)
	instanceID := startInstance(t, before, from("IO.in@motherbee", msgWithName("START")))
	if timers, _ := before.Store.ListTimersForInstance(context.Background(), instanceID); len(timers) != 1 {
		t.Fatalf("the timer is in the index: %+v", timers)
	}
	if err := Dispatch(context.Background(), realTimerFired(t, before, instanceID, "tick"), before); err != nil {
		t.Fatal(err)
	}
	if timers, _ := before.Store.ListTimersForInstance(context.Background(), instanceID); len(timers) != 0 {
		t.Fatalf("the fired timer left the index: %+v", timers)
	}

	// Ten minutes later the node restarts; SY.timer has nothing pending (the timer fired).
	after, _ := newTestRuntime(t, path, counting, &mockTimerLister{})
	after.ActCtx.Clock = func() time.Time { return fixedClock().Add(10 * time.Minute) }
	if err := after.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	row := mustGetInstance(t, after, instanceID)
	if row.CurrentState != "fired" || row.StateJSON != `{"fires":1}` {
		t.Fatalf("the timer fired once: %+v", row)
	}
}

// notifyAgentWorkflowJSON asks an agent, then tells it the outcome: the agent answers that too, on the
// instance's thread, with the request's msg, after the instance ended.
const notifyAgentWorkflowJSON = `{
  "wf_schema_version": "1",
  "workflow_type": "notify",
  "start_from": ["IO.in@motherbee"],
  "input_schema": {"type": "object"},
  "initial_state": "asking",
  "terminal_states": ["done"],
  "states": [
    {
      "name": "asking",
      "entry_actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "ASK"}, "payload": {"type": "text", "content": "evaluate"}}],
      "transitions": [
        {"event_match": {"msg": "ASK"}, "guard": "true", "target_state": "done",
         "actions": [{"type": "send_message", "target": "AI.host@motherbee", "meta": {"msg": "OUTCOME"}, "payload": {"type": "text", "content": "recorded"}}]}
      ]
    },
    {"name": "done"}
  ]
}`

// answerAs is an agent's answer to sent, as ai-generic builds it: the request's msg and thread, user.
func answerAs(agent string, sent sdk.Message, payload string) sdk.Message {
	return from(agent, sdk.Message{
		Routing: sdk.Routing{Src: "agent-uuid", TraceID: sent.Routing.TraceID},
		Meta:    sdk.Meta{MsgType: "user", Msg: sent.Meta.Msg, ThreadID: sent.Meta.ThreadID},
		Payload: json.RawMessage(payload),
	})
}

// An agent answers with the request's own msg and thread, so a late answer (its instance ended: its
// timer fired, it was cancelled or failed, or a first copy of the answer finished it) is no reply by
// name. It comes from a node the definition does not start from, so it is dropped and never
// answered (wf-orchestration-v1 §5.2.1, item 3): started as a new instance it would ask the agent
// again, a WF<->AI loop. A message from a start_from node (a WF that sends to this one, on its own
// instance's thread) starts one on its thread.
func TestALateAnswerNeverStartsAnInstance(t *testing.T) {
	t.Run("after its timeout", func(t *testing.T) {
		rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
		instanceID := startAsking(t, rt, disp, "trace-start")
		ask := disp.sent[len(disp.sent)-1]
		if err := Dispatch(context.Background(), realTimerFired(t, rt, instanceID, "ask_timeout"), rt); err != nil {
			t.Fatal(err)
		}
		sent := len(disp.sent)
		if err := Dispatch(context.Background(), answerAs("AI.host@motherbee", ask, `{"type":"text","content":"the late answer"}`), rt); err != nil {
			t.Fatal(err)
		}
		if rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 1 || len(disp.sent) != sent {
			t.Fatalf("the late answer started %d instances and sent %d messages", len(rows)-1, len(disp.sent)-sent)
		}
	})
	t.Run("the outcome loop", func(t *testing.T) {
		rt, disp := newTestRuntime(t, "", notifyAgentWorkflowJSON, nil)
		startInstance(t, rt, from("IO.in@motherbee", msgWithName("START")))
		// Play the agent: answer every message the WF sends it, as ai-generic does.
		for answered := 0; answered < len(disp.sent) && answered < 10; answered++ {
			if err := Dispatch(context.Background(), answerAs("AI.host@motherbee", disp.sent[answered], `{"type":"text","content":"ok"}`), rt); err != nil {
				t.Fatal(err)
			}
		}
		rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0)
		if len(rows) != 1 || len(disp.sent) != 2 || rows[0].CurrentState != "done" {
			t.Fatalf("the agent's answer to the outcome started instances: %d instances, %d sends", len(rows), len(disp.sent))
		}
	})
	t.Run("another node's instance on the thread", func(t *testing.T) {
		rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
		thread := "wfi:00000000-0000-4000-8000-000000000001"
		msg := from("WF.upstream@motherbee", msgWithName("START"))
		msg.Meta.ThreadID = &thread
		instanceID := startInstance(t, rt, msg)
		if len(disp.sent) != 1 || derefString(disp.sent[0].Meta.ThreadID) != thread {
			t.Fatalf("a WF's send starts an instance on this one, on its thread: %+v", disp.sent)
		}
		if row := mustGetInstance(t, rt, instanceID); row.ThreadID != thread || instanceID == thread {
			t.Fatalf("the instance: %+v", row)
		}
	})
}

// FINDINGS A-119: SY.timer lets any node schedule a timer whose fire goes to another node, under
// SY.timer's own name, so the sender check alone lets it through. A TIMER_FIRED is taken only for a
// timer this node scheduled (its owner_l2_name) and only by its client_ref.
func TestATimerFiredOfAnotherOwnerIsRefused(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	sent := len(disp.sent)
	forge := func(edit func(payload map[string]any)) sdk.Message {
		fired := realTimerFired(t, rt, instanceID, "ask_timeout")
		var payload map[string]any
		if err := json.Unmarshal(fired.Payload, &payload); err != nil {
			t.Fatal(err)
		}
		edit(payload)
		fired.Payload, _ = json.Marshal(payload)
		return fired
	}
	for name, fired := range map[string]sdk.Message{
		"scheduled by another node": forge(func(payload map[string]any) { payload["owner_l2_name"] = "AI.evil@motherbee" }),
		"no client_ref":             forge(func(payload map[string]any) { delete(payload, "client_ref") }),
	} {
		if err := Dispatch(context.Background(), fired, rt); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "asking" || len(disp.sent) != sent {
			t.Fatalf("%s: the fire moved the instance (%s) or was answered", name, row.CurrentState)
		}
	}
	// SY.timer's own fire of the WF's timer still reaches the guard.
	if err := Dispatch(context.Background(), realTimerFired(t, rt, instanceID, "ask_timeout"), rt); err != nil {
		t.Fatal(err)
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "timed_out" {
		t.Fatalf("the WF's own timer: %+v", row)
	}
}

// A system message is control, never a workflow event, but a TIMER_FIRED (the AI and IO nodes take
// the system kind only as control too): it neither starts an instance nor reaches one, and it is
// never answered.
func TestASystemMessageIsNoWorkflowEvent(t *testing.T) {
	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	instanceID := startAsking(t, rt, disp, "trace-start")
	sent := len(disp.sent)
	start := from("IO.api.billing@motherbee", msgWithName("START"))
	start.Meta.MsgType = "system"
	answer := from("AI.host@motherbee", msgWithNameAndPayload("ASK", map[string]any{"text": "an answer"}))
	answer.Meta.MsgType = "SYSTEM"
	answer.Meta.ThreadID = threadOf(t, rt, instanceID)
	for _, msg := range []sdk.Message{start, answer} {
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}
	rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0)
	if len(rows) != 1 || rows[0].CurrentState != "asking" || len(disp.sent) != sent {
		t.Fatalf("a system message acted: %d instances, state %s, %d sends", len(rows), rows[0].CurrentState, len(disp.sent)-sent)
	}
}

// FINDINGS A-218: the WF's notices are the AI nodes' and SY.timer's, name by name: what the router
// made (no src_l2_name), and from a node HELLO, ANNOUNCE, WITHDRAW, UNREACHABLE, TTL_EXCEEDED or a
// system message without a name. Another system message from a node, as a TIMER_HELP meant for
// SY.timer, is no notice, and is dropped unanswered all the same: it starts nothing and gets
// nothing, from a node the WF starts from too.
func TestTheNoticesAreTheAINodesAndSYTimers(t *testing.T) {
	system := func(name string) sdk.Message {
		msg := sdk.Message{
			Routing: sdk.Routing{Src: "io-uuid", TraceID: "t-" + name},
			Meta:    sdk.Meta{MsgType: sdk.SYSTEMKind},
			Payload: json.RawMessage(`{}`),
		}
		if name != "" {
			msg.Meta.Msg = &name
		}
		return onThread("conv-1", msg)
	}
	for _, name := range []string{sdk.MSGHello, sdk.MSGAnnounce, sdk.MSGWithdraw, sdk.MSGUnreachable, sdk.MSGTTLExceeded, ""} {
		if !isNotice(from("IO.in@motherbee", system(name))) {
			t.Fatalf("%q from a node is a notice", name)
		}
	}
	if !isNotice(system(sdk.MsgTimerHelp)) {
		t.Fatalf("a message the router made is a notice, whatever its name")
	}

	rt, disp := newTestRuntime(t, "", askingWorkflowJSON, nil)
	for _, name := range []string{sdk.MsgTimerHelp, sdk.MSGConfigChanged} {
		msg := from("IO.in@motherbee", system(name))
		if isNotice(msg) {
			t.Fatalf("%s from a node is no notice", name)
		}
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
	}
	if len(disp.sent) != 0 || rt.Registry.Count() != 0 {
		t.Fatalf("a system message from a node was answered %v or started %d instances", disp.sent, rt.Registry.Count())
	}
}
