package node

import (
	"context"
	"encoding/json"
	"path/filepath"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

// newTestRuntime is a WF node's runtime as startEngine builds it: the store at path (a new file
// when path is ""), the definition kept by its hash and current, an empty registry. Recovery is the
// test's to run.
func newTestRuntime(t *testing.T, path, definition string, timer TimerSender) (*NodeRuntime, *mockDispatcher) {
	t.Helper()
	if path == "" {
		path = filepath.Join(t.TempDir(), "wf.db")
	}
	def, err := LoadDefinitionBytes([]byte(definition), "", fixedClock)
	if err != nil {
		t.Fatalf("load definition: %v", err)
	}
	store, err := OpenStore(path)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })
	hash := definitionHash([]byte(definition))
	if err := store.PutDefinition(context.Background(), DefinitionRow{
		Hash:           hash,
		WorkflowType:   def.WorkflowType,
		DefinitionJSON: definition,
		LoadedAtMS:     nowMS(fixedClock),
	}); err != nil {
		t.Fatalf("PutDefinition: %v", err)
	}
	if timer == nil {
		timer = &mockTimerSender{}
	}
	disp := &mockDispatcher{l2name: testNodeName, uuid: "wf-uuid"}
	rt := &NodeRuntime{
		Def:      def,
		DefJSON:  definition,
		DefHash:  hash,
		Registry: NewInstanceRegistry(),
		Store:    store,
		ActCtx:   ActionContext{Store: store, Dispatcher: disp, Timer: timer, Clock: fixedClock},
		NodeUUID: "wf-uuid",
		NodeName: testNodeName,
	}
	return rt, disp
}

// from is msg as the router delivers it from sender: routing.src_l2_name stamped.
func from(sender string, msg sdk.Message) sdk.Message {
	msg.Routing.SrcL2Name = &sender
	// The sender's entry in the message's trace, as its SDK writes it (docs/message-trace-v1.md).
	sdk.WriteTraceEntry(&msg, sender, sdk.TraceNote{})
	return msg
}

// onThread is msg on thread, a conversation's: a message without one starts no instance (FINDINGS
// A-135).
func onThread(thread string, msg sdk.Message) sdk.Message {
	msg.Meta.ThreadID = &thread
	return msg
}

// threadOf is the thread of the instance instanceID, the conversation's: a message on it reaches the
// instance.
func threadOf(t *testing.T, rt *NodeRuntime, instanceID string) *string {
	t.Helper()
	thread := mustGetInstance(t, rt, instanceID).ThreadID
	return &thread
}

// notTakenReason is the reason of msg when it is the engine's NOT_TAKEN: a system message whose
// payload is {ok: false, error: not_taken, reason, detail}. "" for any other message.
func notTakenReason(t *testing.T, msg sdk.Message) string {
	t.Helper()
	if derefString(msg.Meta.Msg) != msgNotTaken {
		return ""
	}
	var payload struct {
		OK     *bool  `json:"ok"`
		Error  string `json:"error"`
		Reason string `json:"reason"`
		Detail string `json:"detail"`
	}
	if err := json.Unmarshal(msg.Payload, &payload); err != nil || msg.Meta.MsgType != sdk.SYSTEMKind || payload.OK == nil || *payload.OK ||
		payload.Error != "not_taken" || payload.Reason == "" || payload.Detail == "" {
		t.Fatalf("a NOT_TAKEN is a system message with {ok: false, error: not_taken, reason, detail}: %+v %s (%v)", msg.Meta, msg.Payload, err)
	}
	return payload.Reason
}

// putRunningInstance stores a running instance of rt's current definition in state, as a previous run
// of the node left it; its thread is its id, so a test reaches it by its id.
func putRunningInstance(t *testing.T, rt *NodeRuntime, instanceID, state string) {
	t.Helper()
	if err := rt.Store.CreateInstance(context.Background(), WFInstanceRow{
		InstanceID:     instanceID,
		ThreadID:       instanceID,
		WorkflowType:   rt.Def.WorkflowType,
		DefinitionHash: rt.DefHash,
		Status:         "running",
		CurrentState:   state,
		InputJSON:      `{"customer_id":"c1"}`,
		StateJSON:      `{}`,
		CreatedAtMS:    1000,
		UpdatedAtMS:    1000,
	}); err != nil {
		t.Fatalf("CreateInstance: %v", err)
	}
}

// startInstance dispatches trigger and returns the id of the instance it starts. A trigger without a
// thread gets one of its own, a new conversation's, which threadOf gives back: a message without one
// starts no instance (FINDINGS A-135).
func startInstance(t *testing.T, rt *NodeRuntime, trigger sdk.Message) string {
	t.Helper()
	if strings.TrimSpace(derefString(trigger.Meta.ThreadID)) == "" {
		trigger = onThread("conv-"+uuid.NewString(), trigger)
	}
	before := map[string]bool{}
	for _, inst := range rt.Registry.All() {
		before[inst.InstanceID] = true
	}
	if err := Dispatch(context.Background(), trigger, rt); err != nil {
		t.Fatalf("Dispatch %s: %v", rawMsgName(trigger), err)
	}
	for _, inst := range rt.Registry.All() {
		if !before[inst.InstanceID] {
			return inst.InstanceID
		}
	}
	t.Fatalf("%s started no running instance", rawMsgName(trigger))
	return ""
}

func mustGetInstance(t *testing.T, rt *NodeRuntime, instanceID string) WFInstanceRow {
	t.Helper()
	row, err := rt.Store.GetInstance(context.Background(), instanceID)
	if err != nil {
		t.Fatalf("GetInstance %s: %v", instanceID, err)
	}
	return row
}
