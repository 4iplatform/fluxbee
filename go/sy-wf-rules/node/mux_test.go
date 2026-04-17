package node

import (
	"context"
	"testing"
	"time"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

type fakeMuxReceiver struct {
	ch chan fluxbeesdk.Message
}

func (f *fakeMuxReceiver) Recv(ctx context.Context) (fluxbeesdk.Message, error) {
	select {
	case <-ctx.Done():
		return fluxbeesdk.Message{}, ctx.Err()
	case msg := <-f.ch:
		return msg, nil
	}
}

func TestMessageMuxRoutesTraceResponsesWithoutDroppingUnrelatedMessages(t *testing.T) {
	rcv := &fakeMuxReceiver{ch: make(chan fluxbeesdk.Message, 2)}
	mux := newMessageMux(rcv)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go mux.Run(ctx)

	responseCh, unregister, err := mux.SubscribeTrace("rpc-trace")
	if err != nil {
		t.Fatalf("SubscribeTrace: %v", err)
	}
	defer unregister()

	unrelated := fluxbeesdk.Message{
		Routing: fluxbeesdk.Routing{TraceID: "other-trace"},
		Meta:    fluxbeesdk.Meta{MsgType: "query", Msg: stringPtr("get_status")},
	}
	response := fluxbeesdk.Message{
		Routing: fluxbeesdk.Routing{TraceID: "rpc-trace"},
		Meta:    fluxbeesdk.Meta{MsgType: fluxbeesdk.SYSTEMKind, Msg: stringPtr(fluxbeesdk.MSGNodeStatusGetResponse)},
	}

	rcv.ch <- unrelated
	rcv.ch <- response

	select {
	case item := <-responseCh:
		if item.err != nil {
			t.Fatalf("unexpected response error: %v", item.err)
		}
		if item.msg.Routing.TraceID != "rpc-trace" {
			t.Fatalf("unexpected routed trace %q", item.msg.Routing.TraceID)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for trace response")
	}

	gotMain, err := mux.NextMessage(context.Background())
	if err != nil {
		t.Fatalf("NextMessage: %v", err)
	}
	if gotMain.Routing.TraceID != "other-trace" {
		t.Fatalf("unexpected main trace %q", gotMain.Routing.TraceID)
	}
}
