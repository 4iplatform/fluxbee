package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"

	"github.com/4iplatform/json-router/nodes/wf/wf-generic/node"
)

func main() {
	var configPath string
	flag.StringVar(&configPath, "config", "", "path to config.json")
	flag.Parse()

	if configPath == "" {
		fmt.Fprintln(os.Stderr, "missing required --config flag")
		os.Exit(2)
	}

	cfg, err := node.LoadConfig(configPath)
	if err != nil {
		log.Fatalf("wf-generic: %v", err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	if err := node.Run(ctx, node.RunOptions{
		Config: cfg,
	}); err != nil {
		log.Fatalf("wf-generic: %v", err)
	}
}
