package node

import (
	"fmt"
	"path/filepath"
	"strings"
)

const (
	defaultNodeBaseName = "SY.wf-rules"
	defaultConfigDir    = "/etc/fluxbee"
	defaultRouterSock   = "/var/run/fluxbee/routers"
	defaultStateDir     = "/var/lib/fluxbee/wf-rules"
	defaultUUIDDir      = "/var/lib/fluxbee/state/nodes"
)

type RuntimeConfig struct {
	NodeBaseName       string
	ConfigDir          string
	RouterSocketDir    string
	StateDir           string
	UUIDPersistenceDir string
}

type NodeConfig struct {
	HiveID             string
	NodeName           string
	OrchestratorTarget string
	StateDir           string
}

func DefaultRuntimeConfig() RuntimeConfig {
	return RuntimeConfig{
		NodeBaseName:       defaultNodeBaseName,
		ConfigDir:          defaultConfigDir,
		RouterSocketDir:    defaultRouterSock,
		StateDir:           defaultStateDir,
		UUIDPersistenceDir: defaultUUIDDir,
	}
}

func BuildNodeConfig(fullNodeName string, stateDir string) (NodeConfig, error) {
	fullNodeName = strings.TrimSpace(fullNodeName)
	if fullNodeName == "" {
		return NodeConfig{}, fmt.Errorf("node name is required")
	}
	_, hiveID, ok := strings.Cut(fullNodeName, "@")
	if !ok || strings.TrimSpace(hiveID) == "" {
		return NodeConfig{}, fmt.Errorf("invalid node name %q", fullNodeName)
	}
	if strings.TrimSpace(stateDir) == "" {
		stateDir = defaultStateDir
	}
	return NodeConfig{
		HiveID:             strings.TrimSpace(hiveID),
		NodeName:           fullNodeName,
		OrchestratorTarget: fmt.Sprintf("SY.orchestrator@%s", strings.TrimSpace(hiveID)),
		StateDir:           filepath.Clean(stateDir),
	}, nil
}
