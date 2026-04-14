package node

import (
	"context"
	"errors"
)

type RunOptions struct {
	DefinitionPath string
	Clock          ClockFunc
}

type Runtime struct {
	Definition *WorkflowDefinition
}

func Run(_ context.Context, opts RunOptions) (*Runtime, error) {
	if opts.DefinitionPath == "" {
		return nil, errors.New("definition path is required")
	}
	def, err := LoadDefinitionFile(opts.DefinitionPath, opts.Clock)
	if err != nil {
		return nil, err
	}
	return &Runtime{Definition: def}, nil
}
