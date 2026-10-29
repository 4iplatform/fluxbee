package node

import (
	"context"
	"encoding/json"
	"path/filepath"
	"strings"
	"testing"
)

// deployWorkflowJSON waits for GO and ends in done_<version>: two versions of one workflow that a
// redeploy swaps.
func deployWorkflowJSON(version string) string {
	return strings.ReplaceAll(`{
  "wf_schema_version": "1",
  "workflow_type": "deploy",
  "description": "VERSION",
  "input_schema": {"type": "object"},
  "initial_state": "waiting",
  "terminal_states": ["done_VERSION"],
  "states": [
    {"name": "waiting", "transitions": [{"event_match": {"msg": "GO"}, "guard": "true", "target_state": "done_VERSION"}]},
    {"name": "done_VERSION"}
  ]
}`, "VERSION", version)
}

// FINDINGS A-118, wf-orchestration-v1 §5.1: each instance finishes on the definition it was born
// with. A redeploy (a restart on a new definition) keeps a running instance on its own, by its hash,
// while a new instance takes the new one; the old definition goes once no running instance uses it.
func TestARedeployKeepsARunningInstanceOnItsDefinition(t *testing.T) {
	path := filepath.Join(t.TempDir(), "wf.db")
	v1, _ := newTestRuntime(t, path, deployWorkflowJSON("v1"), nil)
	old := startInstance(t, v1, from("IO.in@motherbee", msgWithName("START")))

	v2, _ := newTestRuntime(t, path, deployWorkflowJSON("v2"), &mockTimerLister{})
	if err := v2.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	if _, kept, _ := v2.Store.GetDefinition(context.Background(), v1.DefHash); !kept {
		t.Fatal("the old definition is kept while an instance runs on it")
	}
	fresh := startInstance(t, v2, from("IO.in@motherbee", msgWithName("START")))
	if row := mustGetInstance(t, v2, fresh); row.DefinitionHash != v2.DefHash {
		t.Fatalf("a new instance takes the current definition: %+v", row)
	}

	for _, instanceID := range []string{old, fresh} {
		goMsg := from("AI.host@motherbee", msgWithName("GO"))
		goMsg.Meta.ThreadID = &instanceID
		if err := Dispatch(context.Background(), goMsg, v2); err != nil {
			t.Fatal(err)
		}
	}
	if row := mustGetInstance(t, v2, old); row.CurrentState != "done_v1" || row.Status != "completed" || row.DefinitionHash != v1.DefHash {
		t.Fatalf("the old instance finishes on its own definition: %+v", row)
	}
	if row := mustGetInstance(t, v2, fresh); row.CurrentState != "done_v2" {
		t.Fatalf("the new instance runs on the new definition: %+v", row)
	}
	if _, kept, _ := v2.Store.GetDefinition(context.Background(), v1.DefHash); kept {
		t.Fatal("the old definition goes once no running instance uses it")
	}
	if _, kept, _ := v2.Store.GetDefinition(context.Background(), v2.DefHash); !kept {
		t.Fatal("the current definition stays")
	}
}

// A running instance whose definition is not kept, or no longer loads (one written before 0.1.90,
// with meta.type), cannot go on: recovery fails it (definition_unavailable), loudly, instead of an
// instance that errs on every event forever.
func TestAnInstanceWhoseDefinitionIsGoneFailsAtRecovery(t *testing.T) {
	rt, _ := newTestRuntime(t, "", deployWorkflowJSON("v1"), &mockTimerLister{})
	putRunningInstance(t, rt, "wfi:lost", "waiting")
	putRunningInstance(t, rt, "wfi:outdated", "waiting")
	outdated := strings.Replace(askingWorkflowJSON, `"meta": {"msg": "ASK"}`, `"meta": {"msg": "ASK", "type": "system"}`, 1)
	if err := rt.Store.PutDefinition(context.Background(), DefinitionRow{Hash: "outdated", WorkflowType: "deploy", DefinitionJSON: outdated}); err != nil {
		t.Fatal(err)
	}
	for id, hash := range map[string]string{"wfi:lost": "never-kept", "wfi:outdated": "outdated"} {
		if _, err := rt.Store.db.Exec(`UPDATE wf_instances SET definition_hash = ? WHERE instance_id = ?;`, hash, id); err != nil {
			t.Fatal(err)
		}
	}
	if err := rt.Recover(context.Background(), recoverOpts); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"wfi:lost", "wfi:outdated"} {
		if row := mustGetInstance(t, rt, id); row.Status != "failed" {
			t.Fatalf("%s: %+v", id, row)
		}
		if entry := lastLog(t, rt, id); !strings.HasPrefix(entry.ErrorDetail, causeDefinitionUnavailable+": ") {
			t.Fatalf("%s: its log names the cause: %+v", id, entry)
		}
	}
	if _, kept, _ := rt.Store.GetDefinition(context.Background(), "outdated"); kept {
		t.Fatal("a definition no running instance uses goes")
	}
}

// 0.1.90: {"$ref": path, "default": value} gives the literal value when path leads to no value or to
// null; without a default, a missing field still fails the action (and, by default, the instance).
func TestARefWithADefaultForwardsAMessageWithoutContext(t *testing.T) {
	const forwarding = `{
  "wf_schema_version": "1",
  "workflow_type": "forwarding",
  "input_schema": {"type": "object"},
  "initial_state": "forwarding",
  "terminal_states": ["forwarded", "failed"],
  "states": [
    {
      "name": "forwarding",
      "entry_actions": [
        {"type": "send_message", "target": "AI.host@motherbee",
         "meta": {"msg": "TURN", "context": {"$ref": "event.context", "default": {}}},
         "payload": {"text": {"$ref": "event.payload.text"}, "lang": {"$ref": "input.lang", "default": "en"}}}
      ],
      "transitions": [{"event_match": {"msg": "TURN"}, "guard": "true", "target_state": "forwarded"}]
    },
    {"name": "forwarded"},
    {"name": "failed"}
  ]
}`
	rt, disp := newTestRuntime(t, "", forwarding, nil)
	withContext := from("IO.in@motherbee", msgWithNameAndPayload("IN", map[string]any{"text": "hello"}))
	withContext.Meta.Context = json.RawMessage(`{"io": {"channel": "wapp"}}`)
	startInstance(t, rt, withContext)
	startInstance(t, rt, from("IO.in@motherbee", msgWithNameAndPayload("IN", map[string]any{"text": "hello", "lang": "fr"})))
	startInstance(t, rt, from("IO.in@motherbee", msgWithNameAndPayload("IN", map[string]any{"text": "hello", "lang": nil})))
	if len(disp.sent) != 3 {
		t.Fatalf("sent %d", len(disp.sent))
	}
	if got := string(disp.sent[0].Meta.Context); got != `{"io":{"channel":"wapp"}}` {
		t.Fatalf("a present field is taken: %s", got)
	}
	if got := string(disp.sent[1].Meta.Context); got != `{}` {
		t.Fatalf("a missing field takes its default: %s", got)
	}
	if got := string(disp.sent[0].Payload); got != `{"lang":"en","text":"hello"}` {
		t.Fatalf("payload: %s", got)
	}
	if got := string(disp.sent[1].Payload); got != `{"lang":"fr","text":"hello"}` {
		t.Fatalf("payload: %s", got)
	}
	if got := string(disp.sent[2].Payload); got != `{"lang":"en","text":"hello"}` {
		t.Fatalf("a null field takes its default too: %s", got)
	}

	// No default: a message without text fails the send, and the instance.
	if err := Dispatch(context.Background(), from("IO.in@motherbee", msgWithNameAndPayload("IN", map[string]any{})), rt); err != nil {
		t.Fatal(err)
	}
	rows, _ := rt.Store.ListInstances(context.Background(), "failed", 0, 0)
	if len(rows) != 1 || !strings.Contains(lastLog(t, rt, rows[0].InstanceID).ErrorDetail, `$ref "event.payload.text": key "text" not found`) {
		t.Fatalf("a missing field without a default fails the instance: %+v", rows)
	}
}
