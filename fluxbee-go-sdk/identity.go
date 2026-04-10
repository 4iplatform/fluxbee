package sdk

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/google/uuid"
)

const (
	DefaultPeerResolveRetryWindow   = 2 * time.Second
	DefaultPeerResolveRetryInterval = 25 * time.Millisecond
)

type PeerIdentityResolver struct {
	UUIDPersistenceDir string
	HiveID             string
	RetryWindow        time.Duration
	RetryInterval      time.Duration
}

func (r PeerIdentityResolver) ResolvePeerL2Name(sourceUUID string) (string, error) {
	return ResolvePeerL2Name(sourceUUID, r)
}

func ResolvePeerL2Name(sourceUUID string, resolver PeerIdentityResolver) (string, error) {
	sourceUUID = strings.TrimSpace(sourceUUID)
	if sourceUUID == "" {
		return "", fmt.Errorf("source_uuid must be non-empty")
	}
	if resolver.UUIDPersistenceDir == "" {
		return "", fmt.Errorf("uuid_persistence_dir must be non-empty")
	}
	if resolver.HiveID == "" {
		return "", fmt.Errorf("hive_id must be non-empty")
	}
	if _, err := uuid.Parse(sourceUUID); err != nil {
		return "", fmt.Errorf("invalid source_uuid: %w", err)
	}

	retryWindow := resolver.RetryWindow
	if retryWindow <= 0 {
		retryWindow = DefaultPeerResolveRetryWindow
	}
	retryInterval := resolver.RetryInterval
	if retryInterval <= 0 {
		retryInterval = DefaultPeerResolveRetryInterval
	}

	deadline := time.Now().Add(retryWindow)
	for {
		name, err := resolvePeerL2NameOnce(sourceUUID, resolver.UUIDPersistenceDir, resolver.HiveID)
		if err == nil {
			return name, nil
		}
		if time.Now().After(deadline) {
			return "", err
		}
		time.Sleep(retryInterval)
	}
}

func resolvePeerL2NameOnce(sourceUUID, dir, hiveID string) (string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return "", err
	}
	names := make([]string, 0, len(entries))
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if strings.HasSuffix(name, ".uuid") {
			names = append(names, name)
		}
	}
	sort.Strings(names)
	for _, name := range names {
		data, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil {
			continue
		}
		if strings.TrimSpace(string(data)) != sourceUUID {
			continue
		}
		baseName := strings.TrimSuffix(name, ".uuid")
		if strings.Contains(baseName, "@") {
			return baseName, nil
		}
		return fmt.Sprintf("%s@%s", baseName, hiveID), nil
	}
	return "", fmt.Errorf("source uuid %s not found in %s", sourceUUID, dir)
}
