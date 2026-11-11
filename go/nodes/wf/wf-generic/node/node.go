package node

import (
	"context"
	"crypto/sha256"
	"fmt"
	"log"
	"runtime/debug"
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
//     the GC goroutine (startEngine). A store it cannot use leaves the node up, waiting for it
//  4. Enters the receive loop (serve); without a config in use it answers control messages only,
//     saying why, and a restart applies a corrected config. A node that waits for its store opens it
//     again from the loop every storeRetryInterval, until the engine runs. A node whose timers are
//     not reconciled (SY.timer did not answer, or the index could not be read) tries again from the
//     loop, until they are. A timer request SY.timer did not take marks its timer, which the loop
//     sends again until SY.timer takes it (FINDINGS A-254). Once a GC interval the loop closes the
//     conversations that went quiet (closeQuietInstances)
//
// It returns an error only for what the node cannot run without at all: a config file it can read,
// a router connection, its receive channels, a timer client (and, in a dry run, a config in use). A
// config or a store it cannot use is a state it reports, never an exit (FINDINGS A-217).
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

	profile, err := wfRouteProfile()
	if err != nil {
		return fmt.Errorf("wf rpc profile invalid: %w", err)
	}
	routerDispatcher, err := sdk.ConnectWithRetry(*opts.SDKConfig, time.Second, profile)
	if err != nil {
		return fmt.Errorf("sdk connect: %w", err)
	}
	defer func() { _ = routerDispatcher.Close() }()
	sender := routerDispatcher.SenderSnapshot()
	incoming, err := routerDispatcher.TakeCommandReceiver(incomingChannel)
	if err != nil {
		return fmt.Errorf("take incoming receiver: %w", err)
	}
	timerAnswers, err := routerDispatcher.Subscribe(timerAnswersChannel)
	if err != nil {
		return fmt.Errorf("take the timer answers' receiver: %w", err)
	}

	rt := &NodeRuntime{
		ActCtx: ActionContext{Dispatcher: NewSDKDispatcher(sender), Clock: clock, SelfILK: sdk.ReadSelfIlkFromEnv(),
			Requests: newTimerRequests()},
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
		timerSender, err := NewSDKTimerSender(routerDispatcher, materialized.Config.SYTimerL2Name)
		if err != nil {
			return fmt.Errorf("create timer sender: %w", err)
		}
		rt.ActCtx.Timer = timerSender
	}
	rt.serve(ctx, incoming, timerAnswers, materialized, loopIntervals{
		storeRetry: storeRetryInterval, timerRetry: timerReconcileRetryInterval, timerRequests: timerRequestsInterval,
	})
	return nil
}

// The SDK's channels the node takes its messages from: incomingChannel, the receive loop's, and
// timerAnswersChannel, SY.timer's answers to the timer requests the node sends without waiting.
const (
	incomingChannel     = "incoming"
	timerAnswersChannel = "timer_answers"
)

// wfRouteProfile is how the SDK routes what reaches the node, beyond the replies its own calls wait
// for (TimerClient's TIMER_LIST, a confirmed cancel): a TIMER_RESPONSE to timerAnswersChannel, and
// everything else to incomingChannel. SY.timer's TIMER_RESPONSE goes to a broadcast channel because
// a broadcast route keeps the SDK from taking it as a reply only a call waits for: once the start's
// TIMER_LIST had waited for one, the SDK dropped every later TIMER_RESPONSE as an orphan, and
// SY.timer's refusals of the node's schedules went unseen (FINDINGS A-254). A late answer to a call
// that timed out is still dropped by the SDK.
func wfRouteProfile() (sdk.OperationalRouteProfile, error) {
	return sdk.NewOperationalRouteProfile().
		CommandChannel(incomingChannel).
		BroadcastChannel(timerAnswersChannel).
		PostPendingRule(sdk.RouteExact{MsgType: sdk.SYSTEMKind, Msg: sdk.MsgTimerResponse}, sdk.RouteBroadcast{Channel: timerAnswersChannel}).
		PostPendingRule(sdk.RouteAny{}, sdk.RouteCommand{Channel: incomingChannel}).
		Build()
}

// loopIntervals are how often the receive loop opens the store again while the engine waits for it,
// asks SY.timer again while the timers wait for their reconcile, and looks at its timer requests
// while it waits for SY.timer to answer one or a refused timer waits to go again.
type loopIntervals struct {
	storeRetry, timerRetry, timerRequests time.Duration
}

// serve is Run once the router is connected: it starts the engine, with a config in use, and runs
// the receive loop on incoming, and on timerAnswers, SY.timer's answers to the node's timer
// requests, until ctx is done or incoming closes. The caller sets the timer sender before, and
// closes rt.Store after.
func (rt *NodeRuntime) serve(ctx context.Context, incoming, timerAnswers <-chan sdk.Message, materialized *Materialized, every loopIntervals) {
	// While the engine waits for its store, the store is opened again every every.storeRetry, on the
	// loop: recovery runs between two messages, as at the start. While the timers wait for their
	// reconcile, a TIMER_LIST goes out every every.timerRetry, asked off the loop (the loop keeps
	// serving) and taken on it. While a timer request waits for SY.timer's answer, or a refused timer
	// to go again, the loop looks at them every every.timerRequests (FINDINGS A-254).
	var storeRetry, reconcileRetry, requestsTick loopTicker
	defer storeRetry.stop()
	defer reconcileRetry.stop()
	defer requestsTick.stop()
	// watchTimerRequests ticks while the node waits for anything from SY.timer, and stops once it
	// does not.
	watchTimerRequests := func() {
		if rt.Store != nil && rt.ActCtx.Requests.waiting() {
			requestsTick.start(every.timerRequests)
		} else {
			requestsTick.stop()
		}
	}
	// engineUp is what the loop does once the engine runs: no more opens of the store, and SY.timer
	// asked again while the timers wait for their reconcile.
	engineUp := func() {
		storeRetry.stop()
		if rt.timersPending != nil {
			reconcileRetry.start(every.timerRetry)
		}
		watchTimerRequests()
	}
	if materialized != nil {
		if rt.startEngine(ctx, materialized) {
			engineUp()
		} else {
			storeRetry.start(every.storeRetry)
		}
	}
	timerLists := make(chan timerListResult, 1)
	listing := false
	recoverOpts := RecoverOptions{OwnerL2Name: rt.NodeName}

	// The quiet conversations close once a GC interval (FINDINGS A-154), on the loop: between two
	// messages, as a cancel SY.admin sends.
	var idleTick <-chan time.Time
	if materialized != nil {
		ticker := time.NewTicker(time.Duration(materialized.Config.GCIntervalSeconds) * time.Second)
		defer ticker.Stop()
		idleTick = ticker.C
	}

	log.Printf("wf: entering receive loop for %s", rt.NodeName)
	for {
		select {
		case <-ctx.Done():
			return
		case msg, ok := <-incoming:
			if !ok {
				log.Printf("wf: incoming channel closed; exiting")
				return
			}
			if err := dispatchGuarded(ctx, msg, rt); err != nil {
				log.Printf("wf: dispatch error: %v", err)
			}
		case msg, ok := <-timerAnswers:
			if !ok {
				timerAnswers = nil
				continue
			}
			if err := dispatchGuarded(ctx, msg, rt); err != nil {
				log.Printf("wf: dispatch error: %v", err)
			}
		case <-storeRetry.C:
			if rt.startEngine(ctx, materialized) {
				engineUp()
			}
		case <-reconcileRetry.C:
			if listing || rt.timersPending == nil {
				continue
			}
			listing = true
			askedAtMS := nowMS(rt.ActCtx.Clock)
			go func() {
				timers, err := rt.listTimers(ctx, recoverOpts)
				timerLists <- timerListResult{timers: timers, err: err, askedAtMS: askedAtMS}
			}()
		case result := <-timerLists:
			listing = false
			rt.takeTimerList(ctx, result, recoverOpts)
			if rt.timersPending == nil {
				reconcileRetry.stop()
			}
		case <-requestsTick.C:
			rt.tickTimerRequests(ctx)
		case <-idleTick:
			rt.closeQuietInstances(ctx)
		}
		watchTimerRequests()
	}
}

// dispatchGuarded is Dispatch for the receive loop: a panic outside an instance's transition (which
// fails only that instance, NodeRuntime.protect) is logged and the loop goes on, never the node.
func dispatchGuarded(ctx context.Context, msg sdk.Message, rt *NodeRuntime) (err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("panic dispatching msg=%q trace_id=%s: %v\n%s", derefStringPtr(msg.Meta.Msg), msg.Routing.TraceID, recovered, debug.Stack())
		}
	}()
	return Dispatch(ctx, msg, rt)
}

// storeRetryInterval is how often a node whose engine waits for its store opens it again (FINDINGS
// A-217).
const storeRetryInterval = 30 * time.Second

// startEngine opens the instance store, keeps the definition by its hash, recovers the running
// instances and starts the GC: what a node with a config in use runs on top of the control plane.
// It reports whether the engine runs. A store it cannot use, one that does not open or that fails
// during recovery, stops the engine only (FINDINGS A-217): the store is closed again, and rt.storeDown
// says why (storeFailure). The node stays up, DEGRADED, or FAILED_CONFIG when the cause is the
// db_path itself; it refuses every event, and the receive loop calls startEngine again every
// storeRetryInterval. A start after a failed one is a restart's: recovery runs again from the store,
// and counts each event it runs again (Recover). The timer sender is the caller's to set, before;
// the caller closes rt.Store.
//
// The store's path is read (dbPathProblem) before SQLite opens it, with no store open, and again
// once a failed store is closed: a journal file the node cannot write makes SQLite open the store
// read-only, and SQLite may have replaced that file by the time it closes. What the first read found
// comes first.
func (rt *NodeRuntime) startEngine(ctx context.Context, materialized *Materialized) bool {
	dbPath := materialized.Config.DBPath
	probed := dbPathProblem(dbPath)
	if err := rt.openEngine(ctx, materialized); err != nil {
		rt.stopEngine() // first: dbPathProblem opens and closes the store's files
		if probed == nil {
			probed = dbPathProblem(dbPath)
		}
		cause, onPath := storeFailure(dbPath, err, probed)
		rt.storeDown = &storeOutage{cause: cause, def: materialized.Definition}
		if onPath {
			rt.Plane.failOnDBPath(cause)
		} else {
			rt.Plane.inUse(materialized)
		}
		log.Printf("wf: the engine waits for its store and takes no event: %s; the store is opened again every %s", cause.Error(), storeRetryInterval)
		return false
	}
	if rt.storeDown != nil {
		log.Printf("wf: the instance store opened at %s: the engine runs", dbPath)
	}
	rt.storeDown = nil
	// The timers a reconcile that could not run left refused go again from the loop (FINDINGS A-254).
	rt.countRefusedTimers(ctx)
	rt.Plane.inUse(materialized)
	cfg := materialized.Config
	StartGC(ctx, rt.Store, cfg.GCRetentionDays, time.Duration(cfg.GCIntervalSeconds)*time.Second, rt.ActCtx.Clock)
	return true
}

// openEngine is startEngine's work. On an error, what it set is stopEngine's to undo.
func (rt *NodeRuntime) openEngine(ctx context.Context, materialized *Materialized) error {
	store, err := OpenStore(materialized.Config.DBPath)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}
	rt.Store = store
	def := materialized.Definition
	defHash := definitionHash(materialized.DefinitionBytes)
	if err := store.PutDefinition(ctx, DefinitionRow{
		Hash:           defHash,
		WorkflowType:   def.WorkflowType,
		DefinitionJSON: string(materialized.DefinitionBytes),
		LoadedAtMS:     nowMS(rt.ActCtx.Clock),
	}); err != nil {
		return fmt.Errorf("keep definition: %w", err)
	}

	rt.ActCtx.Store = store
	rt.Def = def
	rt.DefJSON = string(materialized.DefinitionBytes)
	rt.DefHash = defHash
	rt.Registry = NewInstanceRegistry()
	if err := rt.Recover(ctx, RecoverOptions{OwnerL2Name: rt.NodeName}); err != nil {
		return fmt.Errorf("recovery failed: %w", err)
	}
	return nil
}

// stopEngine leaves the engine stopped, its store closed: what a failed openEngine set goes, the
// instances it recovered with it and the timer requests it sent (the next start recovers them again
// from the store, and reconciles the timers).
func (rt *NodeRuntime) stopEngine() {
	if rt.Store != nil {
		_ = rt.Store.Close()
	}
	rt.Store, rt.ActCtx.Store = nil, nil
	rt.Def, rt.DefJSON, rt.DefHash = nil, "", ""
	rt.Registry = nil
	rt.timersPending = nil
	rt.ActCtx.Requests.clear()
}

// storeFailure is how the node reports err, the failure of its store at dbPath (FINDINGS A-217), and
// whether the cause is the db_path itself: what probed, dbPathProblem of dbPath, found, when it is the
// path's fault (pathFault). Then invalid_config, the config's failure, which a corrected db_path
// fixes; otherwise store_unavailable (a corrupt file, a full disk, a file system the kernel remounted
// read-only, a failed recovery), with what probed found, if anything.
func storeFailure(dbPath string, err, probed error) (*sdk.ControlPlaneError, bool) {
	if pathFault(probed) {
		return sdk.InvalidConfig("db_path %q cannot hold the instance store: %v", dbPath, probed), true
	}
	message := fmt.Sprintf("the instance store at %q cannot be used: %v", dbPath, err)
	if probed != nil {
		message += "; " + probed.Error()
	}
	return &sdk.ControlPlaneError{Code: errCodeStoreUnavailable, Message: message}, false
}

// loopTicker is a ticker the receive loop selects on only while it runs: C is nil while it is
// stopped, and a nil channel is never ready.
type loopTicker struct {
	ticker *time.Ticker
	C      <-chan time.Time
}

func (t *loopTicker) start(every time.Duration) {
	if t.ticker == nil {
		t.ticker = time.NewTicker(every)
		t.C = t.ticker.C
	}
}

func (t *loopTicker) stop() {
	if t.ticker != nil {
		t.ticker.Stop()
		t.ticker, t.C = nil, nil
	}
}

// definitionHash is what a definition is kept by: the SHA-256 of its bytes, in hex.
func definitionHash(definitionBytes []byte) string {
	return fmt.Sprintf("%x", sha256.Sum256(definitionBytes))
}
