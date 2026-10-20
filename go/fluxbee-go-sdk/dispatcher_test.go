package sdk

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"
)

func TestDispatcherKeepsRunningAfterReceiverError(t *testing.T) {
	dispatcher, _, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "incoming"}))
	defer func() { _ = dispatcher.Close() }()
	incoming, err := dispatcher.TakeCommandReceiver("incoming")
	if err != nil {
		t.Fatalf("take incoming: %v", err)
	}

	rx <- receivedMessage{err: fmt.Errorf("socket closed")}
	rx <- receivedMessage{msg: Message{Meta: Meta{MsgType: "user"}}}

	select {
	case msg := <-incoming:
		if msg.Meta.MsgType != "user" {
			t.Fatalf("unexpected routed message: %+v", msg)
		}
	case <-time.After(time.Second):
		t.Fatalf("dispatcher did not route after receiver error")
	}
}

func TestDispatcherPrePendingWinsBeforePending(t *testing.T) {
	dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("system").
		PrePendingRule(RouteExact{MsgType: SYSTEMKind, Msg: "CONFIG_GET"}, RouteCommand{Channel: "system"}))
	defer func() { _ = dispatcher.Close() }()
	system, err := dispatcher.TakeCommandReceiver("system")
	if err != nil {
		t.Fatalf("take system: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 150*time.Millisecond)
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, err := dispatcher.SendSystemRPC(ctx, SystemRpcRequest{
			Target:      "SY.target@motherbee",
			RequestMsg:  "CONFIG_GET",
			ResponseMsg: "CONFIG_RESPONSE",
			Payload:     map[string]any{},
			Timeout:     time.Second,
		})
		done <- err
	}()

	frame := <-tx
	request := decodeTestFrame(t, frame)
	rx <- receivedMessage{msg: Message{
		Routing: Routing{TraceID: request.Routing.TraceID},
		Meta:    Meta{MsgType: SYSTEMKind, Msg: stringPtr("CONFIG_GET")},
	}}

	select {
	case routed := <-system:
		if routed.Routing.TraceID != request.Routing.TraceID {
			t.Fatalf("unexpected routed trace: %q", routed.Routing.TraceID)
		}
	case <-time.After(time.Second):
		t.Fatalf("pre-pending message was not routed")
	}

	if err := <-done; err == nil {
		t.Fatalf("pending waiter should not have consumed pre-pending traffic")
	}
}

func TestDispatcherBroadcastSubscriberReceivesPostPendingMatch(t *testing.T) {
	dispatcher, _, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		BroadcastChannel("config").
		PostPendingRule(RouteExact{MsgType: SYSTEMKind, Msg: "CONFIG_RESPONSE"}, RouteBroadcast{Channel: "config"}))
	defer func() { _ = dispatcher.Close() }()
	sub, err := dispatcher.Subscribe("config")
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}

	rx <- receivedMessage{msg: Message{Meta: Meta{MsgType: SYSTEMKind, Msg: stringPtr("CONFIG_RESPONSE")}}}

	select {
	case msg := <-sub:
		if stringValue(msg.Meta.Msg) != "CONFIG_RESPONSE" {
			t.Fatalf("unexpected broadcast message: %+v", msg)
		}
	case <-time.After(time.Second):
		t.Fatalf("broadcast subscriber did not receive message")
	}
}

func newDispatcherTestHarness(t *testing.T, builder *OperationalRouteProfileBuilder) (*RouterDispatcher, chan []byte, chan receivedMessage) {
	t.Helper()
	profile, err := builder.Build()
	if err != nil {
		t.Fatalf("build profile: %v", err)
	}
	state := &connectionState{connected: true}
	tx := make(chan []byte, 8)
	rx := make(chan receivedMessage, 8)
	sender := &NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: state}
	receiver := &NodeReceiver{uuid: "src-1", fullName: "WF.demo@motherbee", rx: rx, state: state}
	dispatcher := &RouterDispatcher{
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
		dispatcher.commands[name] = make(chan Message, 64)
	}
	for _, name := range profile.broadcastChannels {
		dispatcher.broadcasts[name] = nil
	}
	go dispatcher.dispatchLoop()
	return dispatcher, tx, rx
}

func decodeTestFrame(t *testing.T, frame []byte) Message {
	t.Helper()
	var msg Message
	if err := json.Unmarshal(frame, &msg); err != nil {
		t.Fatalf("decode frame: %v", err)
	}
	return msg
}

// GO-2 — when a command channel fills up because the consumer is not
// reading, the dispatcher must bump the per-channel drop counter visible
// via `CommandChannelDrops()` instead of silently discarding messages.
func TestDispatcherCommandChannelDropsAreCounted(t *testing.T) {
	dispatcher, _, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "incoming"}))
	defer func() { _ = dispatcher.Close() }()

	// Shrink the command channel buffer so we can saturate it without
	// flooding the test. 4 slots + 10 messages = 6 drops.
	dispatcher.commandsMu.Lock()
	dispatcher.commands["incoming"] = make(chan Message, 4)
	dispatcher.commandsMu.Unlock()

	// Note: we don't take the receiver — we want the buffer to fill up
	// and overflow.
	for i := 0; i < 10; i++ {
		rx <- receivedMessage{msg: Message{Meta: Meta{MsgType: "user"}}}
	}

	// Allow the dispatchLoop time to process every receivedMessage.
	deadline := time.Now().Add(time.Second)
	var drops uint64
	for time.Now().Before(deadline) {
		counts := dispatcher.CommandChannelDrops()
		drops = counts["incoming"]
		if drops >= 6 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	if drops != 6 {
		t.Fatalf("expected 6 drops on 'incoming' (10 sent, 4 buffered), got %d", drops)
	}
}

// GO-1 — a response arriving AFTER the caller's timeout (so the pending
// entry has been removed) but on a trace_id the dispatcher remembers as
// recently-completed must be classified as Stale, bump the stale-drop
// counter, and NOT route through post_pending_rules. The previous Go
// dispatcher would have routed the late reply to "incoming" as if it
// were fresh operational traffic.
func TestDispatcherLateResponseIsClassifiedAsStale(t *testing.T) {
	dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "incoming"}))
	defer func() { _ = dispatcher.Close() }()
	incoming, err := dispatcher.TakeCommandReceiver("incoming")
	if err != nil {
		t.Fatalf("take incoming: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, err := dispatcher.SendSystemRPC(ctx, SystemRpcRequest{
			Target:      "SY.timer@motherbee",
			RequestMsg:  "TIMER_NOW",
			ResponseMsg: "TIMER_RESPONSE",
			Payload:     map[string]any{},
			Timeout:     50 * time.Millisecond,
		})
		done <- err
	}()

	frame := <-tx
	request := decodeTestFrame(t, frame)
	// Wait for timeout to fire so the pending entry is removed and the
	// trace_id is recorded as stale via SendWithMatcher's deferred path.
	if err := <-done; err == nil {
		t.Fatalf("expected timeout error from SendSystemRPC")
	}

	// Now send the late response. It should be classified as Stale, not
	// routed to "incoming".
	rx <- receivedMessage{msg: Message{
		Routing: Routing{TraceID: request.Routing.TraceID},
		Meta:    Meta{MsgType: SYSTEMKind, Msg: stringPtr("TIMER_RESPONSE")},
	}}

	// Give the dispatchLoop a moment.
	deadline := time.Now().Add(500 * time.Millisecond)
	var staleDrops uint64
	for time.Now().Before(deadline) {
		staleDrops = dispatcher.StaleResponseDrops()
		if staleDrops > 0 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	if staleDrops != 1 {
		t.Fatalf("expected 1 stale-response drop, got %d", staleDrops)
	}

	// The late response must NOT have leaked into the operational
	// channel.
	select {
	case stray := <-incoming:
		t.Fatalf("late response leaked into 'incoming' channel: %+v", stray)
	case <-time.After(50 * time.Millisecond):
		// expected — nothing arrived
	}
}

// GO-1 — a message that matches a registered response-shape but has no
// pending matcher and is not stale gets classified as Unknown (orphan)
// and dropped with a counter. This catches "the verb fired AND completed
// AND the trace_id evicted from stale, then a duplicate arrives" — rare
// but worth surfacing instead of misrouting.
func TestDispatcherOrphanResponseShapeIsClassifiedAsUnknown(t *testing.T) {
	dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "incoming"}))
	defer func() { _ = dispatcher.Close() }()
	incoming, err := dispatcher.TakeCommandReceiver("incoming")
	if err != nil {
		t.Fatalf("take incoming: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel()
	go func() {
		_, _ = dispatcher.SendSystemRPC(ctx, SystemRpcRequest{
			Target:      "SY.timer@motherbee",
			RequestMsg:  "TIMER_NOW",
			ResponseMsg: "TIMER_RESPONSE",
			Payload:     map[string]any{},
			Timeout:     50 * time.Millisecond,
		})
	}()

	// Drain the outbound frame so SendWithMatcher actually registers the
	// response-shape (TIMER_RESPONSE) before we trigger the orphan check.
	<-tx

	// Wait until the response-shape registration has actually landed —
	// SendWithMatcher does it after sender.Send, which races with the
	// goroutine scheduling. A 50ms-then-poll loop is good enough.
	deadline := time.Now().Add(time.Second)
	registered := false
	for time.Now().Before(deadline) {
		dispatcher.staleMu.Lock()
		_, registered = dispatcher.responseOnly[RouteExact{
			MsgType: SYSTEMKind,
			Msg:     "TIMER_RESPONSE",
		}]
		dispatcher.staleMu.Unlock()
		if registered {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	if !registered {
		t.Fatalf("response-only registration did not appear")
	}

	// Force the stale TTL to elapse for the original trace by manually
	// evicting (faster than waiting 30s).
	dispatcher.staleMu.Lock()
	dispatcher.staleEntries = make(map[string]staleEntry)
	dispatcher.staleOrder = nil
	dispatcher.staleMu.Unlock()

	// Send a TIMER_RESPONSE on a *fresh, unrelated* trace_id. No pending
	// waiter, not stale, but matches the registered response-shape.
	rx <- receivedMessage{msg: Message{
		Routing: Routing{TraceID: "orphan-trace"},
		Meta:    Meta{MsgType: SYSTEMKind, Msg: stringPtr("TIMER_RESPONSE")},
	}}

	deadline = time.Now().Add(500 * time.Millisecond)
	var unknownDrops uint64
	for time.Now().Before(deadline) {
		unknownDrops = dispatcher.UnknownResponseDrops()
		if unknownDrops > 0 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	if unknownDrops != 1 {
		t.Fatalf("expected 1 unknown-response drop, got %d", unknownDrops)
	}

	// The orphan must NOT have leaked into "incoming".
	select {
	case stray := <-incoming:
		t.Fatalf("orphan response leaked into 'incoming' channel: %+v", stray)
	case <-time.After(50 * time.Millisecond):
		// expected
	}
}

// GO-2 — the warn log fires exactly once per channel even under repeated
// drops, so logs do not flood under sustained backpressure.
func TestDispatcherCommandChannelDropWarnsOnceOnly(t *testing.T) {
	dispatcher, _, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "incoming"}))
	defer func() { _ = dispatcher.Close() }()
	dispatcher.commandsMu.Lock()
	dispatcher.commands["incoming"] = make(chan Message, 1)
	dispatcher.commandsMu.Unlock()

	for i := 0; i < 5; i++ {
		rx <- receivedMessage{msg: Message{Meta: Meta{MsgType: "user"}}}
	}

	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		if dispatcher.CommandChannelDrops()["incoming"] >= 4 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	dispatcher.dropMu.Lock()
	warned := dispatcher.commandWarned["incoming"]
	dispatcher.dropMu.Unlock()
	if !warned {
		t.Fatalf("expected the one-shot warn flag to be set after drops")
	}
}

// A-75 — a family-wide success matcher (RouteAnyMsgOfType) is never
// registered as a reply shape: after awaiting a reply that way, a fresh
// message of that kind is still the node's own traffic, on a catch-all
// profile (io.cloud's, the 2026-08-26 outage) as on SY.timer's. A late
// reply on the awaited trace is still dropped as stale. Mirror of Rust
// `family_wide_success_matcher_never_poisons_command_traffic`.
func TestDispatcherFamilySuccessMatcherNeverPoisonsCommandTraffic(t *testing.T) {
	cases := []struct {
		name     string
		catchAll RouteMatch
		kind     string
		command  string
	}{
		{name: "catch-all profile", catchAll: RouteAny{}, kind: "user", command: "user"},
		{name: "SY.timer profile", catchAll: RouteAnyMsgOfType{MsgType: SYSTEMKind}, kind: SYSTEMKind, command: "TIMER_SCHEDULE"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
				CommandChannel("incoming").
				PostPendingRule(tc.catchAll, RouteCommand{Channel: "incoming"}))
			defer func() { _ = dispatcher.Close() }()
			incoming, err := dispatcher.TakeCommandReceiver("incoming")
			if err != nil {
				t.Fatalf("take incoming: %v", err)
			}

			done := make(chan error, 1)
			go func() {
				_, err := dispatcher.SendWithMatcher(context.Background(), Message{
					Routing: Routing{Dst: UnicastDestination("SY.peer@motherbee"), TTL: 16},
					Meta:    Meta{MsgType: tc.kind, Msg: stringPtr("HANDOFF")},
				}, PendingMatcher{Success: []RouteMatch{RouteAnyMsgOfType{MsgType: tc.kind}}},
					RpcRequestLabels{Target: "SY.peer@motherbee", RequestMsg: "HANDOFF"}, 50*time.Millisecond)
				done <- err
			}()
			request := decodeTestFrame(t, <-tx)
			var rpcErr *RpcError
			if err := <-done; !errors.As(err, &rpcErr) || rpcErr.Kind != RpcErrTimeout {
				t.Fatalf("expected a timeout, got %v", err)
			}
			dispatcher.staleMu.Lock()
			registered := len(dispatcher.responseOnly)
			dispatcher.staleMu.Unlock()
			if registered != 0 {
				t.Fatalf("a family success matcher must not be registered as a reply shape (%d registered)", registered)
			}

			rx <- receivedMessage{msg: Message{
				Routing: Routing{TraceID: request.Routing.TraceID},
				Meta:    Meta{MsgType: tc.kind, Msg: stringPtr("LATE_REPLY")},
			}}
			rx <- receivedMessage{msg: Message{
				Routing: Routing{TraceID: "fresh-trace"},
				Meta:    Meta{MsgType: tc.kind, Msg: stringPtr(tc.command)},
			}}
			select {
			case routed := <-incoming:
				if routed.Routing.TraceID != "fresh-trace" {
					t.Fatalf("the late reply leaked into 'incoming': %+v", routed)
				}
			case <-time.After(time.Second):
				t.Fatalf("a fresh %q message was dropped", tc.kind)
			}
			if got := dispatcher.StaleResponseDrops(); got != 1 {
				t.Fatalf("expected the late reply dropped as stale, got %d stale drops", got)
			}
			if got := dispatcher.UnknownResponseDrops(); got != 0 {
				t.Fatalf("expected no unknown-response drops, got %d", got)
			}
		})
	}
}

// A-75 — a family a post-pending rule sends to a broadcast channel keeps
// fanning out after a request awaited a reply of that family. Mirror of
// Rust `response_only_skips_profile_declared_observational_family`.
func TestDispatcherObservationalFamilyKeepsFanningOut(t *testing.T) {
	dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		BroadcastChannel("query").
		PostPendingRule(RouteAnyMsgOfType{MsgType: "query_response"}, RouteBroadcast{Channel: "query"}))
	defer func() { _ = dispatcher.Close() }()
	sub, err := dispatcher.Subscribe("query")
	if err != nil {
		t.Fatalf("subscribe: %v", err)
	}

	done := make(chan error, 1)
	go func() {
		_, err := dispatcher.SendWithMatcher(context.Background(), Message{
			Routing: Routing{Dst: UnicastDestination("SY.target@motherbee"), TTL: 16},
			Meta:    Meta{MsgType: "query", Msg: stringPtr("DO_QUERY")},
		}, PendingMatcher{Success: []RouteMatch{RouteAnyMsgOfType{MsgType: "query_response"}}},
			RpcRequestLabels{Target: "SY.target@motherbee", RequestMsg: "DO_QUERY"}, 50*time.Millisecond)
		done <- err
	}()
	<-tx
	if err := <-done; err == nil {
		t.Fatalf("expected a timeout")
	}

	rx <- receivedMessage{msg: Message{
		Routing: Routing{TraceID: "unrelated-trace"},
		Meta:    Meta{MsgType: "query_response", Msg: stringPtr("ANY_RESPONSE_NAME")},
	}}
	select {
	case routed := <-sub:
		if routed.Meta.MsgType != "query_response" {
			t.Fatalf("unexpected broadcast message: %+v", routed)
		}
	case <-time.After(time.Second):
		t.Fatalf("the observational family stopped fanning out")
	}
	if got := dispatcher.UnknownResponseDrops(); got != 0 {
		t.Fatalf("expected no unknown-response drops, got %d", got)
	}
}

// A-75 — a catch-all (RouteAny) post-pending rule to a broadcast channel
// declares no reply shape observational: exact reply shapes are still
// registered, as in Rust (`post_pending_declares_observational_exact`).
func TestDispatcherCatchAllBroadcastDoesNotExemptReplyShapes(t *testing.T) {
	dispatcher, _, _ := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		BroadcastChannel("all").
		PostPendingRule(RouteAny{}, RouteBroadcast{Channel: "all"}))
	defer func() { _ = dispatcher.Close() }()

	if _, err := dispatcher.SendSystemRPC(context.Background(), SystemRpcRequest{
		Target:      "SY.target@motherbee",
		RequestMsg:  "REQ",
		ResponseMsg: "RESP",
		Payload:     map[string]any{},
		Timeout:     50 * time.Millisecond,
	}); err == nil {
		t.Fatalf("expected a timeout")
	}
	dispatcher.staleMu.Lock()
	_, registered := dispatcher.responseOnly[RouteExact{MsgType: SYSTEMKind, Msg: "RESP"}]
	dispatcher.staleMu.Unlock()
	if !registered {
		t.Fatalf("a catch-all broadcast rule must not exempt the exact reply shape")
	}
}

// A-75 — SendSystemRPC sends ttl 16 unless Options.TTL says otherwise, like
// SendAdminRPC and Rust `send_system_rpc`, so a request reaches a node on
// another router or hive instead of dying with TTL_EXCEEDED.
func TestSendSystemRPCDefaultsToRequestTTL(t *testing.T) {
	cases := []struct {
		name string
		ttl  uint8
		want uint8
	}{
		{name: "default", ttl: 0, want: 16},
		{name: "explicit", ttl: 3, want: 3},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dispatcher, tx, _ := newDispatcherTestHarness(t, NewOperationalRouteProfile())
			defer func() { _ = dispatcher.Close() }()
			done := make(chan struct{})
			go func() {
				defer close(done)
				_, _ = dispatcher.SendSystemRPC(context.Background(), SystemRpcRequest{
					Target:      "WF.demo@worker-1",
					RequestMsg:  "WF_LIST_INSTANCES",
					ResponseMsg: "WF_LIST_INSTANCES_RESPONSE",
					Payload:     map[string]any{},
					Timeout:     20 * time.Millisecond,
					Options:     SystemEnvelopeOptions{TTL: tc.ttl},
				})
			}()
			request := decodeTestFrame(t, <-tx)
			<-done
			if request.Routing.TTL != tc.want {
				t.Fatalf("ttl = %d, want %d", request.Routing.TTL, tc.want)
			}
		})
	}
}

// A-75 — an RPC whose request is over the frame limit fails at once with
// RpcErrMessageTooLarge and leaves no pending request and no registered
// reply shape. Mirror of Rust
// `send_failure_does_not_register_response_only_shape`.
func TestSendSystemRPCOversizeRequestLeavesNothingBehind(t *testing.T) {
	dispatcher, tx, _ := newDispatcherTestHarness(t, NewOperationalRouteProfile())
	defer func() { _ = dispatcher.Close() }()

	_, err := dispatcher.SendSystemRPC(context.Background(), SystemRpcRequest{
		Target:      "SY.target@motherbee",
		RequestMsg:  "REQ",
		ResponseMsg: "RESP",
		Payload:     map[string]any{"blob": strings.Repeat("x", maxFrameSize)},
		Timeout:     5 * time.Second,
	})
	var rpcErr *RpcError
	if !errors.As(err, &rpcErr) || rpcErr.Kind != RpcErrMessageTooLarge {
		t.Fatalf("expected RpcErrMessageTooLarge, got %v", err)
	}
	var tooLarge *MessageTooLargeError
	if !errors.As(err, &tooLarge) || tooLarge.Max != maxFrameSize {
		t.Fatalf("expected the *MessageTooLargeError cause, got %v", err)
	}
	if len(tx) != 0 {
		t.Fatalf("the oversize request was queued")
	}
	dispatcher.pendingMu.Lock()
	pending := len(dispatcher.pending)
	dispatcher.pendingMu.Unlock()
	dispatcher.staleMu.Lock()
	registered := len(dispatcher.responseOnly)
	dispatcher.staleMu.Unlock()
	if pending != 0 || registered != 0 {
		t.Fatalf("left behind: %d pending requests, %d reply shapes", pending, registered)
	}
}
