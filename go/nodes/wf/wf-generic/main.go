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

	// The engine it runs is the runtime version its binary is in, which its status reports
	// (FINDINGS A-302): the start script the orchestrator launches runs the binary beside it.
	executable, err := os.Executable()
	if err != nil {
		log.Printf("wf-generic: its binary's path could not be read, its status names no engine: %v", err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	// A store the node cannot use is a state it reports too (DEGRADED, or FAILED_CONFIG for a bad
	// db_path), and it opens the store again until it can (FINDINGS A-217). Run fails only for what
	// the node cannot run without at all: its router connection, its receive channels (its messages,
	// and SY.timer's answers to its timer requests, FINDINGS A-254), a timer client.
	if err := node.Run(ctx, node.RunOptions{
		ConfigPath: configPath,
		NodeName:   nodeName,
		Plane:      plane,
		Engine:     node.EngineOf(executable),
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
