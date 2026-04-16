package wfcel

import (
	"context"
	"log"

	"github.com/google/cel-go/cel"
	"github.com/google/cel-go/common/types"
	"github.com/google/cel-go/common/types/ref"
)

func CompileGuard(expr string, clock ClockFunc) (cel.Program, error) {
	env, err := NewGuardEnv(clock)
	if err != nil {
		return nil, err
	}
	ast, issues := env.Compile(expr)
	if issues != nil && issues.Err() != nil {
		return nil, issues.Err()
	}
	return env.Program(ast)
}

func EvalGuard(program cel.Program, input, state, event map[string]any) bool {
	ctx, cancel := context.WithTimeout(context.Background(), GuardEvalTimeout)
	defer cancel()

	type result struct {
		val ref.Val
		err error
	}
	ch := make(chan result, 1)
	go func() {
		val, _, err := program.Eval(map[string]any{
			"input": input,
			"state": state,
			"event": event,
		})
		ch <- result{val, err}
	}()

	select {
	case <-ctx.Done():
		log.Printf("wfcel: guard eval timed out after %s", GuardEvalTimeout)
		return false
	case r := <-ch:
		if r.err != nil {
			log.Printf("wfcel: guard eval error: %v", r.err)
			return false
		}
		boolVal, ok := r.val.(types.Bool)
		if !ok {
			log.Printf("wfcel: guard eval returned non-bool type %T", r.val)
			return false
		}
		return bool(boolVal)
	}
}
