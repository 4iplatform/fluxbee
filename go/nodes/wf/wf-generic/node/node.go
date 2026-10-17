package node

import (
	"context"
	"crypto/sha256"
	"fmt"
	"log"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// RunOptions is the full set of options for node.Run().
type RunOptions struct {
	// ConfigPath is the path to config.json (used by the real entrypoint).
	ConfigPath string
	// NodeName is the managed node name (FLUXBEE_NODE_NAME); "" in local dev.
	NodeName string
	// Plane is the node's control plane when the caller already bootstrapped it; otherwise Run
	// bootstraps it from ConfigPath.
	Plane *ControlPlane
	// Clock overrides time.Now for deterministic testing.
	Clock ClockFunc
	// SDKConfig is the SDK connection config. If nil, Run only checks the config and returns.
	SDKConfig *sdk.NodeConfig
}

// Run is the main lifecycle function. It:
//  1. Bootstraps the control plane (config + workflow definition)
//  2. Connects to the router, whatever the config state
//  3. With a config in use: opens SQLite, recovers running instances, reconciles timers and starts
//     the GC goroutine
//  4. Enters the receive loop; without a config in use it answers control messages only, saying
//     why, and a restart applies a corrected config
func Run(ctx context.Context, opts RunOptions) error {
	clock := opts.Clock
	if clock == nil {
		clock = defaultClock
	}
	plane := opts.Plane
	if plane == nil {
		if opts.ConfigPath == "" {
			return fmt.Errorf("either ConfigPath or Plane is required")
		}
		var err error
		plane, err = BootstrapControlPlane(opts.ConfigPath, opts.NodeName, clock)
		if err != nil {
			return err
		}
	}
	materialized := plane.Configured()
	if materialized != nil {
		log.Printf("wf: loaded definition workflow_type=%s states=%d", materialized.Definition.WorkflowType, len(materialized.Definition.States))
	} else {
		log.Printf("wf: config state %s (%s): answering control messages only; a restart applies a corrected config", plane.State.CurrentState, plane.notConfiguredReason())
	}

	// No SDK config: only the config check (tests, dry run).
	if opts.SDKConfig == nil {
		if materialized == nil {
			return fmt.Errorf("config not in use: %s", plane.notConfiguredReason())
		}
		log.Printf("wf: no SDK config, exiting after the config check")
		return nil
	}

	profile, err := sdk.NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(
			sdk.RouteAny{},
			sdk.RouteCommand{Channel: "incoming"},
		).
		Build()
	if err != nil {
		return fmt.Errorf("wf rpc profile invalid: %w", err)
	}
	routerDispatcher, err := sdk.ConnectWithRetry(*opts.SDKConfig, time.Second, profile)
	if err != nil {
		return fmt.Errorf("sdk connect: %w", err)
	}
	defer func() { _ = routerDispatcher.Close() }()
	sender := routerDispatcher.SenderSnapshot()
	incoming, err := routerDispatcher.TakeCommandReceiver("incoming")
	if err != nil {
		return fmt.Errorf("take incoming receiver: %w", err)
	}

	rt := &NodeRuntime{
		ActCtx:     ActionContext{Dispatcher: NewSDKDispatcher(sender), Clock: clock},
		NodeUUID:   sender.UUID(),
		NodeName:   sender.FullName(),
		Plane:      plane,
		VersionStr: "1.0",
	}
	defer func() {
		if rt.Store != nil {
			rt.Store.Close()
		}
	}()
	if materialized != nil {
		if err := startEngine(ctx, rt, materialized, routerDispatcher, clock); err != nil {
			return err
		}
	}

	log.Printf("wf: entering receive loop for %s", sender.FullName())
	for {
		select {
		case <-ctx.Done():
			return nil
		case msg, ok := <-incoming:
			if !ok {
				log.Printf("wf: incoming channel closed; exiting")
				return nil
			}
			if err := Dispatch(ctx, msg, rt); err != nil {
				log.Printf("wf: dispatch error: %v", err)
			}
		}
	}
}

// startEngine opens the instance store, recovers the running instances and starts the GC: what a
// node with a config in use runs on top of the control plane. The caller closes rt.Store.
func startEngine(ctx context.Context, rt *NodeRuntime, materialized *Materialized, routerDispatcher *sdk.RouterDispatcher, clock ClockFunc) error {
	cfg := materialized.Config
	store, err := OpenStore(cfg.DBPath)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}
	rt.Store = store
	def := materialized.Definition
	defHash := fmt.Sprintf("%x", sha256.Sum256(materialized.DefinitionBytes))
	if err := store.UpsertDefinition(ctx, def.WorkflowType, string(materialized.DefinitionBytes), defHash, clock); err != nil {
		return fmt.Errorf("upsert definition: %w", err)
	}
	timerSender, err := NewSDKTimerSender(routerDispatcher, cfg.SYTimerL2Name)
	if err != nil {
		return fmt.Errorf("create timer sender: %w", err)
	}

	reg := NewInstanceRegistry()
	rt.ActCtx.Store = store
	rt.ActCtx.Timer = timerSender
	if err := Recover(ctx, def, store, reg, rt.ActCtx, RecoverOptions{
		OwnerL2Name: rt.NodeName,
	}); err != nil {
		return fmt.Errorf("recovery failed: %w", err)
	}

	StartGC(ctx, store, cfg.GCRetentionDays, time.Duration(cfg.GCIntervalSeconds)*time.Second, clock)

	rt.Def = def
	rt.DefJSON = string(materialized.DefinitionBytes)
	rt.Registry = reg
	return nil
}
