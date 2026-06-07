package sdk

import (
	"context"
	"encoding/json"
	"fmt"
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
