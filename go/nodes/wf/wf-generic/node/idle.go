package node

import (
	"context"
	"encoding/json"
	"fmt"
	"log"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

// closeQuietInstances closes every running instance that went quiet (FINDINGS A-154): it took no
// event for its definition's idle_timeout, and holds no timer. A conversation that waits for its own
// timer is not abandoned, however long the wait. The receive loop calls it once a GC interval, between
// two messages, so a close never meets a transition.
func (rt *NodeRuntime) closeQuietInstances(ctx context.Context) {
	if rt.Registry == nil || rt.Store == nil {
		return
	}
	for _, inst := range rt.Registry.All() {
		if err := rt.closeIfQuiet(ctx, inst); err != nil {
			log.Printf("instance %s: closing it after its idle_timeout: %v", inst.InstanceID, err)
		}
	}
}

// closeIfQuiet closes inst as a cancel does (spec §12): the current state's exit actions, the
// cancelled state's entry actions, and the instance's log tells why. The event those actions see is
// the WF_CANCEL_INSTANCE SY.admin would send, with no sender and a trace_id of its own.
func (rt *NodeRuntime) closeIfQuiet(ctx context.Context, inst *WFInstance) error {
	inst.Lock()
	defer inst.Unlock()
	if inst.Status != "running" {
		return nil
	}
	after, words := inst.def.IdleAfter()
	if nowMS(rt.ActCtx.Clock)-inst.UpdatedAtMS < after.Milliseconds() {
		return nil
	}
	timers, err := rt.Store.ListTimersForInstance(ctx, inst.InstanceID)
	if err != nil {
		return fmt.Errorf("list its timers: %w", err)
	}
	if len(timers) > 0 {
		return nil
	}
	reason := fmt.Sprintf("quiet for %s with no timer pending (idle_timeout)", words)
	payload, err := json.Marshal(map[string]string{"instance_id": inst.InstanceID, "reason": reason})
	if err != nil {
		return err
	}
	msgName := MsgWFCancelInstance
	event := sdk.Message{
		Routing: sdk.Routing{TraceID: uuid.NewString()},
		Meta:    sdk.Meta{MsgType: "system", Msg: &msgName},
		Payload: payload,
	}
	_ = rt.Store.AppendLog(ctx, WFLogEntry{
		InstanceID: inst.InstanceID,
		LoggedAtMS: nowMS(rt.ActCtx.Clock),
		ActionType: "idle_close",
		Summary:    "closed: " + reason,
		OK:         true,
	})
	log.Printf("instance %s: closed: %s", inst.InstanceID, reason)
	err = rt.protect(ctx, inst, event, takenEvent{}, func() error {
		return inst.runCancelTransition(ctx, event, rt.ActCtx, takenEvent{})
	})
	rt.settle(ctx, inst)
	return err
}
