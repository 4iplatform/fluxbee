package sdk

import (
	"os"
	"path/filepath"
	"testing"
)

func TestResolvePeerL2NameMapsUUIDFileToL2Name(t *testing.T) {
	dir := tempDir(t, "fluxbee-go-sdk-peer-identity")
	wantUUID := "11111111-1111-1111-1111-111111111111"
	if err := os.WriteFile(filepath.Join(dir, "WF.demo.step.uuid"), []byte(wantUUID), 0o644); err != nil {
		t.Fatalf("write uuid file: %v", err)
	}

	got, err := ResolvePeerL2Name(wantUUID, PeerIdentityResolver{
		UUIDPersistenceDir: dir,
		HiveID:             "motherbee",
		RetryWindow:        1,
		RetryInterval:      1,
	})
	if err != nil {
		t.Fatalf("resolve peer l2 name: %v", err)
	}
	if got != "WF.demo.step@motherbee" {
		t.Fatalf("unexpected l2 name: %q", got)
	}
}

func TestResolvePeerL2NameReturnsExistingHiveSuffixAsIs(t *testing.T) {
	dir := tempDir(t, "fluxbee-go-sdk-peer-identity-l2")
	wantUUID := "22222222-2222-2222-2222-222222222222"
	if err := os.WriteFile(filepath.Join(dir, "SY.timer@worker-a.uuid"), []byte(wantUUID), 0o644); err != nil {
		t.Fatalf("write uuid file: %v", err)
	}

	got, err := ResolvePeerL2Name(wantUUID, PeerIdentityResolver{
		UUIDPersistenceDir: dir,
		HiveID:             "motherbee",
		RetryWindow:        1,
		RetryInterval:      1,
	})
	if err != nil {
		t.Fatalf("resolve peer l2 name: %v", err)
	}
	if got != "SY.timer@worker-a" {
		t.Fatalf("unexpected l2 name: %q", got)
	}
}
