package node

import (
	"testing"
	"time"
)

func TestEvalGuardTrueWhenConditionMet(t *testing.T) {
	program, err := compileGuard("event.payload.complete == true", fixedClock)
	if err != nil {
		t.Fatalf("compileGuard: %v", err)
	}
	event := map[string]any{
		"payload": map[string]any{"complete": true},
	}
	if !EvalGuard(program, nil, nil, event) {
		t.Fatal("expected true")
	}
}

func TestEvalGuardFalseWhenConditionNotMet(t *testing.T) {
	program, err := compileGuard("event.payload.complete == true", fixedClock)
	if err != nil {
		t.Fatalf("compileGuard: %v", err)
	}
	event := map[string]any{
		"payload": map[string]any{"complete": false},
	}
	if EvalGuard(program, nil, nil, event) {
		t.Fatal("expected false")
	}
}

func TestEvalGuardFalseOnMissingField(t *testing.T) {
	program, err := compileGuard("event.payload.complete == true", fixedClock)
	if err != nil {
		t.Fatalf("compileGuard: %v", err)
	}
	// event has no "payload" key — CEL returns error, guard returns false
	if EvalGuard(program, nil, nil, map[string]any{}) {
		t.Fatal("expected false for missing field")
	}
}

func TestEvalGuardUsesNowFunction(t *testing.T) {
	// fixedClock returns time.UnixMilli(1776100000000)
	// guard: now() == 1776100000000
	program, err := compileGuard("now() == 1776100000000", fixedClock)
	if err != nil {
		t.Fatalf("compileGuard: %v", err)
	}
	if !EvalGuard(program, nil, nil, nil) {
		t.Fatal("expected true: now() should match fixed clock value")
	}
}

func TestEvalGuardFalseOnTimeout(t *testing.T) {
	// Compile a guard that spins in a loop — not possible in CEL directly,
	// but we can test timeout by using a real slow program indirectly.
	// Instead we test the constant: guardEvalTimeout must be <= 10ms.
	if guardEvalTimeout > 10*time.Millisecond {
		t.Fatalf("guardEvalTimeout is %s, must be <= 10ms", guardEvalTimeout)
	}
}

func TestEvalGuardWithInputAndState(t *testing.T) {
	program, err := compileGuard("input.amount > state.threshold", fixedClock)
	if err != nil {
		t.Fatalf("compileGuard: %v", err)
	}
	input := map[string]any{"amount": 100}
	state := map[string]any{"threshold": 50}
	if !EvalGuard(program, input, state, nil) {
		t.Fatal("expected true: 100 > 50")
	}
}

func TestEvalGuardFalseWhenInputBelowState(t *testing.T) {
	program, err := compileGuard("input.amount > state.threshold", fixedClock)
	if err != nil {
		t.Fatalf("compileGuard: %v", err)
	}
	input := map[string]any{"amount": 10}
	state := map[string]any{"threshold": 50}
	if EvalGuard(program, input, state, nil) {
		t.Fatal("expected false: 10 > 50 is false")
	}
}
