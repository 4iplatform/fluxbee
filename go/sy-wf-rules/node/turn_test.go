package node

import (
	"context"
	"encoding/json"
	"reflect"
	"strings"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// configOperationMessage is SY.admin's CONFIG_SET to SY.wf-rules, its config as given.
func configOperationMessage(t *testing.T, traceID string, config map[string]any) fluxbeesdk.Message {
	t.Helper()
	payload, err := json.Marshal(map[string]any{
		"node_name": "SY.wf-rules@motherbee", "schema_version": 1, "config_version": 0, "apply_mode": "replace", "config": config,
	})
	if err != nil {
		t.Fatal(err)
	}
	admin, configSet := fluxbeesdk.PrimaryAdminNode, fluxbeesdk.MSGConfigSet
	return fluxbeesdk.Message{
		Routing: fluxbeesdk.Routing{Src: "admin-uuid", SrcL2Name: &admin, TraceID: traceID},
		Meta:    fluxbeesdk.Meta{MsgType: fluxbeesdk.SYSTEMKind, Msg: &configSet},
		Payload: payload,
	}
}

// adminRequest is SY.admin's command or query to SY.wf-rules.
func adminRequest(t *testing.T, kind, action string, payload map[string]any) fluxbeesdk.Message {
	t.Helper()
	raw, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}
	admin := fluxbeesdk.PrimaryAdminNode
	return fluxbeesdk.Message{
		Routing: fluxbeesdk.Routing{Src: "admin-uuid", SrcL2Name: &admin, TraceID: "t-" + action},
		Meta:    fluxbeesdk.Meta{MsgType: kind, Action: &action},
		Payload: raw,
	}
}

// refusal is an answer's error code and whether it says asking again may pass: a CONFIG_RESPONSE's
// error.code, or a command's or a query's error_code.
func refusal(t *testing.T, answer fluxbeesdk.Message) (string, bool) {
	t.Helper()
	var payload struct {
		ErrorCode string `json:"error_code"`
		Error     *struct {
			Code string `json:"code"`
		} `json:"error"`
		Retryable bool `json:"retryable"`
	}
	if err := json.Unmarshal(answer.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	if payload.Error != nil {
		return payload.Error.Code, payload.Retryable
	}
	return payload.ErrorCode, payload.Retryable
}

// FINDINGS A-307 — SY.wf-rules takes one request at a time, and a change gets its 60 s from its
// turn: a change whose turn comes with less than 20 s of SY.admin's 75 s wait left (it waited more
// than 55 s behind other requests) is refused at once, BUSY and retryable, nothing changed. Before,
// it ran, and could end past SY.admin's wait, which answered TIMEOUT for a change that may still
// complete. Each change is refused so: a compile (a CONFIG_SET with no operation is one), an apply,
// a rollback, a compile_apply and a delete, as a CONFIG_SET operation and as a command.
func TestAChangeWhoseTurnComesLateIsRefusedBusy(t *testing.T) {
	definition := validWorkflowDefinition()
	operation := func(config map[string]any) func(*testing.T) fluxbeesdk.Message {
		return func(t *testing.T) fluxbeesdk.Message { return configOperationMessage(t, "t-late", config) }
	}
	command := func(action string, payload map[string]any) func(*testing.T) fluxbeesdk.Message {
		return func(t *testing.T) fluxbeesdk.Message { return adminRequest(t, "command", action, payload) }
	}
	for _, tc := range []struct {
		name string
		msg  func(*testing.T) fluxbeesdk.Message
	}{
		{"compile", operation(map[string]any{"operation": "compile", "workflow_name": "invoice", "definition": definition})},
		{"a CONFIG_SET with no operation, a compile", operation(map[string]any{"workflow_name": "invoice", "definition": definition})},
		{"compile_apply", operation(map[string]any{"operation": "compile_apply", "workflow_name": "invoice", "definition": definition})},
		{"apply", operation(map[string]any{"operation": "apply", "workflow_name": "invoice"})},
		{"rollback", operation(map[string]any{"operation": "rollback", "workflow_name": "invoice"})},
		{"compile_workflow", command("compile_workflow", map[string]any{"workflow_name": "invoice", "definition": definition})},
		{"apply_workflow", command("apply_workflow", map[string]any{"workflow_name": "invoice"})},
		{"rollback_workflow", command("rollback_workflow", map[string]any{"workflow_name": "invoice"})},
		{"delete_workflow", command("delete_workflow", map[string]any{"workflow_name": "invoice", "force": true})},
	} {
		t.Run(tc.name, func(t *testing.T) {
			svc := newTestService(t)
			// A workflow with a version current, one before it and one staged: each change has
			// something to change.
			for range 2 {
				applyWorkflowForDelete(t, svc)
			}
			if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: definition}); err != nil {
				t.Fatal(err)
			}
			admin := svc.admin.(*fakeAdminClient)
			publishes := admin.publishCalls
			svc.nodes = &fakeNodeClient{killNodeFunc: func(context.Context, string, bool, bool) (map[string]any, error) {
				t.Fatal("a refused delete kills nothing")
				return nil, nil
			}}
			before := snapshotTree(t, svc.store.Root(), svc.cfg.DistRuntimeRoot)
			sender := &recordingSender{}
			svc.sender = sender

			svc.handleTurn(tc.msg(t), 55*time.Second+time.Millisecond)

			if len(sender.sent) != 1 {
				t.Fatalf("want one answer, got %d", len(sender.sent))
			}
			if code, retryable := refusal(t, sender.sent[0]); code != "BUSY" || !retryable {
				t.Fatalf("the late change is answered %q, retryable %v: %s", code, retryable, sender.sent[0].Payload)
			}
			// The detail states the rule: 20 s is what must be left of SY.admin's wait at the turn,
			// not what a change is given (it is given its 60 s from its turn).
			if detail := string(sender.sent[0].Payload); !strings.Contains(detail, "turn came 55.001s after it arrived") ||
				!strings.Contains(detail, "19.999s left of SY.admin's 1m15s wait for it, less than the 20s of it a change needs left at its turn") ||
				!strings.Contains(detail, "nothing was changed") {
				t.Fatalf("the refusal's detail: %s", detail)
			}
			if after := snapshotTree(t, svc.store.Root(), svc.cfg.DistRuntimeRoot); !reflect.DeepEqual(before, after) || admin.publishCalls != publishes {
				t.Fatalf("a refused change changes nothing: the tree changed %v, %d publishes", !reflect.DeepEqual(before, after), admin.publishCalls-publishes)
			}
		})
	}
}

// FINDINGS A-307 — a change whose turn comes with 20 s of SY.admin's wait left runs; a read (a check,
// a query, CONFIG_GET, NODE_STATUS_GET) is never refused for its turn, however long it waited.
func TestAChangeWithItsTurnInTimeRunsAndAReadAlways(t *testing.T) {
	svc := newTestService(t)
	sender := &recordingSender{}
	svc.sender = sender
	definition := validWorkflowDefinition()
	svc.handleTurn(configOperationMessage(t, "t-compile", map[string]any{"operation": "compile", "workflow_name": "invoice", "definition": definition}), 55*time.Second)
	if code, _ := refusal(t, sender.sent[0]); code != "" {
		t.Fatalf("a change with 20 s left runs: %s", sender.sent[0].Payload)
	}
	if _, err := svc.store.ReadStagedMetadata("invoice"); err != nil {
		t.Fatalf("the compile staged nothing: %v", err)
	}

	configGet, statusGet := fluxbeesdk.MSGConfigGet, fluxbeesdk.MSGNodeStatusGet
	admin := fluxbeesdk.PrimaryAdminNode
	reads := []fluxbeesdk.Message{
		wfRulesCheck(t, admin, "check-late", "invoice", definition),
		adminRequest(t, "query", "get_workflow", map[string]any{"workflow_name": "invoice"}),
		adminRequest(t, "query", "list_workflows", map[string]any{}),
		adminRequest(t, "query", "get_status", map[string]any{"workflow_name": "invoice"}),
		{Routing: fluxbeesdk.Routing{Src: "admin-uuid", SrcL2Name: &admin, TraceID: "t-config-get"}, Meta: fluxbeesdk.Meta{MsgType: fluxbeesdk.SYSTEMKind, Msg: &configGet}},
		{Routing: fluxbeesdk.Routing{Src: "orchestrator-uuid", TraceID: "t-status"}, Meta: fluxbeesdk.Meta{MsgType: fluxbeesdk.SYSTEMKind, Msg: &statusGet}},
	}
	for _, read := range reads {
		sent := len(sender.sent)
		svc.handleTurn(read, 80*time.Second)
		if len(sender.sent) != sent+1 {
			t.Fatalf("%s %s: no answer", read.Meta.MsgType, stringValue(read.Meta.Action)+stringValue(read.Meta.Msg))
		}
		if code, _ := refusal(t, sender.sent[sent]); code == "BUSY" {
			t.Fatalf("a read is refused for its turn: %s", sender.sent[sent].Payload)
		}
	}
}

// FINDINGS A-307's review — the zero rule, no wait (a Service NewService did not build), refuses
// nothing, however long a change waited: counted against a wait of 0 s, every change that waited at
// all would be refused BUSY.
func TestTheZeroTurnRuleRefusesNothing(t *testing.T) {
	for _, waited := range []time.Duration{0, time.Millisecond, 55 * time.Second, 10 * time.Minute} {
		if err := (turnRule{}).late("SY.wf-rules@motherbee", waited); err != nil {
			t.Fatalf("the zero rule refused a change that waited %s: %v", waited, err)
		}
	}
	svc := newTestService(t)
	svc.turns = turnRule{}
	sender := &recordingSender{}
	svc.sender = sender
	svc.handleTurn(configOperationMessage(t, "t-compile", map[string]any{"operation": "compile", "workflow_name": "invoice", "definition": validWorkflowDefinition()}), 10*time.Minute)
	if code, _ := refusal(t, sender.sent[0]); code != "" {
		t.Fatalf("a Service with the zero rule refused a change: %s", sender.sent[0].Payload)
	}
}

// chanSender is a sender the service's loop sends through from its own goroutine: each answer
// reaches the test on sent.
type chanSender struct{ sent chan fluxbeesdk.Message }

func (c chanSender) Send(msg fluxbeesdk.Message) error { c.sent <- msg; return nil }
func (c chanSender) UUID() string                      { return "wf-rules-uuid" }
func (c chanSender) FullName() string                  { return "SY.wf-rules@motherbee" }

func (c chanSender) next(t *testing.T) fluxbeesdk.Message {
	t.Helper()
	select {
	case msg := <-c.sent:
		return msg
	case <-time.After(10 * time.Second):
		t.Fatal("no answer")
		return fluxbeesdk.Message{}
	}
}

// FINDINGS A-307 — the loop counts a change's turn from when the request reached SY.wf-rules, not
// from when the change before it ended: a change that came while another ran, and whose turn left
// it less than its share of SY.admin's wait, is refused BUSY at once, nothing changed; one that
// comes when nothing runs runs. (The rule is shortened here: SY.admin waits 2 s, a change needs 1 s.)
func TestTheLoopCountsATurnFromTheRequestsArrival(t *testing.T) {
	svc := newTestService(t)
	svc.turns = turnRule{adminWait: 2 * time.Second, minLeft: time.Second}
	svc.nodes = &fakeNodeClient{}
	admin := svc.admin.(*fakeAdminClient)
	publish := admin.publishRuntimePackageFunc
	release := make(chan struct{})
	admin.publishRuntimePackageFunc = func(ctx context.Context, files map[string]string) (*PackagePublishResult, error) {
		<-release
		return publish(ctx, files)
	}
	sender := chanSender{sent: make(chan fluxbeesdk.Message, 4)}
	svc.sender = sender
	incoming := make(chan fluxbeesdk.Message, 4)
	svc.incoming = incoming
	ctx, cancel := context.WithCancel(context.Background())
	served := make(chan struct{})
	go func() {
		defer close(served)
		_ = svc.RunWithContext(ctx)
	}()
	defer func() {
		cancel()
		<-served
	}()

	deploy := func(trace string) fluxbeesdk.Message {
		return configOperationMessage(t, trace, map[string]any{"operation": "compile_apply", "workflow_name": "invoice", "definition": validWorkflowDefinition()})
	}
	incoming <- deploy("t-first")
	incoming <- deploy("t-second")
	// The first deploy's publish holds it 1.2 s: the second's turn comes with 0.8 s of the 2 s left.
	time.Sleep(1200 * time.Millisecond)
	close(release)
	for _, want := range []struct {
		trace, code string
	}{{"t-first", ""}, {"t-second", "BUSY"}} {
		answer := sender.next(t)
		if code, _ := refusal(t, answer); answer.Routing.TraceID != want.trace || code != want.code {
			t.Fatalf("%s answered %q, want %s answered %q: %s", answer.Routing.TraceID, code, want.trace, want.code, answer.Payload)
		}
	}
	if admin.publishCalls != 1 {
		t.Fatalf("the refused deploy published: %d publishes", admin.publishCalls)
	}
	// Asked again with nothing ahead of it, it runs.
	incoming <- deploy("t-again")
	if answer := sender.next(t); answer.Routing.TraceID != "t-again" {
		t.Fatalf("answered %s", answer.Routing.TraceID)
	} else if code, _ := refusal(t, answer); code != "" {
		t.Fatalf("the deploy asked again is refused %q: %s", code, answer.Payload)
	}
	if current, err := svc.store.ReadCurrentMetadata("invoice"); err != nil || current.Version != 2 {
		t.Fatalf("the deploy asked again made version 2 current: %+v, %v", current, err)
	}
}
