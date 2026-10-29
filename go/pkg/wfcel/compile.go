package wfcel

import (
	"context"
	"log"

	"github.com/google/cel-go/cel"
	"github.com/google/cel-go/common/types"
	"github.com/google/cel-go/common/types/ref"
)

// ValueCostLimit bounds a set_variable's evaluation, deterministically (cel-go's cost units, not
// time): past it the evaluation stops with an error, which fails the action under its on_error.
const ValueCostLimit = 1_000_000

// CompileGuard compiles a guard. Its program checks for an interrupt at every comprehension step, so
// EvalGuard stops it at GuardEvalTimeout.
func CompileGuard(expr string, clock ClockFunc) (cel.Program, error) {
	return compileProgram(expr, clock, cel.InterruptCheckFrequency(1))
}

// CompileValue compiles a set_variable value, with ValueCostLimit.
func CompileValue(expr string, clock ClockFunc) (cel.Program, error) {
	return compileProgram(expr, clock, cel.CostLimit(ValueCostLimit))
}

func compileProgram(expr string, clock ClockFunc, opts ...cel.ProgramOption) (cel.Program, error) {
	env, err := NewGuardEnv(clock)
	if err != nil {
		return nil, err
	}
	ast, issues := env.Compile(expr)
	if issues != nil && issues.Err() != nil {
		return nil, issues.Err()
	}
	return env.Program(ast, opts...)
}

// EvalGuard evaluates a guard on the caller's goroutine, stopped at GuardEvalTimeout. A guard that
// errs, runs out of time or panics is false. It runs on the caller's goroutine on purpose: one left
// running past its timeout kept reading the instance's state while the next transition wrote it, a
// fatal concurrent map access, not a panic (FINDINGS A-118).
func EvalGuard(program cel.Program, input, state, event map[string]any) (result bool) {
	ctx, cancel := context.WithTimeout(context.Background(), GuardEvalTimeout)
	defer cancel()
	defer func() {
		// A panic anywhere in the eval surface (json_parse, ext.Strings) is a false guard, never the
		// node's.
		if p := recover(); p != nil {
			log.Printf("wfcel: guard eval panicked: %v", p)
			result = false
		}
	}()
	val, _, err := program.ContextEval(ctx, map[string]any{
		"input": input,
		"state": state,
		"event": event,
	})
	if err != nil {
		log.Printf("wfcel: guard eval error: %v", err)
		return false
	}
	boolVal, ok := val.(types.Bool)
	if !ok {
		log.Printf("wfcel: guard eval returned non-bool type %T", val)
		return false
	}
	return bool(boolVal)
}

// EvalValue evaluates a set_variable value (CompileValue) on the caller's goroutine.
func EvalValue(program cel.Program, input, state, event map[string]any) (ref.Val, error) {
	val, _, err := program.Eval(map[string]any{
		"input": input,
		"state": state,
		"event": event,
	})
	return val, err
}
