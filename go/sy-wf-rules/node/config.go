package node

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const (
	defaultNodeBaseName = "SY.wf-rules"
	defaultConfigDir    = "/etc/fluxbee"
	defaultRouterSock   = "/var/run/fluxbee/routers"
	defaultStateDir     = "/var/lib/fluxbee/wf-rules"
	defaultUUIDDir      = "/var/lib/fluxbee/state/nodes"
	defaultDistRoot     = "/var/lib/fluxbee/dist/runtimes"
)

type RuntimeConfig struct {
	NodeBaseName       string
	ConfigDir          string
	RouterSocketDir    string
	StateDir           string
	DistRuntimeRoot    string
	UUIDPersistenceDir string
}

type NodeConfig struct {
	HiveID             string
	NodeName           string
	OrchestratorTarget string
	StateDir           string
	DistRuntimeRoot    string
	TenantID           string
}

func DefaultRuntimeConfig() RuntimeConfig {
	return RuntimeConfig{
		NodeBaseName:       defaultNodeBaseName,
		ConfigDir:          defaultConfigDir,
		RouterSocketDir:    defaultRouterSock,
		StateDir:           defaultStateDir,
		DistRuntimeRoot:    defaultDistRoot,
		UUIDPersistenceDir: defaultUUIDDir,
	}
}

func BuildNodeConfig(fullNodeName string, stateDir string, distRuntimeRoot string) (NodeConfig, error) {
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
	if strings.TrimSpace(distRuntimeRoot) == "" {
		distRuntimeRoot = defaultDistRoot
	}
	tenantID := strings.TrimSpace(os.Getenv("WFRULES_TENANT_ID"))
	if tenantID == "" {
		tenantID = strings.TrimSpace(os.Getenv("ORCH_DEFAULT_TENANT_ID"))
	}
	return NodeConfig{
		HiveID:             strings.TrimSpace(hiveID),
		NodeName:           fullNodeName,
		OrchestratorTarget: fmt.Sprintf("SY.orchestrator@%s", strings.TrimSpace(hiveID)),
		StateDir:           filepath.Clean(stateDir),
		DistRuntimeRoot:    filepath.Clean(distRuntimeRoot),
		TenantID:           tenantID,
	}, nil
}
