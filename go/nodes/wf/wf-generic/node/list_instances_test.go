package node

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// sdkRecorder sends through the Go SDK's own sender, connected to a router of the test's, and keeps
// each message the SDK took: a reply over the router's frame is refused by the SDK, as on a hive.
type sdkRecorder struct {
	*SDKDispatcher
	sent []sdk.Message
}

func (d *sdkRecorder) SendMsg(msg sdk.Message, note sdk.TraceNote) error {
	if err := d.SDKDispatcher.SendMsg(msg, note); err != nil {
		return err
	}
	d.sent = append(d.sent, msg)
	return nil
}

// sendThroughTheSDK makes rt send through the Go SDK, as a WF node on a hive does, and returns what
// the SDK takes.
func sendThroughTheSDK(t *testing.T, rt *NodeRuntime) *sdkRecorder {
	t.Helper()
	profile, err := wfRouteProfile()
	if err != nil {
		t.Fatal(err)
	}
	recorder := &sdkRecorder{SDKDispatcher: NewSDKDispatcher(connectToTestRouter(t, profile).SenderSnapshot())}
	rt.ActCtx.Dispatcher = recorder
	return recorder
}

// insertInstances writes n instances of status into store in one transaction, as the engine writes
// an instance's row, created a millisecond apart from createdAtMS; a terminated one ended a minute
// after it started.
func insertInstances(t *testing.T, store *Store, n int, status string, createdAtMS int64, definitionHash string) {
	t.Helper()
	ctx := context.Background()
	tx, err := store.db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	insert, err := tx.PrepareContext(ctx, `INSERT INTO wf_instances (`+instanceColumns+`) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);`)
	if err != nil {
		t.Fatal(err)
	}
	defer insert.Close()
	for i := 0; i < n; i++ {
		row := WFInstanceRow{
			InstanceID:     newInstanceID(),
			ThreadID:       fmt.Sprintf("conv-%s-%04d", status, i),
			WorkflowType:   "invoice",
			DefinitionHash: definitionHash,
			Status:         status,
			CurrentState:   "collecting_data",
			InputJSON:      `{"customer_id":"c1"}`,
			StateJSON:      `{}`,
			CreatedAtMS:    createdAtMS + int64(i),
			UpdatedAtMS:    createdAtMS + int64(i),
		}
		if status != "running" {
			row.CurrentState = status
			row.TerminatedAtMS = sql.NullInt64{Int64: row.CreatedAtMS + 60_000, Valid: true}
		}
		if _, err := insert.ExecContext(ctx, instanceValues(row)...); err != nil {
			t.Fatal(err)
		}
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

// askToList sends rt WF_LIST_INSTANCES with payload as SY.wf-rules of its hive does, and returns what
// Dispatch returned.
func askToList(rt *NodeRuntime, payload json.RawMessage) error {
	request := from("SY.wf-rules@motherbee", msgWithName(MsgWFListInstances))
	request.Meta.MsgType = sdk.SYSTEMKind
	request.Payload = payload
	return Dispatch(context.Background(), request, rt)
}

// listAnswer is the payload of the WF_LIST_INSTANCES_RESPONSE in sent, the last message.
func listAnswer(t *testing.T, sent []sdk.Message) map[string]any {
	t.Helper()
	if len(sent) == 0 {
		t.Fatal("no reply sent")
	}
	reply := sent[len(sent)-1]
	if name := derefString(reply.Meta.Msg); name != MsgWFListInstancesResponse {
		t.Fatalf("reply %q, want %s", name, MsgWFListInstancesResponse)
	}
	var payload map[string]any
	if err := json.Unmarshal(reply.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	return payload
}

// keysOf is the keys of object, sorted and joined by commas.
func keysOf(object map[string]any) string {
	names := make([]string, 0, len(object))
	for name := range object {
		names = append(names, name)
	}
	sort.Strings(names)
	return strings.Join(names, ",")
}

// SY.wf-rules counts a workflow's running instances with a count-only WF_LIST_INSTANCES: a COUNT(*)
// with the list's filters, answered {ok, count} with no rows, whatever the limit. The 1000 running
// instances spec §21 allows a node are counted in one answer of a few hundred bytes, through the
// SDK's own sender; the whole list SY.wf-rules counted before passed the frame past about 350 of
// them, and the node answered INTERNAL_ERROR (FINDINGS A-284). Each count is the length of the list
// with the same filters and no limit.
func TestACountOnlyListCountsAThousandRunningInstancesInOneAnswer(t *testing.T) {
	rt, _ := newTestRuntime(t, "", validWorkflowJSON(), nil)
	const start = int64(1_776_000_000_000)
	insertInstances(t, rt.Store, 1000, "running", start, rt.DefHash)
	insertInstances(t, rt.Store, 7, "completed", start-100_000, rt.DefHash)
	insertInstances(t, rt.Store, 3, "failed", start-200_000, rt.DefHash)
	insertInstances(t, rt.Store, 2, "cancelled", start-300_000, rt.DefHash)
	sent := sendThroughTheSDK(t, rt)

	if err := askToList(rt, json.RawMessage(`{"status": "running", "limit": 0, "count_only": true}`)); err != nil {
		t.Fatalf("the count's answer did not go: %v", err)
	}
	answer := listAnswer(t, sent.sent)
	if keysOf(answer) != "count,ok" || answer["ok"] != true || answer["count"] != float64(1000) {
		t.Fatalf("WF_LIST_INSTANCES count_only = %v", answer)
	}
	frame, err := sdk.MarshalMessage(sent.sent[len(sent.sent)-1])
	if err != nil || len(frame) > 1024 {
		t.Fatalf("the count's answer is %d bytes (%v), want one small frame", len(frame), err)
	}

	for _, c := range []struct {
		name, payload string
		status        string
		after         int64
		want          int
	}{
		{"every instance", `{"count_only": true}`, "", 0, 1012},
		{"one status", `{"status": "completed", "count_only": true}`, "completed", 0, 7},
		{"running after a time, a limit counting none", `{"status": "running", "created_after_ms": 1776000000499, "limit": 10, "count_only": true}`, "running", start + 499, 500},
		{"after a time", `{"created_after_ms": 1775999850000, "count_only": true}`, "", start - 150_000, 1007},
	} {
		t.Run(c.name, func(t *testing.T) {
			if err := askToList(rt, json.RawMessage(c.payload)); err != nil {
				t.Fatal(err)
			}
			rows, err := rt.Store.ListInstances(context.Background(), c.status, 0, c.after)
			if err != nil {
				t.Fatal(err)
			}
			answer := listAnswer(t, sent.sent)
			if keysOf(answer) != "count,ok" || answer["count"] != float64(c.want) || len(rows) != c.want {
				t.Fatalf("count_only %s = %v, want %d, the list of its filters %d", c.payload, answer, c.want, len(rows))
			}
		})
	}
}

// An SY.wf-rules before 0.1.136 asks without count_only, and is answered as before, which is how a
// new engine and an old SY.wf-rules count together during a deploy: the whole list, which past the
// router's frame the SDK refuses and the node answers INTERNAL_ERROR, never a list cut to fit, whose
// count would read as fewer instances than run; a delete without force reads that refusal as
// INSTANCES_UNKNOWN (FINDINGS A-270). A page SY.admin asks for, 200 at most, is listed as before.
func TestAListWithoutCountOnlyIsAnsweredAsBefore(t *testing.T) {
	rt, _ := newTestRuntime(t, "", validWorkflowJSON(), nil)
	const start = int64(1_776_000_000_000)
	insertInstances(t, rt.Store, 1000, "running", start, rt.DefHash)
	sent := sendThroughTheSDK(t, rt)

	err := askToList(rt, json.RawMessage(`{"status": "running", "limit": 0}`))
	var tooLarge *sdk.MessageTooLargeError
	if !errors.As(err, &tooLarge) {
		t.Fatalf("the whole list of 1000 passes the frame, and its failure reaches the receive loop's log: %v", err)
	}
	answer := listAnswer(t, sent.sent)
	if detail, _ := answer["detail"].(string); answer["ok"] != false || answer["error"] != "INTERNAL_ERROR" || !strings.Contains(detail, "message too large") {
		t.Fatalf("the list over the frame = %v", answer)
	}

	if err := askToList(rt, json.RawMessage(`{"status": "running", "limit": 200}`)); err != nil {
		t.Fatal(err)
	}
	answer = listAnswer(t, sent.sent)
	instances, _ := answer["instances"].([]any)
	if keysOf(answer) != "count,instances,ok" || answer["count"] != float64(200) || len(instances) != 200 {
		t.Fatalf("a page of 200 = count %v, %d instances", answer["count"], len(instances))
	}
	if newest, _ := instances[0].(map[string]any); newest["created_at_ms"] != float64(start+999) {
		t.Fatalf("the page starts with the newest instance: %v", newest)
	}
}

// A count-only request the node cannot read, or that names a status no instance has, is refused,
// INVALID_REQUEST, and a count the store fails is STORE_ERROR: never a count, since a count of a
// misspelled status, or of the filters the node could read, would be 0, which a delete without force
// reads as a node with none and purges its instances. Before, a payload that did not decode was
// answered on what it could read, and a status no instance has with none. A request with no payload
// still lists every instance.
func TestACountTheNodeCannotTakeIsRefusedNeverAnswered(t *testing.T) {
	rt, disp := newTestRuntime(t, "", validWorkflowJSON(), nil)
	insertInstances(t, rt.Store, 3, "running", 1_776_000_000_000, rt.DefHash)

	for _, c := range []struct{ name, payload, says string }{
		{"a status no instance has", `{"status": "Running", "count_only": true}`, `status must be one of running, completed, cancelled, failed, not "Running"`},
		{"count_only not a boolean", `{"status": "running", "count_only": "true"}`, "invalid WF_LIST_INSTANCES payload"},
		{"a status that is no string", `{"status": 1, "count_only": true}`, "invalid WF_LIST_INSTANCES payload"},
		{"no object", `["running"]`, "invalid WF_LIST_INSTANCES payload"},
	} {
		t.Run(c.name, func(t *testing.T) {
			if err := askToList(rt, json.RawMessage(c.payload)); err != nil {
				t.Fatal(err)
			}
			answer := listAnswer(t, disp.sent)
			if detail, _ := answer["detail"].(string); answer["ok"] != false || answer["error"] != "INVALID_REQUEST" || !strings.Contains(detail, c.says) || answer["count"] != nil {
				t.Fatalf("%s = %v, want INVALID_REQUEST saying %q", c.payload, answer, c.says)
			}
		})
	}

	if err := askToList(rt, nil); err != nil {
		t.Fatal(err)
	}
	if answer := listAnswer(t, disp.sent); answer["ok"] != true || answer["count"] != float64(3) {
		t.Fatalf("a request with no payload = %v, want every instance", answer)
	}

	if err := rt.Store.Close(); err != nil {
		t.Fatal(err)
	}
	if err := askToList(rt, json.RawMessage(`{"status": "running", "limit": 0, "count_only": true}`)); err != nil {
		t.Fatal(err)
	}
	if answer := listAnswer(t, disp.sent); answer["ok"] != false || answer["error"] != errCodeStoreError || answer["count"] != nil {
		t.Fatalf("a count the store fails = %v, want %s", answer, errCodeStoreError)
	}
}
