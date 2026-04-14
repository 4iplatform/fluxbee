package node

import (
	"time"

	"github.com/google/cel-go/cel"
	"github.com/google/cel-go/common/types"
	"github.com/google/cel-go/common/types/ref"
)

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
