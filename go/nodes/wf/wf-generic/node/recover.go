package node

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// RecoverOptions controls boot-time reconciliation behaviour.
type RecoverOptions struct {
	// OwnerL2Name is this WF node's own L2 name, used to filter SY.timer results.
	OwnerL2Name string
	// MaxRetries is how many times to retry TIMER_LIST on failure (default 3).
	MaxRetries int
}

// Recover loads all running instances from the store into the registry and
// reconciles the local timer index against SY.timer.
//
// Must be called before starting the receive loop.
func Recover(ctx context.Context, def *WorkflowDefinition, store *Store, reg *InstanceRegistry, actx ActionContext, opts RecoverOptions) error {
	if opts.MaxRetries <= 0 {
		opts.MaxRetries = 3
	}

	// 1. Load all running/cancelling instances into the registry.
	rows, err := store.LoadRunningInstances(ctx)
	if err != nil {
		return fmt.Errorf("recover: load running instances: %w", err)
	}
	for _, row := range rows {
		inst, err := FromRow(row, def)
		if err != nil {
			log.Printf("recover: skip instance %s: %v", row.InstanceID, err)
			continue
		}
		reg.Add(inst)
	}
	log.Printf("recover: loaded %d running instances", len(rows))

	// 2. Load local timer expectations.
	expectedTimers, err := store.ListAllExpectedTimers(ctx)
	if err != nil {
		return fmt.Errorf("recover: list expected timers: %w", err)
	}

	// 3. Query SY.timer for all timers owned by this node.
	if actx.Timer == nil {
		log.Printf("recover: no timer sender configured, skipping timer reconciliation")
		return nil
	}

	var syTimers []sdk.TimerInfo
	var listErr error
	retryDelays := []time.Duration{500 * time.Millisecond, 2 * time.Second, 5 * time.Second}
	for attempt := 0; attempt < opts.MaxRetries; attempt++ {
		syTimers, listErr = actx.Timer.List(ctx, sdk.ListFilter{
			OwnerL2Name:  opts.OwnerL2Name,
			StatusFilter: "pending",
			Limit:        sdk.MaxTimerListLimit,
		})
		if listErr == nil {
			break
		}
		log.Printf("recover: TIMER_LIST attempt %d/%d failed: %v", attempt+1, opts.MaxRetries, listErr)
		if attempt < opts.MaxRetries-1 {
			select {
			case <-ctx.Done():
				return ctx.Err()
			case <-time.After(retryDelays[attempt]):
			}
		}
	}
	if listErr != nil {
		return fmt.Errorf("recover: TIMER_LIST failed after %d attempts: %w (hard dependency)", opts.MaxRetries, listErr)
	}
	if len(syTimers) == sdk.MaxTimerListLimit {
		log.Printf("recover: TIMER_LIST reached max limit=%d for owner=%s; reconciliation may be truncated", sdk.MaxTimerListLimit, opts.OwnerL2Name)
	}

	// Build a clientRef → TimerInfo index from SY.timer's view.
	syView := make(map[string]sdk.TimerInfo, len(syTimers))
	for _, t := range syTimers {
		if t.ClientRef != nil && *t.ClientRef != "" {
			syView[*t.ClientRef] = t
		}
	}

	// 4. Reconcile: iterate WF's expected timers.
	now := actx.Clock().UnixMilli()
	for _, row := range expectedTimers {
		ref := timerClientRef(row.InstanceID, row.TimerKey)

		// Skip timers for cancelling instances — leave them alone,
		// but remove from syView so they don't appear as orphans.
		if inst, ok := reg.Get(row.InstanceID); ok && inst.Status == "cancelling" {
			delete(syView, ref)
			continue
		}
		syTimer, existsInSY := syView[ref]

		switch { //nolint:gocritic
		case existsInSY && syTimer.FireAtUTCMS > now:
			// Case A: timer exists in SY.timer, fire_at in future — nothing to do.

		case existsInSY && syTimer.FireAtUTCMS <= now:
			// Case B: timer exists in SY.timer but already past its fire_at.
			// Inject a synthetic TIMER_FIRED event, then cancel in SY.timer.
			log.Printf("recover: timer %s/%s past fire_at, injecting synthetic TIMER_FIRED", row.InstanceID, row.TimerKey)
			inst, ok := reg.Get(row.InstanceID)
			if !ok {
				log.Printf("recover: instance %s not in registry, skipping synthetic fire", row.InstanceID)
			} else {
				synthMsg := buildSyntheticTimerFired(row.InstanceID, row.TimerKey, ref, syTimer, actx.Clock)
				inst.Lock()
				if err := inst.RunTransition(ctx, synthMsg, actx); err != nil {
					log.Printf("recover: synthetic TIMER_FIRED for %s/%s: %v", row.InstanceID, row.TimerKey, err)
				}
				if inst.Status != "running" && inst.Status != "cancelling" {
					reg.Remove(inst.InstanceID)
				}
				inst.Unlock()
			}
			currentRow, existsAfterTransition, err := store.GetTimerForInstance(ctx, row.InstanceID, row.TimerKey)
			if err != nil {
				log.Printf("recover: reload timer row %s/%s: %v", row.InstanceID, row.TimerKey, err)
			}
			if existsAfterTransition && (currentRow.FireAtMS != row.FireAtMS || currentRow.ScheduledAtMS != row.ScheduledAtMS) {
				log.Printf("recover: timer %s/%s was re-scheduled during synthetic fire, keeping new timer", row.InstanceID, row.TimerKey)
				delete(syView, ref)
				continue
			}
			if !existsAfterTransition {
				delete(syView, ref)
				continue
			}
			// Cancel in SY.timer to prevent double-firing of the old pending timer.
			if err := actx.Timer.CancelByClientRef(ctx, ref); err != nil {
				log.Printf("recover: cancel past timer %s: %v", ref, err)
			}
			_ = store.DeleteTimer(ctx, row.InstanceID, row.TimerKey)

		case !existsInSY:
			// Case C: WF expects timer but SY.timer doesn't know about it.
			log.Printf("recover: expected timer %s/%s missing from SY.timer, cleaning up", row.InstanceID, row.TimerKey)
			_ = store.DeleteTimer(ctx, row.InstanceID, row.TimerKey)
		}

		// Remove from syView so we can detect orphans below.
		delete(syView, ref)
	}

	// 5. Timers in SY.timer but NOT expected by WF — orphans, cancel them.
	for ref, t := range syView {
		instanceID, _, err := parseTimerClientRef(ref)
		if err != nil {
			// client_ref doesn't belong to this WF format, skip.
			continue
		}
		log.Printf("recover: orphaned timer %s (instance %s), cancelling in SY.timer", ref, instanceID)
		if err := actx.Timer.CancelByClientRef(ctx, ref); err != nil {
			log.Printf("recover: cancel orphan %s: %v", ref, err)
		}
		_ = t.UUID // suppress unused warning
	}

	log.Printf("recover: reconciliation complete")
	return nil
}

// buildSyntheticTimerFired constructs a fake TIMER_FIRED message for use during
// boot recovery when a timer should have fired while the WF was down.
func buildSyntheticTimerFired(instanceID, timerKey, clientRef string, info sdk.TimerInfo, clock ClockFunc) sdk.Message {
	userPayload := map[string]any{
		"instance_id": instanceID,
		"timer_key":   timerKey,
	}
	clientRefCopy := clientRef
	fired := sdk.FiredEvent{
		TimerUUID:            info.UUID,
		OwnerL2Name:          info.OwnerL2Name,
		ClientRef:            &clientRefCopy,
		Kind:                 "oneshot",
		ScheduledFireAtUTCMS: info.FireAtUTCMS,
		ActualFireAtUTCMS:    clock().UnixMilli(),
		FireCount:            1,
		IsLastFire:           true,
	}
	raw, _ := json.Marshal(userPayload)
	fired.UserPayload = raw

	firedJSON, _ := json.Marshal(fired)
	msgName := sdk.MsgTimerFired
	return sdk.Message{
		Routing: sdk.Routing{
			Src:     info.OwnerL2Name,
			Dst:     sdk.UnicastDestination(info.OwnerL2Name),
			TTL:     1,
			TraceID: fmt.Sprintf("recover-%s-%s", instanceID, timerKey),
		},
		Meta: sdk.Meta{
			MsgType: "system",
			Msg:     &msgName,
		},
		Payload: firedJSON,
	}
}
