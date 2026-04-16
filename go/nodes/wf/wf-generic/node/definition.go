package node

import (
	"time"

	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

const (
	workflowSchemaVersion = wfcel.WorkflowSchemaVersion
	minTimerDuration      = wfcel.MinTimerDuration
)

type WorkflowDefinition = wfcel.WorkflowDefinition
type StateDefinition = wfcel.StateDefinition
type TransitionDefinition = wfcel.TransitionDefinition
type EventMatch = wfcel.EventMatch
type ActionMeta = wfcel.ActionMeta
type ActionDefinition = wfcel.ActionDefinition
type LoadError = wfcel.ValidationError

func LoadDefinitionFile(path string, clock ClockFunc) (*WorkflowDefinition, error) {
	return wfcel.LoadDefinitionFile(path, clock)
}

func LoadDefinitionBytes(data []byte, source string, clock ClockFunc) (*WorkflowDefinition, error) {
	return wfcel.LoadDefinitionBytes(data, source, clock)
}

func ValidateDefinition(def *WorkflowDefinition, clock ClockFunc) error {
	if errs := wfcel.ValidateDefinition(def, clock); len(errs) > 0 {
		return errs
	}
	return nil
}

func validateInputPayload(def *WorkflowDefinition, input map[string]any) error {
	return wfcel.ValidateInputPayload(def, input)
}

func parseWorkflowDuration(value string) (time.Duration, error) {
	return wfcel.ParseWorkflowDuration(value)
}

func isValidL2Name(value string) bool {
	return wfcel.IsValidL2Name(value)
}
