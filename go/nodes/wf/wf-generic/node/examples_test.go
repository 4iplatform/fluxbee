package node

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func exampleDefinition(t *testing.T, pkg string) string {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "examples", "packages", pkg, "flow", "definition.json"))
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func lastSent(t *testing.T, disp *mockDispatcher) (sdk.Message, map[string]any) {
	t.Helper()
	if len(disp.sent) == 0 {
		t.Fatal("nothing sent")
	}
	msg := disp.sent[len(disp.sent)-1]
	var payload map[string]any
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	return msg, payload
}

// The wf.router example (0.1.90): it replies to the IO node a question came from by a node name
// written in the definition (one transition per IO node, no target_ref), its error branches match
// <msg>.error, and its timeouts fire on event.timer_key.
func TestTheRouterExampleRepliesToTheIONodeTheQuestionCameFrom(t *testing.T) {
	rt, disp := newTestRuntime(t, "", exampleDefinition(t, "wf.router"), nil)
	ask := func(io string) string {
		t.Helper()
		relay := from(io, sdk.Message{Routing: sdk.Routing{TraceID: "trace-" + io}, Meta: sdk.Meta{MsgType: "user"}, Payload: json.RawMessage(`{"type":"text","content":"where is my order?"}`)})
		relay.Meta.Context = json.RawMessage(`{"io":{"reply_target":{"kind":"api","address":"conv-1"}}}`)
		instanceID := startInstance(t, rt, relay)
		if msg, payload := lastSent(t, disp); msg.Routing.Dst.Value() != "AI.host@motherbee" || msg.Meta.MsgType != "user" || payload["content"] != "where is my order?" {
			t.Fatalf("the host is asked, with a user message: %+v %v", msg, payload)
		}
		return instanceID
	}
	answer := func(instanceID, agent, msgName, payload string) {
		t.Helper()
		reply := from(agent, msgWithName(msgName))
		reply.Meta.MsgType = "user"
		reply.Meta.ThreadID = &instanceID
		reply.Payload = json.RawMessage(payload)
		if err := Dispatch(context.Background(), reply, rt); err != nil {
			t.Fatal(err)
		}
	}
	failure := func(instanceID string) any {
		t.Helper()
		row := mustGetInstance(t, rt, instanceID)
		if row.Status != "failed" {
			t.Fatalf("%s: %+v", instanceID, row)
		}
		state, _ := unmarshalJSON(row.StateJSON)
		return state["failure_reason"]
	}

	viaAPI := ask("IO.api.support@motherbee")
	answer(viaAPI, "AI.host@motherbee", "HOST_TURN", `{"type":"text","content":"the answer"}`)
	msg, payload := lastSent(t, disp)
	if msg.Routing.Dst.Value() != "IO.api.support@motherbee" || derefString(msg.Meta.Msg) != "CUSTOMER_REPLY" || payload["content"] != "the answer" ||
		string(msg.Meta.Context) != `{"io":{"reply_target":{"address":"conv-1","kind":"api"}}}` {
		t.Fatalf("the answer goes back to IO.api with its reply context: %+v %s", msg, msg.Meta.Context)
	}
	if row := mustGetInstance(t, rt, viaAPI); row.CurrentState != "answered" {
		t.Fatalf("%+v", row)
	}

	viaWhatsApp := ask("IO.wapp.acme@motherbee")
	answer(viaWhatsApp, "AI.host@motherbee", "HOST_TURN", `{"type":"text","content":"ROUTE_SPECIALIST"}`)
	if msg, _ := lastSent(t, disp); msg.Routing.Dst.Value() != "AI.specialist@motherbee" {
		t.Fatalf("the host escalates: %+v", msg)
	}
	answer(viaWhatsApp, "AI.specialist@motherbee", "SPECIALIST_TURN", `{"type":"text","content":"the specialist's answer"}`)
	if msg, payload := lastSent(t, disp); msg.Routing.Dst.Value() != "IO.wapp.acme@motherbee" || payload["content"] != "the specialist's answer" {
		t.Fatalf("the specialist's answer goes back to IO.wapp: %+v %v", msg, payload)
	}

	errored := ask("IO.api.support@motherbee")
	answer(errored, "AI.host@motherbee", "HOST_TURN.error", `{"type":"error","code":"rate_limited","message":"slow down","retryable":true}`)
	if reason := failure(errored); reason != "host_error:rate_limited" {
		t.Fatalf("an agent's error arrives as <msg>.error: %v", reason)
	}

	timedOut := ask("IO.api.support@motherbee")
	if err := Dispatch(context.Background(), realTimerFired(t, timedOut, "host_timeout"), rt); err != nil {
		t.Fatal(err)
	}
	if reason := failure(timedOut); reason != "host_timeout" {
		t.Fatalf("the timeout fires on event.timer_key: %v", reason)
	}

	stranger := ask("IO.slack.ops@motherbee")
	sent := len(disp.sent)
	answer(stranger, "AI.host@motherbee", "HOST_TURN", `{"type":"text","content":"the answer"}`)
	if reason := failure(stranger); reason != "unknown_origin:IO.slack.ops@motherbee" || len(disp.sent) != sent {
		t.Fatalf("an IO node not written in the definition gets nothing: %v, sent %d", reason, len(disp.sent)-sent)
	}
}

// The wf.invoice example (0.1.90): a missing currency takes its default from the $ref (the engine
// never applied the JSON-Schema default it used to rely on), and its timeouts fire on
// event.timer_key.
func TestTheInvoiceExampleDefaultsTheCurrencyAndTimesOut(t *testing.T) {
	rt, disp := newTestRuntime(t, "", exampleDefinition(t, "wf.invoice"), nil)
	instanceID := startInstance(t, rt, from("AI.billing@motherbee", msgWithNameAndPayload("INVOICE_START", map[string]any{"customer_id": "c1", "amount_cents": 2500})))
	msg, payload := lastSent(t, disp)
	if msg.Routing.Dst.Value() != "IO.quickbooks@motherbee" || payload["currency"] != "USD" {
		t.Fatalf("the request: %+v %v", msg, payload)
	}
	// An integer goes on the wire as an integer: a Rust u64 field refuses 2500.0.
	if !strings.Contains(string(msg.Payload), `"amount_cents":2500,`) && !strings.Contains(string(msg.Payload), `"amount_cents":2500}`) {
		t.Fatalf("amount_cents on the wire: %s", msg.Payload)
	}

	if err := Dispatch(context.Background(), realTimerFired(t, instanceID, "collection_timeout"), rt); err != nil {
		t.Fatal(err)
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "failed" || row.Status != "failed" {
		t.Fatalf("the collection timeout fires: %+v", row)
	}
	if msg, payload := lastSent(t, disp); msg.Routing.Dst.Value() != "IO.notifications@motherbee" || payload["failure_reason"] != "collection_timeout" {
		t.Fatalf("the failure is notified: %+v %v", msg, payload)
	}
}
