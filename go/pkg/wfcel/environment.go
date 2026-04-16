package wfcel

import (
	"time"

	"github.com/google/cel-go/cel"
	"github.com/google/cel-go/common/types"
	"github.com/google/cel-go/common/types/ref"
)

func DefaultClock() time.Time {
	return time.Now().UTC()
}

func NewGuardEnv(clock ClockFunc) (*cel.Env, error) {
	if clock == nil {
		clock = DefaultClock
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
