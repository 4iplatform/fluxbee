package sdk

import (
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
