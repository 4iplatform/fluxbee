package sdk

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/uuid"
)

func TestLookupPeerL2NameInRouterSnapshotFindsMatchingUUID(t *testing.T) {
	nodeUUID := mustUUID(t, "11111111-1111-1111-1111-111111111111")
	otherUUID := mustUUID(t, "22222222-2222-2222-2222-222222222222")
	data := buildRouterSnapshotFixture([]routerSnapshotFixtureNode{
		{UUID: otherUUID, Name: "AI.other@motherbee"},
		{UUID: nodeUUID, Name: "WF.demo@motherbee"},
	})

	got, err := resolvePeerL2NameFromRouterSnapshot(data, nodeUUID)
	if err != nil {
		t.Fatalf("resolve from snapshot: %v", err)
	}
	if got != "WF.demo@motherbee" {
		t.Fatalf("unexpected l2 name: %q", got)
	}
}

func TestLookupPeerL2NameInRouterSnapshotReturnsNotFound(t *testing.T) {
	data := buildRouterSnapshotFixture([]routerSnapshotFixtureNode{
		{UUID: mustUUID(t, "22222222-2222-2222-2222-222222222222"), Name: "AI.other@motherbee"},
	})

	_, err := resolvePeerL2NameFromRouterSnapshot(
		data,
		mustUUID(t, "11111111-1111-1111-1111-111111111111"),
	)
	if err == nil || !strings.Contains(err.Error(), "not found") {
		t.Fatalf("expected not found error, got %v", err)
	}
}

func TestResolveLocalRouterL2NameUsesGatewayNameFromHiveFile(t *testing.T) {
	dir := tempIdentityDir(t)
	writeFile(t, dir, "hive.yaml", "hive_id: motherbee\nwan:\n  gateway_name: RT.mesh\n")

	got, err := resolveLocalRouterL2Name(dir, "motherbee", "")
	if err != nil {
		t.Fatalf("resolve local router l2: %v", err)
	}
	if got != "RT.mesh@motherbee" {
		t.Fatalf("unexpected router l2 name: %q", got)
	}
}

func TestLoadRouterSHMNameReadsIdentityFile(t *testing.T) {
	dir := tempIdentityDir(t)
	path := writeFile(t, dir, "identity.yaml", "shm:\n  name: /jsr-deadbeef\n")

	got, err := loadRouterSHMName(path)
	if err != nil {
		t.Fatalf("load router shm name: %v", err)
	}
	if got != "/jsr-deadbeef" {
		t.Fatalf("unexpected shm name: %q", got)
	}
}

type routerSnapshotFixtureNode struct {
	UUID uuid.UUID
	Name string
}

func buildRouterSnapshotFixture(nodes []routerSnapshotFixtureNode) []byte {
	totalLen := routerNodeOffset + len(nodes)*routerNodeEntrySize
	data := make([]byte, totalLen)
	putU32(data, 0, routerSHMMagic)
	putU32(data, 4, routerSHMVersion)
	putU64(data, routerSeqOffset, 0)
	putU32(data, routerNodeCountOffset, uint32(len(nodes)))
	for i, node := range nodes {
		offset := routerNodeOffset + i*routerNodeEntrySize
		copy(data[offset+routerNodeUUIDOffset:], node.UUID[:])
		nameBytes := []byte(node.Name)
		copy(data[offset+routerNodeNameOffset:], nameBytes)
		putU16(data, offset+routerNodeNameLenOffset, uint16(len(nameBytes)))
	}
	return data
}

func putU16(data []byte, offset int, value uint16) {
	data[offset] = byte(value)
	data[offset+1] = byte(value >> 8)
}

func putU32(data []byte, offset int, value uint32) {
	data[offset] = byte(value)
	data[offset+1] = byte(value >> 8)
	data[offset+2] = byte(value >> 16)
	data[offset+3] = byte(value >> 24)
}

func putU64(data []byte, offset int, value uint64) {
	data[offset] = byte(value)
	data[offset+1] = byte(value >> 8)
	data[offset+2] = byte(value >> 16)
	data[offset+3] = byte(value >> 24)
	data[offset+4] = byte(value >> 32)
	data[offset+5] = byte(value >> 40)
	data[offset+6] = byte(value >> 48)
	data[offset+7] = byte(value >> 56)
}

func mustUUID(t *testing.T, raw string) uuid.UUID {
	t.Helper()
	value, err := uuid.Parse(raw)
	if err != nil {
		t.Fatalf("parse uuid %q: %v", raw, err)
	}
	return value
}

func writeFile(t *testing.T, dir, name, contents string) string {
	t.Helper()
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, []byte(contents), 0o644); err != nil {
		t.Fatalf("write %s: %v", name, err)
	}
	return path
}

func tempIdentityDir(t *testing.T) string {
	t.Helper()
	return t.TempDir()
}
