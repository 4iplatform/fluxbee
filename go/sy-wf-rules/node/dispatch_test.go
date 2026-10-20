package node

import (
	"errors"
	"strings"
	"testing"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// A reply refused as over the frame limit is logged and answered with the error reply its handler
// uses for other failures, so the admin does not wait out its timeout. Any other send failure (the
// queue full during a reconnect) is only logged: the work may be done, and the admin's timeout
// reads "unknown, not failed". The status reply has no error form and is only logged (A-75).
func TestARefusedReplyIsAnsweredWithAnError(t *testing.T) {
	admin := fluxbeesdk.PrimaryAdminNode
	listWorkflows, configGet, statusGet := "list_workflows", fluxbeesdk.MSGConfigGet, fluxbeesdk.MSGNodeStatusGet
	cases := []struct {
		name    string
		meta    fluxbeesdk.Meta
		sendErr error  // how the first send fails; nil = over the frame limit
		reply   string // the error reply's msg; empty when nothing replaces the failed reply
		failed  string // how the error reply says it failed
	}{
		{name: "query", meta: fluxbeesdk.Meta{MsgType: "query", Action: &listWorkflows}, reply: "LIST_WORKFLOWS_RESPONSE", failed: `"status":"error"`},
		{name: "config", meta: fluxbeesdk.Meta{MsgType: fluxbeesdk.SYSTEMKind, Msg: &configGet}, reply: fluxbeesdk.MSGConfigResponse, failed: `"ok":false`},
		{name: "status", meta: fluxbeesdk.Meta{MsgType: fluxbeesdk.SYSTEMKind, Msg: &statusGet}},
		{name: "queue full", meta: fluxbeesdk.Meta{MsgType: "query", Action: &listWorkflows}, sendErr: errors.New("sender queue full")},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			svc := newTestService(t)
			sender := &recordingSender{refuse: 1, err: tc.sendErr}
			svc.sender = sender
			svc.handleMessage(fluxbeesdk.Message{
				Routing: fluxbeesdk.Routing{Src: "caller-uuid", SrcL2Name: &admin, TraceID: "t-1"},
				Meta:    tc.meta,
			})
			if tc.reply == "" {
				if len(sender.sent) != 0 {
					t.Fatalf("sent %d messages after the failed reply, want none", len(sender.sent))
				}
				return
			}
			if len(sender.sent) != 1 {
				t.Fatalf("sent %d messages after the refused reply, want the error reply", len(sender.sent))
			}
			reply := sender.sent[0]
			body := string(reply.Payload)
			if stringValue(reply.Meta.Msg) != tc.reply || reply.Routing.TraceID != "t-1" ||
				!strings.Contains(body, tc.failed) || !strings.Contains(body, `"WFRULES_ERROR"`) || !strings.Contains(body, "message too large") {
				t.Fatalf("error reply %s on %s: %s", stringValue(reply.Meta.Msg), reply.Routing.TraceID, body)
			}
		})
	}
}
