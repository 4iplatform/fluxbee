package sdk

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
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
