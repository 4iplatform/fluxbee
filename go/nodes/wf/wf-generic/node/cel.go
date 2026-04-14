package node

import (
	"context"
	"log"
	"time"

	"github.com/google/cel-go/cel"
	"github.com/google/cel-go/common/types"
	"github.com/google/cel-go/common/types/ref"
)

const guardEvalTimeout = 10 * time.Millisecond

type ClockFunc func() time.Time

func defaultClock() time.Time {
	return time.Now().UTC()
}

func newGuardEnv(clock ClockFunc) (*cel.Env, error) {
	if clock == nil {
		clock = defaultClock
	}
	return cel.NewEnv(
		cel.Variable("input", cel.MapType(cel.StringType, cel.DynType)),
		cel.Variable("state", cel.MapType(cel.StringType, cel.DynType)),
		cel.Variable("event", cel.MapType(cel.StringType, cel.DynType)),
		cel.Function(
			"now",
			cel.Overload(
				"wf_now_utc_ms",
				[]*cel.Type{},
				cel.IntType,
				cel.FunctionBinding(func(args ...ref.Val) ref.Val {
					return types.Int(clock().UnixMilli())
				}),
			),
		),
	)
}

// EvalGuard evaluates a compiled guard program with a 10ms timeout.
// Returns false (never true) on timeout or evaluation error.
func EvalGuard(program cel.Program, input, state, event map[string]any) bool {
	ctx, cancel := context.WithTimeout(context.Background(), guardEvalTimeout)
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
		log.Printf("wf: guard eval timed out after %s", guardEvalTimeout)
		return false
	case r := <-ch:
		if r.err != nil {
			log.Printf("wf: guard eval error: %v", r.err)
			return false
		}
		boolVal, ok := r.val.(types.Bool)
		if !ok {
			log.Printf("wf: guard eval returned non-bool type %T", r.val)
			return false
		}
		if !bool(boolVal) {
			return false
		}
		return true
	}
}


func compileGuard(expr string, clock ClockFunc) (cel.Program, error) {
	env, err := newGuardEnv(clock)
	if err != nil {
		return nil, err
	}
	ast, issues := env.Compile(expr)
	if issues != nil && issues.Err() != nil {
		return nil, issues.Err()
	}
	return env.Program(ast)
}
