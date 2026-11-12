package sdk

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

// setLaneBounds makes each lane of the queue of channel hold at most messages messages and bytes
// bytes.
func setLaneBounds(t *testing.T, dispatcher *RouterDispatcher, channel string, messages, bytes int) *commandQueue {
	t.Helper()
	dispatcher.commandsMu.Lock()
	queue := dispatcher.commands[channel]
	dispatcher.commandsMu.Unlock()
	if queue == nil {
		t.Fatalf("no command channel %q", channel)
	}
	queue.mu.Lock()
	queue.maxMessages, queue.maxBytes = messages, bytes
	queue.mu.Unlock()
	return queue
}

// userMessage is a user message from the node name (its uuid uuid), on trace and thread.
func userMessage(name, uuid, msg, trace, thread string) Message {
	out := Message{
		Routing: Routing{Src: uuid, SrcL2Name: stringPtr(name), Dst: UnicastDestination("WF.demo@motherbee"), TTL: 16, TraceID: trace},
		Meta:    Meta{MsgType: "user"},
		Payload: json.RawMessage(`{"type":"text","content":"hi"}`),
	}
	if msg != "" {
		out.Meta.Msg = stringPtr(msg)
	}
	if thread != "" {
		out.Meta.ThreadID = stringPtr(thread)
	}
	return out
}

// systemMessage is a system message msg from the node name (its uuid uuid), on trace.
func systemMessage(name, uuid, msg, trace string) Message {
	out := Message{
		Routing: Routing{Src: uuid, Dst: UnicastDestination("WF.demo@motherbee"), TTL: 16, TraceID: trace},
		Meta:    Meta{MsgType: SYSTEMKind},
		Payload: json.RawMessage(`{}`),
	}
	if name != "" {
		out.Routing.SrcL2Name = stringPtr(name)
	}
	if msg != "" {
		out.Meta.Msg = stringPtr(msg)
	}
	return out
}

func waitFor(t *testing.T, what string, done func() bool) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for !done() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(2 * time.Millisecond)
	}
}

func readMessage(t *testing.T, from <-chan Message) Message {
	t.Helper()
	select {
	case msg := <-from:
		return msg
	case <-time.After(2 * time.Second):
		t.Fatalf("the consumer got no message")
		return Message{}
	}
}

// FINDINGS A-329: a message that does not fit a command channel is answered NOT_TAKEN, reason
// node_busy, retryable, to its sender's uuid, on its trace and thread; the ones that fit are handed
// over in the order they came. Before, the channel (64) dropped it and told no one.
func TestAMessageThatDoesNotFitIsAnsweredNotTaken(t *testing.T) {
	dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "incoming"}))
	defer func() { _ = dispatcher.Close() }()
	setLaneBounds(t, dispatcher, "incoming", 4, commandLaneMaxBytes)
	incoming, err := dispatcher.TakeCommandReceiver("incoming")
	if err != nil {
		t.Fatalf("take incoming: %v", err)
	}

	for i := 0; i < 10; i++ {
		rx <- receivedMessage{msg: userMessage("IO.gate@worker1", "io-uuid", "", fmt.Sprintf("trace-%d", i), fmt.Sprintf("thread-%d", i))}
	}
	for i := 4; i < 10; i++ {
		answer := decodeTestFrame(t, <-tx)
		notice, ok := ParseNotTaken(answer)
		if !ok || answer.Routing.Dst.Value() != "io-uuid" || answer.Routing.Src != "src-1" ||
			answer.Routing.TraceID != fmt.Sprintf("trace-%d", i) || stringValue(answer.Meta.ThreadID) != fmt.Sprintf("thread-%d", i) ||
			answer.Routing.TTL != 16 {
			t.Fatalf("answer %d: want a NOT_TAKEN to io-uuid on trace-%d and thread-%d, got %+v %s", i, i, i, answer, answer.Payload)
		}
		if notice.OK || notice.Error != "not_taken" || notice.Reason != NotTakenReasonNodeBusy || !notice.Retryable ||
			!strings.Contains(notice.Detail, "WF.demo@motherbee holds 4 non-system messages") {
			t.Fatalf("answer %d: want node_busy, retryable, got %+v", i, notice)
		}
	}
	if drops := dispatcher.CommandChannelDrops()["incoming"]; drops != 6 {
		t.Fatalf("want 6 refusals counted, got %d", drops)
	}
	if sent := dispatcher.NotTakenSent(); sent != (NotTakenCounts{Answered: 6}) {
		t.Fatalf("want 6 NOT_TAKEN sent, got %+v", sent)
	}
	for i := 0; i < 4; i++ {
		if got := readMessage(t, incoming); got.Routing.TraceID != fmt.Sprintf("trace-%d", i) {
			t.Fatalf("message %d: got %s, want trace-%d", i, got.Routing.TraceID, i)
		}
	}
}

// FINDINGS A-329: no one answers a notice or a reply, a NOT_TAKEN included, so a refused one is
// counted and dropped: two nodes never bounce a NOT_TAKEN (FINDINGS A-112, A-119). A person's text,
// a user message without a name, is answered.
func TestANoticeOrAReplyThatDoesNotFitIsNeverAnswered(t *testing.T) {
	dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "incoming"}))
	defer func() { _ = dispatcher.Close() }()
	setLaneBounds(t, dispatcher, "incoming", 1, commandLaneMaxBytes)

	// Both lanes full.
	rx <- receivedMessage{msg: systemMessage("SY.orchestrator@worker1", "orch-uuid", MSGNodeStatusGet, "fill-system")}
	rx <- receivedMessage{msg: userMessage("IO.gate@worker1", "io-uuid", "", "fill-user", "thread-1")}

	unanswered := []Message{
		systemMessage("", "router-uuid", MSGUnreachable, "router-made"),
		systemMessage("WF.other@worker1", "wf-uuid", MSGWithdraw, "withdraw"),
		systemMessage("WF.other@worker1", "wf-uuid", "", "nameless-system"),
		systemMessage("SY.timer@worker1", "timer-uuid", MsgTimerResponse, "timer-answer"),
		systemMessage("WF.other@worker1", "wf-uuid", MSGNotTaken, "not-taken"),
		userMessage("AI.sales@worker1", "ai-uuid", "EVALUATE.error", "agent-error", "thread-1"),
		userMessage("WF.other@worker1", "wf-uuid", "ANSWER_RESPONSE", "user-reply", "thread-1"),
	}
	for _, msg := range unanswered {
		rx <- receivedMessage{msg: msg}
	}
	waitFor(t, "the refusals", func() bool { return dispatcher.NotTakenSent().Unanswered == uint64(len(unanswered)) })
	select {
	case frame := <-tx:
		t.Fatalf("a notice or a reply was answered: %s", frame)
	case <-time.After(50 * time.Millisecond):
	}

	rx <- receivedMessage{msg: userMessage("IO.gate@worker1", "io-uuid", "", "persons-text", "thread-1")}
	answer := decodeTestFrame(t, <-tx)
	if _, ok := ParseNotTaken(answer); !ok || answer.Routing.TraceID != "persons-text" {
		t.Fatalf("a person's text that does not fit gets a NOT_TAKEN, got %+v", answer)
	}
	want := NotTakenCounts{Answered: 1, Unanswered: uint64(len(unanswered))}
	if sent := dispatcher.NotTakenSent(); sent != want {
		t.Fatalf("want %+v, got %+v", want, sent)
	}
	if drops := dispatcher.CommandChannelDrops()["incoming"]; drops != uint64(len(unanswered))+1 {
		t.Fatalf("every refusal counts, got %d", drops)
	}
}

// FINDINGS A-329: system messages never wait behind user ones. A queue that holds both hands over
// its system messages first, then the rest, each lane in the order it came.
func TestTheQueueHandsOverItsSystemMessagesFirst(t *testing.T) {
	queue := newCommandQueue("incoming")
	order := []Message{
		userMessage("IO.gate@worker1", "io-uuid", "", "user-1", "t"),
		userMessage("IO.gate@worker1", "io-uuid", "", "user-2", "t"),
		systemMessage("SY.orchestrator@worker1", "orch-uuid", MSGNodeStatusGet, "system-1"),
		userMessage("IO.gate@worker1", "io-uuid", "", "user-3", "t"),
		systemMessage("SY.timer@worker1", "timer-uuid", MsgTimerFired, "system-2"),
	}
	for _, msg := range order {
		if !queue.offer(msg, approxMessageSize(msg)) {
			t.Fatalf("%s did not fit", msg.Routing.TraceID)
		}
	}
	done := make(chan struct{})
	defer close(done)
	go queue.run(done)
	for _, want := range []string{"system-1", "system-2", "user-1", "user-2", "user-3"} {
		if got := readMessage(t, queue.out).Routing.TraceID; got != want {
			t.Fatalf("got %s, want %s", got, want)
		}
	}
	if depths := queue.depths(); depths != [laneCount]int{} {
		t.Fatalf("the queue is empty once handed over, got %v", depths)
	}
}

// FINDINGS A-329: a system message that comes while the consumer works on a user message, with
// more user messages queued, is the next the consumer gets.
func TestASystemMessageThatComesDuringABacklogGoesNext(t *testing.T) {
	dispatcher, _, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("incoming").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "incoming"}))
	defer func() { _ = dispatcher.Close() }()
	incoming, err := dispatcher.TakeCommandReceiver("incoming")
	if err != nil {
		t.Fatalf("take incoming: %v", err)
	}
	for i := 1; i <= 5; i++ {
		rx <- receivedMessage{msg: userMessage("IO.gate@worker1", "io-uuid", "", fmt.Sprintf("user-%d", i), "t")}
	}
	if got := readMessage(t, incoming).Routing.TraceID; got != "user-1" {
		t.Fatalf("got %s, want user-1", got)
	}
	queue := dispatcher.commands["incoming"]
	waitFor(t, "the backlog", func() bool { return queue.depths()[laneOther] == 4 })
	rx <- receivedMessage{msg: systemMessage("SY.orchestrator@worker1", "orch-uuid", MSGNodeStatusGet, "status")}
	waitFor(t, "the status request queued", func() bool { return queue.depths()[laneSystem] == 1 })
	// The forwarder looks again once a message comes in: give it the time to.
	time.Sleep(50 * time.Millisecond)
	for _, want := range []string{"status", "user-2", "user-3", "user-4", "user-5"} {
		if got := readMessage(t, incoming).Routing.TraceID; got != want {
			t.Fatalf("got %s, want %s", got, want)
		}
	}
}

// FINDINGS A-329: each lane holds at most its messages and its bytes, and a full lane refuses only
// its own kind.
func TestALaneHoldsAtMostItsMessagesAndBytes(t *testing.T) {
	queue := newCommandQueue("incoming")
	queue.maxMessages, queue.maxBytes = 3, 1000
	user := userMessage("IO.gate@worker1", "io-uuid", "", "user", "t")
	system := systemMessage("SY.timer@worker1", "timer-uuid", MsgTimerFired, "fire")
	if !queue.offer(user, 400) || !queue.offer(user, 400) {
		t.Fatalf("two user messages of 400 bytes fit 1000")
	}
	if queue.offer(user, 400) {
		t.Fatalf("a third of 400 bytes is over 1000")
	}
	if !queue.offer(user, 100) {
		t.Fatalf("one of 100 bytes fits")
	}
	if queue.offer(user, 1) {
		t.Fatalf("a fourth message is over 3")
	}
	if !queue.offer(system, 2000) {
		t.Fatalf("the system lane has its own bounds, and takes one message larger than its bytes when empty")
	}
	if queue.offer(system, 10) {
		t.Fatalf("the system lane is full by its bytes")
	}
	if depths := queue.depths(); depths != [laneCount]int{1, 3} {
		t.Fatalf("want 1 system and 3 user messages, got %v", depths)
	}
}

// syncBuffer is a log destination the test reads while the dispatcher writes.
type syncBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *syncBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *syncBuffer) lines() []string {
	b.mu.Lock()
	defer b.mu.Unlock()
	var out []string
	for _, line := range strings.Split(strings.TrimSpace(b.buf.String()), "\n") {
		if strings.Contains(line, `fluxbee-go-sdk: command channel "logged"`) {
			out = append(out, line)
		}
	}
	return out
}

// FINDINGS A-329: the node's log says what its queue refused: the first refusal at once, then a
// line at most every refusalLogEvery, which counts what came since; a burst's last refusals are
// written once the interval passes, not lost.
func TestTheLogSaysWhatTheQueueRefusedWithoutFlooding(t *testing.T) {
	logged := &syncBuffer{}
	log.SetOutput(logged)
	defer log.SetOutput(os.Stderr)
	every := refusalLogEvery
	refusalLogEvery = 100 * time.Millisecond
	defer func() { refusalLogEvery = every }()

	// A channel of its own: a line due from another test's dispatcher names "incoming".
	dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile().
		CommandChannel("logged").
		PostPendingRule(RouteAny{}, RouteCommand{Channel: "logged"}))
	defer func() { _ = dispatcher.Close() }()
	setLaneBounds(t, dispatcher, "logged", 1, commandLaneMaxBytes)
	go func() {
		for range tx {
		}
	}()

	rx <- receivedMessage{msg: userMessage("IO.gate@worker1", "io-uuid", "", "fits", "t")}
	rx <- receivedMessage{msg: userMessage("IO.gate@worker1", "io-uuid", "", "first-refused", "t")}
	waitFor(t, "the first line", func() bool { return len(logged.lines()) == 1 })
	if line := logged.lines()[0]; !strings.Contains(line, `command channel "logged" full (0 system and 1 non-system messages waiting): 1 message(s) not taken`) ||
		!strings.Contains(line, "1 answered NOT_TAKEN node_busy") || !strings.Contains(line, "trace_id=first-refused") {
		t.Fatalf("unexpected first line: %s", line)
	}
	for i := 0; i < 5; i++ {
		rx <- receivedMessage{msg: userMessage("IO.gate@worker1", "io-uuid", "", fmt.Sprintf("burst-%d", i), "t")}
	}
	rx <- receivedMessage{msg: systemMessage("SY.orchestrator@worker1", "orch-uuid", MSGNodeStatusGet, "fits-too")}
	rx <- receivedMessage{msg: systemMessage("", "router-uuid", MSGUnreachable, "notice-refused")}
	waitFor(t, "the refusals", func() bool { return dispatcher.NotTakenSent().Answered+dispatcher.NotTakenSent().Unanswered == 7 })
	if lines := logged.lines(); len(lines) != 1 {
		t.Fatalf("one line per interval, got %d: %v", len(lines), lines)
	}
	waitFor(t, "the burst's line", func() bool { return len(logged.lines()) == 2 })
	if line := logged.lines()[1]; !strings.Contains(line, "6 message(s) not taken") ||
		!strings.Contains(line, "5 answered NOT_TAKEN node_busy, 1 not answered (a notice or a reply)") ||
		!strings.Contains(line, "trace_id=notice-refused") {
		t.Fatalf("unexpected burst line: %s", line)
	}
	time.Sleep(3 * refusalLogEvery)
	if lines := logged.lines(); len(lines) != 2 {
		t.Fatalf("nothing more to say, got %d lines: %v", len(lines), lines)
	}
}

// FINDINGS A-329: a request its target answers NOT_TAKEN fails at once with RpcErrNotTaken, its
// reason and whether it is retryable: a system request, an admin request and the timer client's.
// Before, a system request failed as an invalid response, and an admin request waited for its
// timeout.
func TestARequestAnsweredNotTakenFailsWithItsReason(t *testing.T) {
	cases := []struct {
		name string
		send func(*RouterDispatcher) error
	}{
		{"system", func(d *RouterDispatcher) error {
			_, err := d.SendSystemRPC(context.Background(), SystemRpcRequest{
				Target: "WF.busy@worker1", RequestMsg: "WF_LIST_INSTANCES", ResponseMsg: "WF_LIST_INSTANCES_RESPONSE",
				Payload: map[string]any{}, Timeout: 2 * time.Second,
			})
			return err
		}},
		{"admin", func(d *RouterDispatcher) error {
			_, err := d.SendAdminRPC(context.Background(), AdminRpcRequest{
				AdminTarget: "SY.admin@motherbee", Action: "list_hives", Timeout: 2 * time.Second,
			})
			return err
		}},
		{"timer", func(d *RouterDispatcher) error {
			client, err := NewTimerClient(d, TimerClientConfig{})
			if err != nil {
				return err
			}
			ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
			defer cancel()
			_, err = client.sendTimerRPC(ctx, Message{
				Routing: Routing{Dst: UnicastDestination("SY.timer@motherbee"), TTL: 16},
				Meta:    Meta{MsgType: SYSTEMKind, Msg: stringPtr("TIMER_GET")},
				Payload: json.RawMessage(`{}`),
			})
			return err
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dispatcher, tx, rx := newDispatcherTestHarness(t, NewOperationalRouteProfile())
			defer func() { _ = dispatcher.Close() }()
			done := make(chan error, 1)
			go func() { done <- tc.send(dispatcher) }()
			request := decodeTestFrame(t, <-tx)
			request.Routing.Src = "caller-uuid"
			request.Routing.SrcL2Name = stringPtr("SY.wf-rules@worker1")
			answer, err := BuildNotTaken("target-uuid", request, NotTakenReasonNodeBusy, "WF.busy@worker1 holds 1024 system messages", true)
			if err != nil {
				t.Fatalf("build NOT_TAKEN: %v", err)
			}
			answer.Routing.SrcL2Name = stringPtr("WF.busy@worker1")
			rx <- receivedMessage{msg: answer}
			var rpcErr *RpcError
			select {
			case err := <-done:
				if !errors.As(err, &rpcErr) || rpcErr.Kind != RpcErrNotTaken || rpcErr.Notice != MSGNotTaken ||
					rpcErr.Reason != NotTakenReasonNodeBusy || !rpcErr.Retryable || rpcErr.TraceID != request.Routing.TraceID {
					t.Fatalf("want RpcErrNotTaken node_busy retryable, got %#v", err)
				}
				if !strings.Contains(err.Error(), "rpc not taken") || !strings.Contains(err.Error(), "NOT_TAKEN reason=node_busy retryable=true: WF.busy@worker1 holds 1024 system messages") {
					t.Fatalf("unexpected text: %v", err)
				}
			case <-time.After(time.Second):
				t.Fatalf("the request did not end at its NOT_TAKEN")
			}
		})
	}
}

// FINDINGS A-329: the connection's reader waits for room in the receiver's queue instead of dropping
// a frame (and failing every pending request on the error it pushed): the frames wait in the
// router's queue for the link meanwhile.
func TestTheReaderWaitsForRoomInsteadOfDroppingAFrame(t *testing.T) {
	serverConn, clientConn := net.Pipe()
	defer serverConn.Close()
	dir := t.TempDir()
	tx := make(chan []byte)
	rx := make(chan receivedMessage, 2)
	state := &connectionState{connected: true}
	cfg := NodeConfig{Name: "WF.demo", RouterSocket: filepath.Join(dir, "missing.sock"), ConfigDir: dir}
	go connectionManager(cfg, "WF.demo", "WF.demo@motherbee", "node-uuid", clientConn, tx, rx, state)

	const frames = 10
	sizes := make(chan int, frames)
	go func() {
		for i := 0; i < frames; i++ {
			msg := userMessage("IO.gate@worker1", "io-uuid", "", fmt.Sprintf("trace-%d", i), "t")
			data, err := MarshalMessage(msg)
			if err != nil || WriteFrame(serverConn, data) != nil {
				return
			}
			sizes <- len(data)
		}
	}()
	// Nothing reads the receiver's queue meanwhile: a reader that dropped frames would have dropped
	// all but the first two by now.
	time.Sleep(100 * time.Millisecond)
	for i := 0; i < frames; i++ {
		select {
		case item := <-rx:
			if item.err != nil || item.msg.Routing.TraceID != fmt.Sprintf("trace-%d", i) || item.size != <-sizes {
				t.Fatalf("frame %d: got %+v", i, item)
			}
		case <-time.After(2 * time.Second):
			t.Fatalf("frame %d never came", i)
		}
	}
}
