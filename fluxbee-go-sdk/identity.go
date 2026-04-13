package sdk

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/google/uuid"
	"gopkg.in/yaml.v3"
)

const (
	DefaultPeerResolveRetryWindow   = 2 * time.Second
	DefaultPeerResolveRetryInterval = 25 * time.Millisecond
	defaultConfigDir                = "/etc/fluxbee"
	defaultStateDir                 = "/var/lib/fluxbee/state"
	defaultRouterBaseName           = "RT.gateway"
	routerSHMMagic                  = 0x4A535352
	routerSHMVersion                = 2
	routerSeqOffset                 = 48
	routerNodeCountOffset           = 56
	routerHeaderSize                = 248
	routerNodeOffset                = 256
	routerNodeEntrySize             = 328
	routerNodeUUIDOffset            = 0
	routerNodeNameOffset            = 16
	routerNodeNameLenOffset         = 272
	routerNodeNameMaxLen            = 256
	routerMaxNodes                  = 1024
	routerSeqlockReadTimeout        = 5 * time.Millisecond
)

type PeerIdentityResolver struct {
	ConfigDir     string
	StateDir      string
	HiveID        string
	RouterName    string
	RetryWindow   time.Duration
	RetryInterval time.Duration
}

type resolverHiveFile struct {
	HiveID string `yaml:"hive_id"`
	Wan    *struct {
		GatewayName string `yaml:"gateway_name"`
	} `yaml:"wan"`
}

type routerIdentityFile struct {
	Shm struct {
		Name string `yaml:"name"`
	} `yaml:"shm"`
}

func (r PeerIdentityResolver) ResolvePeerL2Name(sourceUUID string) (string, error) {
	return ResolvePeerL2Name(sourceUUID, r)
}

func ResolvePeerL2Name(sourceUUID string, resolver PeerIdentityResolver) (string, error) {
	sourceUUID = strings.TrimSpace(sourceUUID)
	if sourceUUID == "" {
		return "", fmt.Errorf("source_uuid must be non-empty")
	}
	parsedUUID, err := uuid.Parse(sourceUUID)
	if err != nil {
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
		name, err := resolvePeerL2NameOnce(parsedUUID, resolver)
		if err == nil {
			return name, nil
		}
		if time.Now().After(deadline) {
			return "", err
		}
		time.Sleep(retryInterval)
	}
}

func resolvePeerL2NameOnce(sourceUUID uuid.UUID, resolver PeerIdentityResolver) (string, error) {
	configDir := strings.TrimSpace(resolver.ConfigDir)
	if configDir == "" {
		configDir = defaultConfigDir
	}
	stateDir := strings.TrimSpace(resolver.StateDir)
	if stateDir == "" {
		stateDir = defaultStateDir
	}
	hiveID := strings.TrimSpace(resolver.HiveID)
	if hiveID == "" {
		loadedHiveID, err := loadHiveID(configDir)
		if err != nil {
			return "", err
		}
		hiveID = loadedHiveID
	}

	routerL2Name, err := resolveLocalRouterL2Name(configDir, hiveID, resolver.RouterName)
	if err != nil {
		return "", err
	}
	identityPath := filepath.Join(stateDir, routerL2Name, "identity.yaml")
	shmName, err := loadRouterSHMName(identityPath)
	if err != nil {
		return "", err
	}
	data, closeFn, err := openRouterSHMReadOnly(shmName)
	if err != nil {
		return "", err
	}
	defer closeFn()

	return resolvePeerL2NameFromRouterSnapshot(data, sourceUUID)
}

func resolveLocalRouterL2Name(configDir, hiveID, routerNameOverride string) (string, error) {
	routerName := strings.TrimSpace(routerNameOverride)
	if routerName == "" {
		routerName = strings.TrimSpace(os.Getenv("JSR_ROUTER_NAME"))
	}
	if routerName == "" {
		hiveFile, err := loadResolverHiveFile(configDir)
		if err != nil {
			return "", err
		}
		if hiveID == "" {
			hiveID = strings.TrimSpace(hiveFile.HiveID)
		}
		routerName = defaultRouterBaseName
		if hiveFile.Wan != nil && strings.TrimSpace(hiveFile.Wan.GatewayName) != "" {
			routerName = strings.TrimSpace(hiveFile.Wan.GatewayName)
		}
	}
	if hiveID == "" {
		return "", fmt.Errorf("hive_id must be non-empty")
	}
	if strings.Contains(routerName, "@") {
		return routerName, nil
	}
	return fmt.Sprintf("%s@%s", routerName, hiveID), nil
}

func loadResolverHiveFile(configDir string) (*resolverHiveFile, error) {
	data, err := os.ReadFile(filepath.Join(configDir, "hive.yaml"))
	if err != nil {
		return nil, err
	}
	var hive resolverHiveFile
	if err := yaml.Unmarshal(data, &hive); err != nil {
		return nil, err
	}
	return &hive, nil
}

func loadRouterSHMName(identityPath string) (string, error) {
	data, err := os.ReadFile(identityPath)
	if err != nil {
		return "", err
	}
	var identity routerIdentityFile
	if err := yaml.Unmarshal(data, &identity); err != nil {
		return "", err
	}
	name := strings.TrimSpace(identity.Shm.Name)
	if name == "" {
		return "", fmt.Errorf("router identity missing shm.name")
	}
	return name, nil
}

func resolvePeerL2NameFromRouterSnapshot(data []byte, sourceUUID uuid.UUID) (string, error) {
	if len(data) < routerNodeOffset {
		return "", fmt.Errorf("router shm too small")
	}
	if u32At(data, 0) != routerSHMMagic || u32At(data, 4) != routerSHMVersion {
		return "", fmt.Errorf("router shm header mismatch")
	}

	deadline := time.Now().Add(routerSeqlockReadTimeout)
	for {
		seq1 := u64At(data, routerSeqOffset)
		if seq1&1 != 0 {
			if time.Now().After(deadline) {
				return "", fmt.Errorf("router shm seqlock timeout")
			}
			time.Sleep(50 * time.Microsecond)
			continue
		}

		nodeCount := int(u32At(data, routerNodeCountOffset))
		if nodeCount < 0 || nodeCount > routerMaxNodes {
			return "", fmt.Errorf("invalid router node_count %d", nodeCount)
		}
		limit := routerNodeOffset + nodeCount*routerNodeEntrySize
		if len(data) < limit {
			return "", fmt.Errorf("router shm truncated for %d nodes", nodeCount)
		}
		snapshot := make([]byte, limit-routerNodeOffset)
		copy(snapshot, data[routerNodeOffset:limit])

		seq2 := u64At(data, routerSeqOffset)
		if seq1 != seq2 {
			if time.Now().After(deadline) {
				return "", fmt.Errorf("router shm seqlock timeout")
			}
			continue
		}

		return lookupPeerL2NameInSnapshot(snapshot, sourceUUID)
	}
}

func lookupPeerL2NameInSnapshot(snapshot []byte, sourceUUID uuid.UUID) (string, error) {
	uuidBytes := sourceUUID
	for offset := 0; offset+routerNodeEntrySize <= len(snapshot); offset += routerNodeEntrySize {
		entryUUID := snapshot[offset+routerNodeUUIDOffset : offset+routerNodeUUIDOffset+16]
		if !bytes.Equal(entryUUID, uuidBytes[:]) {
			continue
		}
		nameLen := int(u16At(snapshot, offset+routerNodeNameLenOffset))
		if nameLen <= 0 || nameLen > routerNodeNameMaxLen {
			return "", fmt.Errorf("invalid node name length in router shm")
		}
		nameBytes := snapshot[offset+routerNodeNameOffset : offset+routerNodeNameOffset+nameLen]
		name := strings.TrimSpace(string(nameBytes))
		if name == "" {
			return "", fmt.Errorf("empty node name in router shm")
		}
		return name, nil
	}
	return "", fmt.Errorf("source uuid %s not found in local router shm", sourceUUID.String())
}

func u16At(data []byte, offset int) uint16 {
	return uint16(data[offset]) | uint16(data[offset+1])<<8
}

func u32At(data []byte, offset int) uint32 {
	return uint32(data[offset]) |
		uint32(data[offset+1])<<8 |
		uint32(data[offset+2])<<16 |
		uint32(data[offset+3])<<24
}

func u64At(data []byte, offset int) uint64 {
	return uint64(data[offset]) |
		uint64(data[offset+1])<<8 |
		uint64(data[offset+2])<<16 |
		uint64(data[offset+3])<<24 |
		uint64(data[offset+4])<<32 |
		uint64(data[offset+5])<<40 |
		uint64(data[offset+6])<<48 |
		uint64(data[offset+7])<<56
}
