package sdk

import (
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func tempDir(t *testing.T, prefix string) string {
	t.Helper()
	path := filepath.Join(os.TempDir(), prefix+"-"+time.Now().Format("20060102150405.000000000"))
	if err := os.MkdirAll(path, 0o755); err != nil {
		t.Fatalf("mkdir temp dir: %v", err)
	}
	return path
}

func TestResolveUUIDPersistentReusesFile(t *testing.T) {
	dir := tempDir(t, "fluxbee-go-sdk-uuid")
	first, err := resolveUUID(NodeUuidPersistent, dir, "SY.opa.rules")
	if err != nil {
		t.Fatalf("resolve first uuid: %v", err)
	}
	second, err := resolveUUID(NodeUuidPersistent, dir, "SY.opa.rules")
	if err != nil {
		t.Fatalf("resolve second uuid: %v", err)
	}
	if first != second {
		t.Fatalf("expected same uuid")
	}
}

func TestNormalizeNodeName(t *testing.T) {
	full, base := normalizeNodeName("SY.opa.rules", "motherbee")
	if full != "SY.opa.rules@motherbee" || base != "SY.opa.rules" {
		t.Fatalf("unexpected normalized name: full=%s base=%s", full, base)
	}
}

func TestRouterSocketCandidatesOrdersSocketsDeterministically(t *testing.T) {
	dir := tempDir(t, "fluxbee-go-sdk-sockets")
	for _, name := range []string{"b.sock", "a.sock", "irp-ignore.sock"} {
		path := filepath.Join(dir, name)
		if err := os.WriteFile(path, []byte(""), 0o644); err != nil {
			t.Fatalf("write socket placeholder %s: %v", name, err)
		}
	}

	got, err := RouterSocketCandidates(dir, "SY.opa.rules")
	if err != nil {
		t.Fatalf("router socket candidates: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("expected 2 router candidates, got %d", len(got))
	}
	if filepath.Base(got[0]) == "irp-ignore.sock" || filepath.Base(got[1]) == "irp-ignore.sock" {
		t.Fatalf("irp socket should have been filtered out: %v", got)
	}

	again, err := RouterSocketCandidates(dir, "SY.opa.rules")
	if err != nil {
		t.Fatalf("router socket candidates second pass: %v", err)
	}
	if got[0] != again[0] || got[1] != again[1] {
		t.Fatalf("candidate order should be deterministic: first=%v second=%v", got, again)
	}
}

func TestPerformHandshakeReturnsAnnounceEnvelopeAndPayload(t *testing.T) {
	serverConn, clientConn := net.Pipe()
	defer serverConn.Close()
	defer clientConn.Close()

	done := make(chan error, 1)
	go func() {
		defer close(done)
		frame, err := ReadFrame(serverConn)
		if err != nil {
			done <- err
			return
		}
		var hello Message
		if err := json.Unmarshal(frame, &hello); err != nil {
			done <- err
			return
		}
		if hello.Meta.MsgType != SYSTEMKind || stringValue(hello.Meta.Msg) != MSGHello {
			done <- fmt.Errorf("unexpected hello envelope: %+v", hello.Meta)
			return
		}
		announce, err := BuildAnnounce("router-1", "node-1", "trace-announce", NodeAnnouncePayload{
			UUID:       "router-uuid",
			Name:       "SY.opa.rules@motherbee",
			Status:     "ready",
			VpnID:      7,
			RouterName: "router-1",
		})
		if err != nil {
			done <- err
			return
		}
		payload, err := json.Marshal(announce)
		if err != nil {
			done <- err
			return
		}
		done <- WriteFrame(serverConn, payload)
	}()

	msg, announce, err := PerformHandshake(clientConn, "SY.opa.rules@motherbee", "node-1", "1.0")
	if err != nil {
		t.Fatalf("perform handshake: %v", err)
	}
	if msg.Routing.Src != "router-1" {
		t.Fatalf("expected router src, got %q", msg.Routing.Src)
	}
	if announce.RouterName != "router-1" || announce.VpnID != 7 {
		t.Fatalf("unexpected announce payload: %+v", announce)
	}
	if err := <-done; err != nil {
		t.Fatalf("server goroutine: %v", err)
	}
}

func TestConnectionStateTracksRouterName(t *testing.T) {
	var state connectionState
	state.SetRouterName("RT.edge@motherbee")
	if got := state.RouterName(); got != "RT.edge@motherbee" {
		t.Fatalf("unexpected router name: %q", got)
	}
	state.SetRouterName("  RT.gateway@motherbee  ")
	if got := state.RouterName(); got != "RT.gateway@motherbee" {
		t.Fatalf("unexpected trimmed router name: %q", got)
	}
}

func TestConnectionStateSetAnnounceUpdatesVisibleNodeMetadata(t *testing.T) {
	var state connectionState
	state.SetAnnounce(&NodeAnnouncePayload{
		Name:       "WF.invoice@motherbee",
		VpnID:      9,
		RouterName: "rt-gateway@motherbee",
	})

	sender := &NodeSender{fullName: "stale-name", state: &state}
	receiver := &NodeReceiver{fullName: "stale-name", vpnID: 1, state: &state}

	if got := sender.FullName(); got != "WF.invoice@motherbee" {
		t.Fatalf("sender FullName: got %q", got)
	}
	if got := receiver.FullName(); got != "WF.invoice@motherbee" {
		t.Fatalf("receiver FullName: got %q", got)
	}
	if got := receiver.VpnID(); got != 9 {
		t.Fatalf("receiver VpnID: got %d", got)
	}
	if got := sender.RouterName(); got != "rt-gateway@motherbee" {
		t.Fatalf("sender RouterName: got %q", got)
	}
	if !receiver.IsConnected() {
		t.Fatalf("receiver should report connected after announce")
	}
}

// A-75 — a message whose frame is over the limit is refused before it is
// queued, with *MessageTooLargeError, and the link stays up; a frame of
// exactly the limit is queued. Mirror of Rust `NodeSender::send`.
func TestNodeSenderRefusesFrameOverTheLimit(t *testing.T) {
	state := &connectionState{connected: true}
	tx := make(chan []byte, 2)
	sender := &NodeSender{uuid: "src-1", fullName: "WF.demo@motherbee", tx: tx, state: state}

	err := sender.Send(messageWithFrameSize(t, maxFrameSize+1))
	var tooLarge *MessageTooLargeError
	if !errors.As(err, &tooLarge) || tooLarge.Size != maxFrameSize+1 || tooLarge.Max != maxFrameSize {
		t.Fatalf("oversize send: got %v, want *MessageTooLargeError", err)
	}
	if len(tx) != 0 {
		t.Fatalf("the oversize frame was queued")
	}
	if !state.IsConnected() {
		t.Fatalf("an oversize send must not mark the link down")
	}

	if err := sender.Send(messageWithFrameSize(t, maxFrameSize)); err != nil {
		t.Fatalf("a frame of exactly the limit: %v", err)
	}
	if frame := <-tx; len(frame) != maxFrameSize {
		t.Fatalf("queued frame is %d bytes, want %d", len(frame), maxFrameSize)
	}
}

// A frame carries a raw payload byte for byte (wf-orchestration-v1 §5.2.1,
// item 10): json.Marshal wrote <, > and & as <, > and &, so a
// payload a WF forwards as it received it went out changed. Nothing follows
// the JSON, the encoder's newline included.
func TestAFrameCarriesARawPayloadByteForByte(t *testing.T) {
	payload := `{"type":"text","content":"<b>1 & 2</b> > 0","amount":1.10,"z":1,"a":2}`
	msg := Message{
		Routing: Routing{Src: "src-1", Dst: UnicastDestination("AI.sales@worker1"), TTL: 16, TraceID: "trace-raw"},
		Meta:    Meta{MsgType: "user", Context: json.RawMessage(`{"note":"a<b"}`)},
		Payload: json.RawMessage(payload),
	}
	state := &connectionState{connected: true}
	tx := make(chan []byte, 1)
	sender := &NodeSender{uuid: "src-1", fullName: "WF.demo@worker1", tx: tx, state: state}
	if err := sender.Send(msg); err != nil {
		t.Fatalf("send: %v", err)
	}
	frame := string(<-tx)
	if !strings.Contains(frame, `"payload":`+payload) || !strings.Contains(frame, `"context":{"note":"a<b"}`) {
		t.Fatalf("the frame changed the payload or the context: %s", frame)
	}
	if strings.HasSuffix(frame, "\n") {
		t.Fatalf("the frame ends with a newline: %q", frame)
	}
	var back Message
	if err := json.Unmarshal([]byte(frame), &back); err != nil || string(back.Payload) != payload {
		t.Fatalf("the frame reads back as it went: %s %v", back.Payload, err)
	}
}

// A-75 — over a real socket: the oversize send is refused and the next
// frame arrives on the same connection. Before, the writer dropped the
// connection on the oversize frame and the node reconnected 500 ms later.
func TestOversizeSendKeepsTheRouterConnection(t *testing.T) {
	// A short path: macOS limits a Unix socket path to 104 bytes.
	dir, err := os.MkdirTemp("", "fbsdk")
	if err != nil {
		t.Fatalf("mkdir temp dir: %v", err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	if err := os.WriteFile(filepath.Join(dir, "hive.yaml"), []byte("hive_id: motherbee\n"), 0o644); err != nil {
		t.Fatalf("write hive.yaml: %v", err)
	}
	socketPath := filepath.Join(dir, "rt.sock")
	frames := startFakeRouter(t, socketPath)

	profile, err := NewOperationalRouteProfile().Build()
	if err != nil {
		t.Fatalf("build profile: %v", err)
	}
	dispatcher, err := ConnectWithRetry(NodeConfig{
		Name:         "WF.demo",
		RouterSocket: socketPath,
		UUIDMode:     NodeUuidEphemeral,
		ConfigDir:    dir,
		Version:      "test",
	}, 0, profile)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer func() { _ = dispatcher.Close() }()
	sender := dispatcher.SenderSnapshot()
	send := func(name string, payload any) error {
		msg, err := BuildSystemUnicast(sender.UUID(), "SY.peer@motherbee", "trace-"+name, name, payload)
		if err != nil {
			t.Fatalf("build %s: %v", name, err)
		}
		return sender.Send(msg)
	}

	if err := send("BEFORE", map[string]any{}); err != nil {
		t.Fatalf("send BEFORE: %v", err)
	}
	before := nextRouterFrame(t, frames)
	var tooLarge *MessageTooLargeError
	if err := send("TOO_LARGE", map[string]any{"blob": strings.Repeat("x", maxFrameSize)}); !errors.As(err, &tooLarge) {
		t.Fatalf("oversize send: got %v, want *MessageTooLargeError", err)
	}
	if err := send("AFTER", map[string]any{}); err != nil {
		t.Fatalf("send AFTER: %v", err)
	}
	after := nextRouterFrame(t, frames)
	if before.msg != "BEFORE" || after.msg != "AFTER" || after.conn != before.conn {
		t.Fatalf("the router got %+v then %+v, want BEFORE then AFTER on one connection", before, after)
	}
}

// messageWithFrameSize builds a message whose JSON frame is exactly size bytes.
func messageWithFrameSize(t *testing.T, size int) Message {
	t.Helper()
	build := func(fill int) Message {
		payload, err := json.Marshal(map[string]string{"blob": strings.Repeat("x", fill)})
		if err != nil {
			t.Fatalf("marshal payload: %v", err)
		}
		return Message{
			Routing: Routing{Src: "src-1", Dst: UnicastDestination("SY.peer@motherbee"), TTL: 16, TraceID: "trace-big"},
			Meta:    Meta{MsgType: SYSTEMKind, Msg: stringPtr("BIG")},
			Payload: payload,
		}
	}
	empty, err := json.Marshal(build(0))
	if err != nil {
		t.Fatalf("marshal message: %v", err)
	}
	msg := build(size - len(empty))
	if frame, err := json.Marshal(msg); err != nil || len(frame) != size {
		t.Fatalf("frame is %d bytes (err %v), want %d", len(frame), err, size)
	}
	return msg
}

// routerFrame is a frame the fake router got after the handshake, with the
// number of the connection it came on.
type routerFrame struct {
	conn int
	msg  string
}

// startFakeRouter listens on socketPath like a router: it answers every
// HELLO with an ANNOUNCE and reports each later frame.
func startFakeRouter(t *testing.T, socketPath string) <-chan routerFrame {
	t.Helper()
	ln, err := net.Listen("unix", socketPath)
	if err != nil {
		t.Fatalf("listen on %s: %v", socketPath, err)
	}
	var connsMu sync.Mutex
	var conns []net.Conn
	t.Cleanup(func() {
		_ = ln.Close()
		connsMu.Lock()
		defer connsMu.Unlock()
		for _, conn := range conns {
			_ = conn.Close()
		}
	})
	frames := make(chan routerFrame, 64)
	go func() {
		for n := 1; ; n++ {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			connsMu.Lock()
			conns = append(conns, conn)
			connsMu.Unlock()
			go serveFakeRouterConn(n, conn, frames)
		}
	}()
	return frames
}

func serveFakeRouterConn(n int, conn net.Conn, frames chan<- routerFrame) {
	defer conn.Close()
	data, err := ReadFrame(conn)
	if err != nil {
		return
	}
	var hello Message
	if err := json.Unmarshal(data, &hello); err != nil {
		return
	}
	announce, err := BuildAnnounce("router-uuid", hello.Routing.Src, hello.Routing.TraceID, NodeAnnouncePayload{
		UUID:       hello.Routing.Src,
		Name:       "WF.demo@motherbee",
		Status:     "registered",
		RouterName: "RT.gateway@motherbee",
	})
	if err != nil {
		return
	}
	if data, err = json.Marshal(announce); err != nil || WriteFrame(conn, data) != nil {
		return
	}
	for {
		data, err := ReadFrame(conn)
		if err != nil {
			return
		}
		var msg Message
		if err := json.Unmarshal(data, &msg); err != nil {
			return
		}
		frames <- routerFrame{conn: n, msg: stringValue(msg.Meta.Msg)}
	}
}

func nextRouterFrame(t *testing.T, frames <-chan routerFrame) routerFrame {
	t.Helper()
	select {
	case frame := <-frames:
		return frame
	case <-time.After(5 * time.Second):
		t.Fatalf("the router got no frame")
		return routerFrame{}
	}
}
