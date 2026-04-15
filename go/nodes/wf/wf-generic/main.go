package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/4iplatform/json-router/nodes/wf/wf-generic/node"
)

const (
	version             = "1.0"
	configDir           = "/etc/fluxbee"
	routerSockDir       = "/var/run/fluxbee/routers"
	uuidPersistenceDir  = "/var/lib/fluxbee/state/nodes"
)

func main() {
	var configPath string
	flag.StringVar(&configPath, "config", "", "path to config.json")
	flag.Parse()

	if configPath == "" {
		derived, err := node.ManagedConfigPathFromEnv()
		if err != nil {
			fmt.Fprintf(os.Stderr, "missing required --config flag and could not derive managed config path: %v\n", err)
			os.Exit(2)
		}
		configPath = derived
	}

	cfg, err := node.LoadConfig(configPath)
	if err != nil {
		log.Fatalf("wf-generic: %v", err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	nodeName, err := node.ResolveManagedNodeName(cfg)
	if err != nil {
		log.Fatalf("wf-generic: %v", err)
	}

	if err := node.Run(ctx, node.RunOptions{
		ConfigPath: configPath,
		Config: cfg,
		SDKConfig: &sdk.NodeConfig{
			Name:               nodeName,
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
