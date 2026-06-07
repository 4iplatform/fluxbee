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
		sender:     sender,
		receiver:   receiver,
		profile:    profile,
		pending:    make(map[string]*pendingEntry),
		commands:   make(map[string]chan Message),
		taken:      make(map[string]bool),
		broadcasts: make(map[string][]chan Message),
		done:       make(chan struct{}),
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
