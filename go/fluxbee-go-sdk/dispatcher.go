package sdk

// RouterDispatcher is the canonical Go-side router transport, mirroring
// `RouterDispatcher` in the Rust SDK. It owns a single (NodeSender,
// NodeReceiver) pair and multiplexes outbound RPCs by `trace_id` against
// caller-supplied response contracts. Non-pending operational traffic is
// routed to per-channel command queues declared in
// `OperationalRouteProfile`.
//
// Design contract (matches Rust):
//   - Exactly one consumer per receiver: this dispatcher owns the
//     receiver goroutine. Nodes never call `receiver.Recv` directly.
//   - Pending matchers come from `SendWithMatcher`; outbound message
//     `routing.trace_id` is auto-assigned when empty.
//   - `post_pending_rules` route incoming traffic that didn't satisfy a
//     pending waiter to a command channel.
//   - Command channels are drained by `TakeCommandReceiver(name)` exactly
//     once per channel.

import (
	"context"
	"errors"
	"fmt"
	"log"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
)

// RouteMatch is the union of pending-matcher / route-rule predicates.
// Concrete implementations: RouteExact, RouteOneOf, RouteAnyMsgOfType,
// RouteAny.
type RouteMatch interface {
	matches(msgType string, msg string) bool
}

type RouteExact struct {
	MsgType string
	Msg     string
}

func (r RouteExact) matches(msgType, msg string) bool {
	return msgType == r.MsgType && msg == r.Msg
}

type RouteOneOf struct {
	MsgType string
	Msgs    []string
}

func (r RouteOneOf) matches(msgType, msg string) bool {
	if msgType != r.MsgType {
		return false
	}
	for _, candidate := range r.Msgs {
		if candidate == msg {
			return true
		}
	}
	return false
}

type RouteAnyMsgOfType struct {
	MsgType string
}

func (r RouteAnyMsgOfType) matches(msgType, _ string) bool {
	return msgType == r.MsgType
}

type RouteAny struct{}

func (RouteAny) matches(string, string) bool { return true }

// PendingMatcher classifies an incoming message against a pending RPC.
// Same semantics as Rust:
//   - Success matches → deliver and resolve.
//   - TerminalError matches → resolve with a transport error.
//   - InvalidResponse matches → resolve with InvalidResponse error
//     (typically AnyMsgOfType(SYSTEMKind) so a noise message on the same
//     trace_id doesn't silently look like a vault payload).
type PendingMatcher struct {
	Success         []RouteMatch
	TerminalError   []RouteMatch
	InvalidResponse []RouteMatch
}

type matchOutcome int

const (
	outcomeUnrelated matchOutcome = iota
	outcomeSuccess
	outcomeTerminalError
	outcomeInvalidResponse
)

func (m PendingMatcher) classify(msgType, msg string) matchOutcome {
	for _, rule := range m.Success {
		if rule.matches(msgType, msg) {
			return outcomeSuccess
		}
	}
	for _, rule := range m.TerminalError {
		if rule.matches(msgType, msg) {
			return outcomeTerminalError
		}
	}
	for _, rule := range m.InvalidResponse {
		if rule.matches(msgType, msg) {
			return outcomeInvalidResponse
		}
	}
	return outcomeUnrelated
}

// RouteTarget for profile rules.
type RouteTarget interface {
	isRouteTarget()
}

type RouteCommand struct {
	Channel string
}

func (RouteCommand) isRouteTarget() {}

type RouteBroadcast struct {
	Channel string
}

func (RouteBroadcast) isRouteTarget() {}

type RouteDrop struct {
	Reason string
}

func (RouteDrop) isRouteTarget() {}

type routeRule struct {
	match  RouteMatch
	target RouteTarget
}

// OperationalRouteProfile declares command channels and the rules that
// route non-pending incoming traffic to them.
type OperationalRouteProfile struct {
	commandChannels   []string
	broadcastChannels []string
	prePendingRules   []routeRule
	postPendingRules  []routeRule
}

// OperationalRouteProfileBuilder fluent builder.
type OperationalRouteProfileBuilder struct {
	commandChannels   []string
	broadcastChannels []string
	prePendingRules   []routeRule
	postPendingRules  []routeRule
}

func NewOperationalRouteProfile() *OperationalRouteProfileBuilder {
	return &OperationalRouteProfileBuilder{}
}

func (b *OperationalRouteProfileBuilder) CommandChannel(name string) *OperationalRouteProfileBuilder {
	b.commandChannels = append(b.commandChannels, name)
	return b
}

func (b *OperationalRouteProfileBuilder) BroadcastChannel(name string) *OperationalRouteProfileBuilder {
	b.broadcastChannels = append(b.broadcastChannels, name)
	return b
}

func (b *OperationalRouteProfileBuilder) PrePendingRule(match RouteMatch, target RouteTarget) *OperationalRouteProfileBuilder {
	b.prePendingRules = append(b.prePendingRules, routeRule{match: match, target: target})
	return b
}

func (b *OperationalRouteProfileBuilder) PostPendingRule(match RouteMatch, target RouteTarget) *OperationalRouteProfileBuilder {
	b.postPendingRules = append(b.postPendingRules, routeRule{match: match, target: target})
	return b
}

func (b *OperationalRouteProfileBuilder) Build() (OperationalRouteProfile, error) {
	if hasDuplicates(b.commandChannels) {
		return OperationalRouteProfile{}, fmt.Errorf("duplicate command channel name")
	}
	if hasDuplicates(b.broadcastChannels) {
		return OperationalRouteProfile{}, fmt.Errorf("duplicate broadcast channel name")
	}
	for _, name := range b.commandChannels {
		if containsString(b.broadcastChannels, name) {
			return OperationalRouteProfile{}, fmt.Errorf("channel %q declared as both command and broadcast", name)
		}
	}
	if err := validateRouteRules("pre_pending_rule", b.prePendingRules, b.commandChannels, b.broadcastChannels); err != nil {
		return OperationalRouteProfile{}, err
	}
	if err := validateRouteRules("post_pending_rule", b.postPendingRules, b.commandChannels, b.broadcastChannels); err != nil {
		return OperationalRouteProfile{}, err
	}
	return OperationalRouteProfile{
		commandChannels:   append([]string(nil), b.commandChannels...),
		broadcastChannels: append([]string(nil), b.broadcastChannels...),
		prePendingRules:   append([]routeRule(nil), b.prePendingRules...),
		postPendingRules:  append([]routeRule(nil), b.postPendingRules...),
	}, nil
}

// CommandChannels returns the declared channel names.
func (p OperationalRouteProfile) CommandChannels() []string {
	return append([]string(nil), p.commandChannels...)
}

func (p OperationalRouteProfile) BroadcastChannels() []string {
	return append([]string(nil), p.broadcastChannels...)
}

func validateRouteRules(label string, rules []routeRule, commandChannels, broadcastChannels []string) error {
	for _, rule := range rules {
		switch target := rule.target.(type) {
		case RouteCommand:
			if !containsString(commandChannels, target.Channel) {
				return fmt.Errorf("%s routes to undeclared command channel %q", label, target.Channel)
			}
		case RouteBroadcast:
			if !containsString(broadcastChannels, target.Channel) {
				return fmt.Errorf("%s routes to undeclared broadcast channel %q", label, target.Channel)
			}
		case RouteDrop:
		default:
			return fmt.Errorf("%s has unsupported target %T", label, rule.target)
		}
	}
	return nil
}

func containsString(values []string, needle string) bool {
	for _, value := range values {
		if value == needle {
			return true
		}
	}
	return false
}

func hasDuplicates(values []string) bool {
	seen := make(map[string]bool, len(values))
	for _, value := range values {
		if seen[value] {
			return true
		}
		seen[value] = true
	}
	return false
}

// RpcError is the dispatcher-level error union.
type RpcError struct {
	Kind    RpcErrorKind
	TraceID string
	Target  string
	Verb    string
	Message string
	Cause   error
}

type RpcErrorKind int

const (
	RpcErrInvalidRequest RpcErrorKind = iota
	RpcErrTimeout
	RpcErrDisconnected
	RpcErrInvalidResponse
	RpcErrTerminalTransport
	RpcErrUnknown
)

func (e *RpcError) Error() string {
	if e == nil {
		return ""
	}
	switch e.Kind {
	case RpcErrTimeout:
		return fmt.Sprintf("rpc timeout: trace_id=%s target=%s verb=%s", e.TraceID, e.Target, e.Verb)
	case RpcErrDisconnected:
		return "rpc disconnected"
	case RpcErrInvalidResponse:
		return fmt.Sprintf("rpc invalid response: %s", e.Message)
	case RpcErrTerminalTransport:
		return fmt.Sprintf("rpc terminal transport error: %s", e.Message)
	case RpcErrInvalidRequest:
		return fmt.Sprintf("rpc invalid request: %s", e.Message)
	default:
		if e.Cause != nil {
			return fmt.Sprintf("rpc error: %s", e.Cause.Error())
		}
		return "rpc error"
	}
}

func (e *RpcError) Unwrap() error {
	return e.Cause
}

// RpcRequestLabels are diagnostic labels carried in timeouts and errors.
type RpcRequestLabels struct {
	Target      string
	RequestMsg  string
	ResponseMsg string
}

type SystemRpcRequest struct {
	Target      string
	RequestMsg  string
	ResponseMsg string
	Payload     any
	Timeout     time.Duration
	Options     SystemEnvelopeOptions
}

const (
	ADMINKind               = "admin"
	MsgAdminCommand         = "ADMIN_COMMAND"
	MsgAdminCommandResponse = "ADMIN_COMMAND_RESPONSE"
)

type AdminRpcRequest struct {
	AdminTarget string
	Action      string
	Target      string
	Params      any
	RequestID   string
	Timeout     time.Duration
}

type pendingEntry struct {
	matcher PendingMatcher
	deliver chan pendingResult
	labels  RpcRequestLabels
	traceID string
	target  string
	verb    string
}

type pendingResult struct {
	msg Message
	err *RpcError
}

// GO-1 constants. RECENT_STALE_TTL matches the Rust SDK so late
// responses are classified the same way across languages. The bound on
// the table avoids unbounded memory under churn.
const (
	recentStaleTTL = 30 * time.Second
	recentStaleMax = 1024
)

// staleEntry records the matcher that closed out a trace_id so a late
// reply on the same trace can be classified the same way the original
// completion would have.
type staleEntry struct {
	matcher   PendingMatcher
	expiresAt time.Time
}

// RouterDispatcher is the canonical Go router transport.
type RouterDispatcher struct {
	sender   *NodeSender
	receiver *NodeReceiver
	profile  OperationalRouteProfile

	pendingMu sync.Mutex
	pending   map[string]*pendingEntry

	commandsMu sync.Mutex
	commands   map[string]chan Message
	taken      map[string]bool

	broadcastsMu sync.Mutex
	broadcasts   map[string][]chan Message

	// GO-2: per-channel drop counters and a one-shot warn flag so command
	// channels that fall behind a slow consumer report drops instead of
	// silently discarding messages. Rust uses unbounded mpsc; here we keep
	// the channel bounded at construction (default 64) and surface drops
	// as visible signals.
	dropMu        sync.Mutex
	commandDrops  map[string]uint64
	commandWarned map[string]bool

	// GO-1: recent-stale registry and response-only matcher set. Together
	// they let `deliver` classify late replies (Stale) and orphaned
	// responses (Unknown) instead of routing them through
	// postPendingRules where they would look like legitimate operational
	// traffic. Mirror of Rust `RecentStaleTable` + `response_only`.
	staleMu          sync.Mutex
	staleEntries     map[string]staleEntry
	staleOrder       []string
	staleDrops       uint64
	responseOnly     map[RouteMatch]struct{}
	unknownRespDrops uint64

	closeOnce sync.Once
	done      chan struct{}
}

// ConnectWithRetry connects to the router and starts the dispatcher
// goroutine. The dispatcher is the single consumer of the underlying
// NodeReceiver.
func ConnectWithRetry(cfg NodeConfig, delay time.Duration, profile OperationalRouteProfile) (*RouterDispatcher, error) {
	sender, receiver, err := connect(cfg)
	if err != nil {
		if delay <= 0 {
			return nil, err
		}
		for {
			time.Sleep(delay)
			sender, receiver, err = connect(cfg)
			if err == nil {
				break
			}
		}
	}
	d := &RouterDispatcher{
		sender:        sender,
		receiver:      receiver,
		profile:       profile,
		pending:       make(map[string]*pendingEntry),
		commands:      make(map[string]chan Message),
		taken:         make(map[string]bool),
		broadcasts:    make(map[string][]chan Message),
		commandDrops:  make(map[string]uint64),
		commandWarned: make(map[string]bool),
		staleEntries:  make(map[string]staleEntry),
		responseOnly:  make(map[RouteMatch]struct{}),
		done:          make(chan struct{}),
	}
	for _, name := range profile.commandChannels {
		d.commands[name] = make(chan Message, 64)
	}
	for _, name := range profile.broadcastChannels {
		d.broadcasts[name] = nil
	}
	go d.dispatchLoop()
	return d, nil
}

// SenderSnapshot returns the underlying sender. Callers can `Send` outbound
// messages directly through it (e.g. for replies that don't need
// trace-id-multiplexed waiting).
func (d *RouterDispatcher) SenderSnapshot() *NodeSender {
	return d.sender
}

// TakeCommandReceiver returns the channel for an operational command
// channel declared in the profile. Each channel can only be taken once.
func (d *RouterDispatcher) TakeCommandReceiver(name string) (<-chan Message, error) {
	d.commandsMu.Lock()
	defer d.commandsMu.Unlock()
	if d.taken[name] {
		return nil, fmt.Errorf("command channel %q already taken", name)
	}
	ch, ok := d.commands[name]
	if !ok {
		return nil, fmt.Errorf("command channel %q not declared in profile", name)
	}
	d.taken[name] = true
	return ch, nil
}

func (d *RouterDispatcher) Subscribe(name string) (<-chan Message, error) {
	d.broadcastsMu.Lock()
	defer d.broadcastsMu.Unlock()
	if _, ok := d.broadcasts[name]; !ok {
		return nil, fmt.Errorf("broadcast channel %q not declared in profile", name)
	}
	ch := make(chan Message, 64)
	d.broadcasts[name] = append(d.broadcasts[name], ch)
	return ch, nil
}

func (d *RouterDispatcher) Close() error {
	var err error
	d.closeOnce.Do(func() {
		if d.sender != nil {
			err = d.sender.Close()
		}
		close(d.done)
	})
	return err
}

func (d *RouterDispatcher) SendSystemRPC(ctx context.Context, request SystemRpcRequest) (Message, error) {
	target := strings.TrimSpace(request.Target)
	requestMsg := strings.TrimSpace(request.RequestMsg)
	responseMsg := strings.TrimSpace(request.ResponseMsg)
	if target == "" {
		return Message{}, &RpcError{Kind: RpcErrInvalidRequest, Message: "target must be non-empty"}
	}
	if requestMsg == "" {
		return Message{}, &RpcError{Kind: RpcErrInvalidRequest, Message: "request_msg must be non-empty"}
	}
	if responseMsg == "" {
		return Message{}, &RpcError{Kind: RpcErrInvalidRequest, Message: "response_msg must be non-empty"}
	}
	msg, err := BuildSystemRequest(
		d.sender.UUID(),
		target,
		requestMsg,
		request.Payload,
		uuid.NewString(),
		request.Options,
	)
	if err != nil {
		return Message{}, err
	}
	matcher := PendingMatcher{
		Success: []RouteMatch{RouteExact{MsgType: SYSTEMKind, Msg: responseMsg}},
		TerminalError: []RouteMatch{
			RouteExact{MsgType: SYSTEMKind, Msg: MSGUnreachable},
			RouteExact{MsgType: SYSTEMKind, Msg: MSGTTLExceeded},
		},
		InvalidResponse: []RouteMatch{RouteAnyMsgOfType{MsgType: SYSTEMKind}},
	}
	return d.SendWithMatcher(ctx, msg, matcher, RpcRequestLabels{
		Target:      target,
		RequestMsg:  requestMsg,
		ResponseMsg: responseMsg,
	}, request.Timeout)
}

func (d *RouterDispatcher) SendAdminRPC(ctx context.Context, request AdminRpcRequest) (Message, error) {
	adminTarget := strings.TrimSpace(request.AdminTarget)
	action := strings.TrimSpace(request.Action)
	if adminTarget == "" {
		return Message{}, &RpcError{Kind: RpcErrInvalidRequest, Message: "admin_target must be non-empty"}
	}
	if action == "" {
		return Message{}, &RpcError{Kind: RpcErrInvalidRequest, Message: "action must be non-empty"}
	}
	requestID := strings.TrimSpace(request.RequestID)
	if requestID == "" {
		requestID = uuid.NewString()
	}
	params := request.Params
	if params == nil {
		params = map[string]any{}
	}
	payload := map[string]any{
		"action":     action,
		"params":     params,
		"request_id": requestID,
	}
	if target := strings.TrimSpace(request.Target); target != "" {
		payload["target"] = target
	}
	raw, err := MarshalPayload(payload)
	if err != nil {
		return Message{}, err
	}
	msgCopy := MsgAdminCommand
	actionCopy := action
	msg := Message{
		Routing: Routing{
			Src:     d.sender.UUID(),
			Dst:     UnicastDestination(adminTarget),
			TTL:     16,
			TraceID: uuid.NewString(),
		},
		Meta: Meta{
			MsgType: ADMINKind,
			Msg:     &msgCopy,
			Target:  &adminTarget,
			Action:  &actionCopy,
		},
		Payload: raw,
	}
	matcher := PendingMatcher{
		Success: []RouteMatch{RouteExact{MsgType: ADMINKind, Msg: MsgAdminCommandResponse}},
		TerminalError: []RouteMatch{
			RouteExact{MsgType: SYSTEMKind, Msg: MSGUnreachable},
			RouteExact{MsgType: SYSTEMKind, Msg: MSGTTLExceeded},
		},
		InvalidResponse: []RouteMatch{RouteAnyMsgOfType{MsgType: ADMINKind}},
	}
	return d.SendWithMatcher(ctx, msg, matcher, RpcRequestLabels{
		Target:      adminTarget,
		RequestMsg:  MsgAdminCommand,
		ResponseMsg: MsgAdminCommandResponse,
	}, request.Timeout)
}

// SendWithMatcher sends an outbound message and blocks until the matcher
// classifies an incoming message (or until timeout / context cancel).
func (d *RouterDispatcher) SendWithMatcher(
	ctx context.Context,
	msg Message,
	matcher PendingMatcher,
	labels RpcRequestLabels,
	timeout time.Duration,
) (Message, error) {
	if !d.receiver.IsConnected() {
		return Message{}, &RpcError{Kind: RpcErrDisconnected}
	}
	if timeout <= 0 {
		timeout = 5 * time.Second
	}
	if strings.TrimSpace(msg.Routing.TraceID) == "" {
		msg.Routing.TraceID = uuid.NewString()
	}
	if strings.TrimSpace(msg.Routing.Src) == "" {
		msg.Routing.Src = d.sender.UUID()
	}
	traceID := msg.Routing.TraceID

	entry := &pendingEntry{
		matcher: matcher,
		deliver: make(chan pendingResult, 1),
		labels:  labels,
		traceID: traceID,
		target:  labels.Target,
		verb:    labels.RequestMsg,
	}
	d.pendingMu.Lock()
	if _, exists := d.pending[traceID]; exists {
		d.pendingMu.Unlock()
		return Message{}, &RpcError{
			Kind:    RpcErrInvalidRequest,
			Message: fmt.Sprintf("duplicate active trace_id %s", traceID),
		}
	}
	d.pending[traceID] = entry
	d.pendingMu.Unlock()

	if err := d.sender.Send(msg); err != nil {
		d.removePending(traceID)
		return Message{}, &RpcError{Kind: RpcErrDisconnected, Cause: err}
	}
	d.registerResponseOnly(matcher)

	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	select {
	case res := <-entry.deliver:
		if res.err != nil {
			return Message{}, res.err
		}
		return res.msg, nil
	case <-deadline.C:
		d.removePending(traceID)
		// GO-1: remember the matcher so a late reply on this trace_id is
		// classified as Stale instead of misrouted through
		// post_pending_rules.
		d.noteStale(traceID, matcher)
		return Message{}, &RpcError{
			Kind:    RpcErrTimeout,
			TraceID: traceID,
			Target:  labels.Target,
			Verb:    labels.RequestMsg,
			Message: fmt.Sprintf("timeout after %dms", timeout.Milliseconds()),
		}
	case <-ctx.Done():
		d.removePending(traceID)
		d.noteStale(traceID, matcher)
		return Message{}, ctx.Err()
	case <-d.done:
		d.removePending(traceID)
		// Dispatcher shutting down — no point noting stale; nothing will
		// consume the registry.
		return Message{}, &RpcError{Kind: RpcErrDisconnected, Message: "dispatcher closed"}
	}
}

func (d *RouterDispatcher) removePending(traceID string) {
	d.pendingMu.Lock()
	delete(d.pending, traceID)
	d.pendingMu.Unlock()
}

func (d *RouterDispatcher) dispatchLoop() {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		<-d.done
		cancel()
	}()
	for {
		select {
		case <-d.done:
			return
		default:
		}
		msg, err := d.receiver.Recv(ctx)
		if err != nil {
			if errors.Is(err, context.Canceled) {
				return
			}
			// The connection manager reconnects transparently and resumes
			// feeding the same receiver channel. Drain in-flight waiters, but
			// keep the dispatcher alive so the reconnected link is consumed.
			d.failAllPending(&RpcError{Kind: RpcErrDisconnected, Cause: err})
			select {
			case <-d.done:
				return
			case <-time.After(50 * time.Millisecond):
			}
			continue
		}
		d.deliver(msg)
	}
}

func (d *RouterDispatcher) deliver(msg Message) {
	traceID := msg.Routing.TraceID
	msgName := stringValue(msg.Meta.Msg)
	if d.routeByRules(d.profile.prePendingRules, msg, msgName) {
		return
	}
	if traceID != "" {
		d.pendingMu.Lock()
		entry, ok := d.pending[traceID]
		d.pendingMu.Unlock()
		if ok {
			outcome := entry.matcher.classify(msg.Meta.MsgType, msgName)
			switch outcome {
			case outcomeSuccess:
				d.removePending(traceID)
				d.noteStale(traceID, entry.matcher)
				entry.deliver <- pendingResult{msg: msg}
				return
			case outcomeTerminalError:
				d.removePending(traceID)
				d.noteStale(traceID, entry.matcher)
				entry.deliver <- pendingResult{
					err: &RpcError{
						Kind:    RpcErrTerminalTransport,
						TraceID: traceID,
						Target:  entry.target,
						Verb:    entry.verb,
						Message: fmt.Sprintf("terminal transport response %q", msgName),
					},
				}
				return
			case outcomeInvalidResponse:
				d.removePending(traceID)
				d.noteStale(traceID, entry.matcher)
				entry.deliver <- pendingResult{
					err: &RpcError{
						Kind:    RpcErrInvalidResponse,
						TraceID: traceID,
						Target:  entry.target,
						Verb:    entry.verb,
						Message: fmt.Sprintf("invalid response %q for verb %q", msgName, entry.verb),
					},
				}
				return
			case outcomeUnrelated:
				// fall through to stale / response-only / post_pending_rules
			}
		}
	}
	// GO-1 step 3: late response on a recently-completed trace_id. Drop with
	// a metric — never route to post_pending_rules where it would look like
	// fresh operational traffic.
	if d.classifyStale(msg, msgName) {
		return
	}
	// GO-1 step 4: orphaned response-only shape. The msg matches a
	// response-shape we have seen from a SendWithMatcher but there is no
	// pending matcher waiting (already timed out, never registered, etc.).
	if d.classifyUnknownResponse(msg, msgName) {
		return
	}
	_ = d.routeByRules(d.profile.postPendingRules, msg, msgName)
}

// noteStale records the matcher that closed out a trace_id so a late
// reply on the same trace can be classified the same way the original
// completion would have. Garbage-collects entries past `recentStaleTTL`.
func (d *RouterDispatcher) noteStale(traceID string, matcher PendingMatcher) {
	if traceID == "" {
		return
	}
	d.staleMu.Lock()
	defer d.staleMu.Unlock()
	d.gcStaleLocked(time.Now())
	if len(d.staleEntries) >= recentStaleMax {
		// Evict the oldest entry to bound memory.
		if len(d.staleOrder) > 0 {
			oldest := d.staleOrder[0]
			d.staleOrder = d.staleOrder[1:]
			delete(d.staleEntries, oldest)
		}
	}
	if _, exists := d.staleEntries[traceID]; !exists {
		d.staleOrder = append(d.staleOrder, traceID)
	}
	d.staleEntries[traceID] = staleEntry{
		matcher:   matcher,
		expiresAt: time.Now().Add(recentStaleTTL),
	}
}

// classifyStale checks whether `msg.Routing.TraceID` matches a
// recently-completed trace AND `msg` matches that entry's success /
// terminal-error / invalid-response shape. If yes, bump the stale-drop
// counter and return true (caller stops dispatching).
func (d *RouterDispatcher) classifyStale(msg Message, msgName string) bool {
	traceID := msg.Routing.TraceID
	if traceID == "" {
		return false
	}
	d.staleMu.Lock()
	defer d.staleMu.Unlock()
	d.gcStaleLocked(time.Now())
	entry, ok := d.staleEntries[traceID]
	if !ok {
		return false
	}
	outcome := entry.matcher.classify(msg.Meta.MsgType, msgName)
	switch outcome {
	case outcomeSuccess, outcomeTerminalError, outcomeInvalidResponse:
		d.staleDrops++
		return true
	default:
		return false
	}
}

// gcStaleLocked walks the FIFO order from oldest and removes expired
// entries. Caller must hold staleMu.
func (d *RouterDispatcher) gcStaleLocked(now time.Time) {
	for len(d.staleOrder) > 0 {
		front := d.staleOrder[0]
		entry, ok := d.staleEntries[front]
		if !ok {
			d.staleOrder = d.staleOrder[1:]
			continue
		}
		if entry.expiresAt.After(now) {
			return
		}
		d.staleOrder = d.staleOrder[1:]
		delete(d.staleEntries, front)
	}
}

// registerResponseOnly remembers the response shapes a SendWithMatcher
// is expecting so the dispatcher can flag orphaned arrivals (responses
// that match a registered shape but have no pending matcher and are not
// stale). Unrolls `RouteOneOf` into `RouteExact` because Go slices are
// not comparable and would otherwise be unusable as map keys.
func (d *RouterDispatcher) registerResponseOnly(matcher PendingMatcher) {
	d.staleMu.Lock()
	defer d.staleMu.Unlock()
	for _, rule := range matcher.Success {
		switch r := rule.(type) {
		case RouteExact:
			if !d.postPendingDeclaresObservationalExact(r.MsgType, r.Msg) {
				d.responseOnly[r] = struct{}{}
			}
		case RouteOneOf:
			for _, m := range r.Msgs {
				if !d.postPendingDeclaresObservationalExact(r.MsgType, m) {
					d.responseOnly[RouteExact{MsgType: r.MsgType, Msg: m}] = struct{}{}
				}
			}
		case RouteAnyMsgOfType:
			if !d.postPendingDeclaresObservationalFamily(r.MsgType) {
				d.responseOnly[r] = struct{}{}
			}
		case RouteAny:
			// Skip — registering RouteAny would drop everything as
			// orphaned. Mirror of Rust behavior.
		}
	}
}

// classifyUnknownResponse returns true if the msg matches any registered
// response-only shape but had no pending matcher and is not stale.
func (d *RouterDispatcher) classifyUnknownResponse(msg Message, msgName string) bool {
	d.staleMu.Lock()
	defer d.staleMu.Unlock()
	for rule := range d.responseOnly {
		if rule.matches(msg.Meta.MsgType, msgName) {
			d.unknownRespDrops++
			return true
		}
	}
	return false
}

// postPendingDeclaresObservationalExact returns true if the profile has a
// post_pending rule that matches (msgType, msg) AND routes to a
// Broadcast target. Such a rule means the response shape is intentionally
// observational (broadcast fan-out) and should NOT be registered as
// response-only.
func (d *RouterDispatcher) postPendingDeclaresObservationalExact(msgType, msg string) bool {
	for _, rule := range d.profile.postPendingRules {
		if !rule.match.matches(msgType, msg) {
			continue
		}
		if _, ok := rule.target.(RouteBroadcast); ok {
			return true
		}
	}
	return false
}

// postPendingDeclaresObservationalFamily returns true if any post_pending
// rule for the same msgType routes to a Broadcast target.
func (d *RouterDispatcher) postPendingDeclaresObservationalFamily(msgType string) bool {
	for _, rule := range d.profile.postPendingRules {
		switch r := rule.match.(type) {
		case RouteAnyMsgOfType:
			if r.MsgType == msgType {
				if _, ok := rule.target.(RouteBroadcast); ok {
					return true
				}
			}
		case RouteExact:
			if r.MsgType == msgType {
				if _, ok := rule.target.(RouteBroadcast); ok {
					return true
				}
			}
		}
	}
	return false
}

// StaleResponseDrops returns the number of late responses (matching a
// recently-completed trace_id within the TTL window) the dispatcher has
// dropped instead of misrouting through post_pending_rules.
func (d *RouterDispatcher) StaleResponseDrops() uint64 {
	d.staleMu.Lock()
	defer d.staleMu.Unlock()
	return d.staleDrops
}

// UnknownResponseDrops returns the number of orphaned response-shape
// messages the dispatcher has dropped (arrivals matching a registered
// response shape but with no pending matcher and not classified as
// stale).
func (d *RouterDispatcher) UnknownResponseDrops() uint64 {
	d.staleMu.Lock()
	defer d.staleMu.Unlock()
	return d.unknownRespDrops
}

func (d *RouterDispatcher) routeByRules(rules []routeRule, msg Message, msgName string) bool {
	for _, rule := range rules {
		if !rule.match.matches(msg.Meta.MsgType, msgName) {
			continue
		}
		return d.routeToTarget(rule.target, msg)
	}
	return false
}

func (d *RouterDispatcher) routeToTarget(target RouteTarget, msg Message) bool {
	switch t := target.(type) {
	case RouteCommand:
		d.commandsMu.Lock()
		ch, ok := d.commands[t.Channel]
		d.commandsMu.Unlock()
		if !ok {
			return true
		}
		select {
		case ch <- msg:
		default:
			// GO-2: the command channel is full. Bump the per-channel
			// drop counter and log a single warning per channel so the
			// signal is visible without flooding logs under sustained
			// backpressure. Operators can inspect counts via
			// CommandChannelDrops.
			d.noteCommandDrop(t.Channel, msg)
		}
		return true
	case RouteBroadcast:
		d.broadcastsMu.Lock()
		subscribers := append([]chan Message(nil), d.broadcasts[t.Channel]...)
		d.broadcastsMu.Unlock()
		for _, ch := range subscribers {
			select {
			case ch <- msg:
			default:
				// Broadcast subscribers are independent; a slow one does
				// not block the others. Each drop is still counted so a
				// consumer can be identified as falling behind.
				d.noteBroadcastDrop(t.Channel)
			}
		}
		return true
	case RouteDrop:
		return true
	default:
		return false
	}
}

// noteCommandDrop bumps the drop counter for the named command channel and
// emits a one-shot warning the first time a drop happens on that channel.
func (d *RouterDispatcher) noteCommandDrop(channel string, msg Message) {
	d.dropMu.Lock()
	d.commandDrops[channel]++
	firstTime := !d.commandWarned[channel]
	if firstTime {
		d.commandWarned[channel] = true
	}
	d.dropMu.Unlock()
	if firstTime {
		log.Printf(
			"fluxbee-go-sdk: command channel %q is FULL — dropped message msg_type=%q msg=%q. "+
				"This is the only warning for this channel; total drops are available via CommandChannelDrops().",
			channel, msg.Meta.MsgType, stringValue(msg.Meta.Msg),
		)
	}
}

// noteBroadcastDrop bumps the drop counter for a broadcast channel slot.
func (d *RouterDispatcher) noteBroadcastDrop(channel string) {
	d.dropMu.Lock()
	d.commandDrops["broadcast:"+channel]++
	d.dropMu.Unlock()
}

// CommandChannelDrops returns a snapshot of how many messages have been
// dropped per command channel due to a full buffer. Counters are
// monotonic. Broadcast-channel drops are reported with a `broadcast:`
// prefix on the channel name.
func (d *RouterDispatcher) CommandChannelDrops() map[string]uint64 {
	d.dropMu.Lock()
	defer d.dropMu.Unlock()
	out := make(map[string]uint64, len(d.commandDrops))
	for name, count := range d.commandDrops {
		out[name] = count
	}
	return out
}

func (d *RouterDispatcher) failAllPending(err *RpcError) {
	d.pendingMu.Lock()
	pending := d.pending
	d.pending = make(map[string]*pendingEntry)
	d.pendingMu.Unlock()
	for _, entry := range pending {
		entry.deliver <- pendingResult{err: err}
	}
}

// IsConnected reports the dispatcher's view of the router link.
func (d *RouterDispatcher) IsConnected() bool {
	return d.receiver.IsConnected()
}
