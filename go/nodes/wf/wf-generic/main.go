package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"strings"
	"syscall"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/4iplatform/json-router/nodes/wf/wf-generic/node"
)

const (
	version            = "1.0"
	configDir          = "/etc/fluxbee"
	routerSockDir      = "/var/run/fluxbee/routers"
	uuidPersistenceDir = "/var/lib/fluxbee/state/nodes"
)

func main() {
	var configPath string
	flag.StringVar(&configPath, "config", "", "path to config.json")
	flag.Parse()

	// The orchestrator launches a managed node with its full name in FLUXBEE_NODE_NAME; its
	// config.json follows from it. --config is for local dev.
	nodeName := strings.TrimSpace(os.Getenv(sdk.ManagedNodeNameEnv))
	if configPath == "" {
		derived, err := sdk.ManagedNodeConfigPath(nodeName, sdk.DefaultManagedNodesRoot)
		if err != nil {
			fmt.Fprintf(os.Stderr, "missing --config and no managed config path: %v\n", err)
			os.Exit(2)
		}
		configPath = derived
	}

	// A config the node cannot use is a state it reports (FAILED_CONFIG), not a reason to exit;
	// only a node it cannot even name, or a file it cannot read, stops it here.
	plane, err := node.BootstrapControlPlane(configPath, nodeName, nil)
	if err != nil {
		log.Fatalf("wf-generic: %v", err)
	}
	if plane.NodeName == "" {
		log.Fatalf("wf-generic: managed node name unavailable: set %s or _system.node_name", sdk.ManagedNodeNameEnv)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	if err := node.Run(ctx, node.RunOptions{
		ConfigPath: configPath,
		NodeName:   nodeName,
		Plane:      plane,
		SDKConfig: &sdk.NodeConfig{
			Name:               plane.NodeName,
			RouterSocket:       routerSockDir,
			UUIDPersistenceDir: uuidPersistenceDir,
			UUIDMode:           sdk.NodeUuidPersistent,
			ConfigDir:          configDir,
			Version:            version,
		},
	}); err != nil {
		log.Fatalf("wf-generic: %v", err)
	}
}
