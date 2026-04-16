package node

import (
	"time"

	"github.com/google/cel-go/cel"

	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

const guardEvalTimeout = wfcel.GuardEvalTimeout

type ClockFunc = wfcel.ClockFunc

func defaultClock() time.Time {
	return wfcel.DefaultClock()
}

func EvalGuard(program cel.Program, input, state, event map[string]any) bool {
	return wfcel.EvalGuard(program, input, state, event)
}

func compileGuard(expr string, clock ClockFunc) (cel.Program, error) {
	return wfcel.CompileGuard(expr, clock)
}
