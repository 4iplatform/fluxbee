package main

import (
	"log"

	"github.com/4iplatform/json-router/sy-wf-rules/node"
)

func main() {
	log.SetFlags(log.LstdFlags | log.Lmicroseconds)
	if err := node.Run(node.DefaultRuntimeConfig()); err != nil {
		log.Fatalf("sy-wf-rules: %v", err)
	}
}
