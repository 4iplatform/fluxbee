package node

import (
	"context"
	"path/filepath"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
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
	return msg
}

// putRunningInstance stores a running instance of rt's current definition in state, on its own id
// as its thread, as a previous run of the node left it.
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

// startInstance dispatches trigger and returns the id of the instance it starts.
func startInstance(t *testing.T, rt *NodeRuntime, trigger sdk.Message) string {
	t.Helper()
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
