package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"

	"github.com/4iplatform/json-router/nodes/wf/wf-generic/node"
)

func main() {
	var definitionPath string
	flag.StringVar(&definitionPath, "definition", "", "path to workflow definition JSON")
	flag.Parse()

	if definitionPath == "" {
		fmt.Fprintln(os.Stderr, "missing required -definition flag")
		os.Exit(2)
	}

	runtime, err := node.Run(context.Background(), node.RunOptions{
		DefinitionPath: definitionPath,
	})
	if err != nil {
		log.Fatalf("wf-generic: %v", err)
	}

	log.Printf(
		"wf-generic loaded workflow_type=%s states=%d terminal_states=%d",
		runtime.Definition.WorkflowType,
		len(runtime.Definition.States),
		len(runtime.Definition.TerminalStates),
	)
}
