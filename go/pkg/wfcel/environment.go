package wfcel

import (
	"encoding/json"
	"time"

	"github.com/google/cel-go/cel"
	"github.com/google/cel-go/common/types"
	"github.com/google/cel-go/common/types/ref"
	"github.com/google/cel-go/ext"
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
		// gap-4: base cel-go already has contains/startsWith/endsWith/matches(regex)/size;
		// ext.Strings() adds split/substring/join/replace/etc. so a guard can extract fields from AI
		// free text without a sentinel-token workaround. The 10ms GuardEvalTimeout (see types.go)
		// stays the DoS bound on the added surface.
		ext.Strings(),
		// json_parse(str) -> dyn: decode a JSON string (e.g. an AI structured-output reply) into a
		// CEL value so guards can branch on real fields. Invalid JSON yields a CEL error, which makes
		// the guard evaluate to a non-true result rather than crashing the engine.
		cel.Function(
			"json_parse",
			cel.Overload(
				"wf_json_parse_string",
				[]*cel.Type{cel.StringType},
				cel.DynType,
				cel.UnaryBinding(func(arg ref.Val) ref.Val {
					raw, ok := arg.Value().(string)
					if !ok {
						return types.NewErr("json_parse expects a string argument")
					}
					var parsed interface{}
					if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
						return types.NewErr("json_parse: invalid JSON: %v", err)
					}
					return types.DefaultTypeAdapter.NativeToValue(parsed)
				}),
			),
		),
	)
}
