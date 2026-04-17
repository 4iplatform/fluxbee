package node

import (
	"context"
	"fmt"
	"sync"

	fluxbeesdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

type rpcReceiver interface {
	SubscribeTrace(traceID string) (<-chan muxResult, func(), error)
}

type muxResult struct {
	msg fluxbeesdk.Message
	err error
}

type messageMux struct {
	source receiver

	mainCh chan muxResult

	mu          sync.Mutex
	subscribers map[string]chan muxResult
}

func newMessageMux(source receiver) *messageMux {
	if source == nil {
		return nil
	}
	return &messageMux{
		source:      source,
		mainCh:      make(chan muxResult, 64),
		subscribers: make(map[string]chan muxResult),
	}
}

func (m *messageMux) Run(ctx context.Context) {
	if m == nil || m.source == nil {
		return
	}
	for {
		msg, err := m.source.Recv(ctx)
		if err != nil {
			m.broadcastError(err)
			return
		}
		if m.dispatchToTrace(msg) {
			continue
		}
		select {
		case m.mainCh <- muxResult{msg: msg}:
		case <-ctx.Done():
			m.broadcastError(ctx.Err())
			return
		}
	}
}

func (m *messageMux) NextMessage(ctx context.Context) (fluxbeesdk.Message, error) {
	if m == nil {
		return fluxbeesdk.Message{}, fmt.Errorf("message mux unavailable")
	}
	select {
	case <-ctx.Done():
		return fluxbeesdk.Message{}, ctx.Err()
	case item := <-m.mainCh:
		return item.msg, item.err
	}
}

func (m *messageMux) SubscribeTrace(traceID string) (<-chan muxResult, func(), error) {
	if m == nil {
		return nil, nil, fmt.Errorf("message mux unavailable")
	}
	if traceID == "" {
		return nil, nil, fmt.Errorf("traceID is required")
	}
	ch := make(chan muxResult, 1)
	m.mu.Lock()
	if _, exists := m.subscribers[traceID]; exists {
		m.mu.Unlock()
		return nil, nil, fmt.Errorf("traceID already registered")
	}
	m.subscribers[traceID] = ch
	m.mu.Unlock()
	return ch, func() { m.unregister(traceID, ch) }, nil
}

func (m *messageMux) dispatchToTrace(msg fluxbeesdk.Message) bool {
	traceID := msg.Routing.TraceID
	if traceID == "" {
		return false
	}
	m.mu.Lock()
	ch, ok := m.subscribers[traceID]
	if ok {
		delete(m.subscribers, traceID)
	}
	m.mu.Unlock()
	if !ok {
		return false
	}
	ch <- muxResult{msg: msg}
	close(ch)
	return true
}

func (m *messageMux) unregister(traceID string, ch chan muxResult) {
	m.mu.Lock()
	defer m.mu.Unlock()
	current, ok := m.subscribers[traceID]
	if !ok || current != ch {
		return
	}
	delete(m.subscribers, traceID)
	close(ch)
}

func (m *messageMux) broadcastError(err error) {
	m.mu.Lock()
	subscribers := m.subscribers
	m.subscribers = make(map[string]chan muxResult)
	m.mu.Unlock()

	for _, ch := range subscribers {
		ch <- muxResult{err: err}
		close(ch)
	}

	select {
	case m.mainCh <- muxResult{err: err}:
	default:
	}
}
