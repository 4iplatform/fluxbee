package node

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"
)

// quietWorkflowJSON is validWorkflowJSON with idleTimeout as its idle_timeout (none when empty), and a
// goodbye in the cancelled state's entry that names the event's reason.
func quietWorkflowJSON(idleTimeout string) string {
	data := validWorkflowJSON()
	at := strings.LastIndex(data, `"entry_actions": [],`)
	data = data[:at] + `"entry_actions": [{"type": "send_message", "target": "IO.api.billing@motherbee", "meta": {"msg": "CLOSED"}, "payload": {"reason": {"$ref": "event.payload.reason"}}}],` + data[at+len(`"entry_actions": [],`):]
	if idleTimeout != "" {
		data = strings.Replace(data, `"workflow_type": "invoice",`, `"workflow_type": "invoice", "idle_timeout": "`+idleTimeout+`",`, 1)
	}
	return data
}

// A conversation that went quiet closes on its own (FINDINGS A-154): no event taken for its
// definition's idle_timeout (4 days when it names none) and no timer pending. It closes as a cancel
// does, its log tells why, and a conversation that waits for its own timer stays open.
func TestAQuietConversationClosesOnItsOwn(t *testing.T) {
	cases := []struct {
		name        string
		idleTimeout string
		later       time.Duration
		timer       bool
		closed      bool
		words       string
	}{
		{name: "quiet past the default", later: 4*24*time.Hour + time.Minute, closed: true, words: "4d"},
		{name: "quiet, short of the default", later: 4*24*time.Hour - time.Minute},
		{name: "quiet past the default, waiting for its timer", later: 30 * 24 * time.Hour, timer: true},
		{name: "quiet past its own idle_timeout", idleTimeout: "36h", later: 37 * time.Hour, closed: true, words: "36h"},
		{name: "quiet, short of its own idle_timeout", idleTimeout: "36h", later: 35 * time.Hour},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			def, err := LoadDefinitionBytes([]byte(quietWorkflowJSON(tc.idleTimeout)), "", fixedClock)
			if err != nil {
				t.Fatalf("load: %v", err)
			}
			disp := &mockDispatcher{l2name: "WF.invoice@motherbee"}
			timer := &mockTimerSender{}
			actx := makeActx(t, disp, timer)
			ctx := context.Background()
			inst := NewInstance(def, "wfi:quiet", "conv-quiet", def.WorkflowType, map[string]any{"customer_id": "cust-1"}, fixedClock)
			row, _ := inst.ToRow(fixedClock)
			if err := actx.Store.CreateInstance(ctx, row); err != nil {
				t.Fatalf("CreateInstance: %v", err)
			}
			if tc.timer {
				if err := actx.Store.RegisterTimer(ctx, inst.InstanceID, "invoice_timeout", nowMS(fixedClock), nowMS(fixedClock)+int64(31*24*time.Hour/time.Millisecond)); err != nil {
					t.Fatalf("RegisterTimer: %v", err)
				}
			}
			reg := NewInstanceRegistry()
			if err := reg.Add(inst); err != nil {
				t.Fatalf("Add: %v", err)
			}
			actx.Clock = func() time.Time { return fixedClock().Add(tc.later) }
			rt := &NodeRuntime{Def: def, Registry: reg, Store: actx.Store, ActCtx: actx, NodeName: "WF.invoice@motherbee"}

			rt.closeQuietInstances(ctx)

			stored, err := actx.Store.GetInstance(ctx, inst.InstanceID)
			if err != nil {
				t.Fatalf("GetInstance: %v", err)
			}
			_, held := reg.Get(inst.InstanceID)
			if !tc.closed {
				if stored.Status != "running" || !held || len(disp.sent) != 0 {
					t.Fatalf("an open conversation was closed: status %q, held %v, sent %d", stored.Status, held, len(disp.sent))
				}
				return
			}
			if stored.Status != "cancelled" || stored.CurrentState != "cancelled" || held {
				t.Fatalf("a quiet conversation was not closed: status %q, state %q, held %v", stored.Status, stored.CurrentState, held)
			}
			reason := "quiet for " + tc.words + " with no timer pending (idle_timeout)"
			if len(disp.sent) != 1 || derefString(disp.sent[0].Meta.Msg) != "CLOSED" {
				t.Fatalf("the cancelled state's entry actions did not run: %+v", disp.sent)
			}
			var goodbye map[string]any
			if err := json.Unmarshal(disp.sent[0].Payload, &goodbye); err != nil || goodbye["reason"] != reason {
				t.Fatalf("the event's reason: %s (%v)", disp.sent[0].Payload, err)
			}
			entries, err := actx.Store.GetRecentLog(ctx, inst.InstanceID, 10)
			if err != nil {
				t.Fatalf("GetRecentLog: %v", err)
			}
			logged := false
			for _, entry := range entries {
				logged = logged || (entry.ActionType == "idle_close" && entry.Summary == "closed: "+reason)
			}
			if !logged {
				t.Fatalf("the instance's log does not tell why: %+v", entries)
			}
		})
	}
}

// idle_timeout takes a duration from 1h to 7d (FINDINGS A-154).
func TestTheIdleTimeoutIsOneHourToSevenDays(t *testing.T) {
	for _, value := range []string{"1h", "90m", "36h", "4d", "7d", "168h"} {
		if _, err := LoadDefinitionBytes([]byte(quietWorkflowJSON(value)), "", fixedClock); err != nil {
			t.Fatalf("%s: %v", value, err)
		}
	}
	for value, want := range map[string]string{
		"59m":   "idle_timeout: must be 1h to 7d",
		"8d":    "idle_timeout: must be 1h to 7d",
		"169h":  "idle_timeout: must be 1h to 7d",
		"4days": `idle_timeout: must be a duration (e.g. "4d" or "36h")`,
	} {
		_, err := LoadDefinitionBytes([]byte(quietWorkflowJSON(value)), "", fixedClock)
		assertLoadErrorContains(t, err, want)
	}
}
