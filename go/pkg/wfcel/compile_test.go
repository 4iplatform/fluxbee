package wfcel

import (
	"testing"
	"time"
)

func TestEvalGuardTrueWhenConditionMet(t *testing.T) {
	program, err := CompileGuard("event.payload.complete == true", fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	event := map[string]any{
		"payload": map[string]any{"complete": true},
	}
	if !EvalGuard(program, nil, nil, event) {
		t.Fatal("expected true")
	}
}

func TestEvalGuardFalseWhenConditionNotMet(t *testing.T) {
	program, err := CompileGuard("event.payload.complete == true", fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	event := map[string]any{
		"payload": map[string]any{"complete": false},
	}
	if EvalGuard(program, nil, nil, event) {
		t.Fatal("expected false")
	}
}

func TestEvalGuardFalseOnMissingField(t *testing.T) {
	program, err := CompileGuard("event.payload.complete == true", fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	if EvalGuard(program, nil, nil, map[string]any{}) {
		t.Fatal("expected false for missing field")
	}
}

func TestEvalGuardUsesNowFunction(t *testing.T) {
	program, err := CompileGuard("now() == 1776100000000", fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	if !EvalGuard(program, nil, nil, nil) {
		t.Fatal("expected true: now() should match fixed clock value")
	}
}

func TestEvalGuardTimeoutBudget(t *testing.T) {
	if GuardEvalTimeout > 10*time.Millisecond {
		t.Fatalf("GuardEvalTimeout is %s, must be <= 10ms", GuardEvalTimeout)
	}
}

func TestEvalGuardWithInputAndState(t *testing.T) {
	program, err := CompileGuard("input.amount > state.threshold", fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	input := map[string]any{"amount": 100}
	state := map[string]any{"threshold": 50}
	if !EvalGuard(program, input, state, nil) {
		t.Fatal("expected true: 100 > 50")
	}
}

func TestEvalGuardFalseWhenInputBelowState(t *testing.T) {
	program, err := CompileGuard("input.amount > state.threshold", fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	input := map[string]any{"amount": 10}
	state := map[string]any{"threshold": 50}
	if EvalGuard(program, input, state, nil) {
		t.Fatal("expected false: 10 > 50 is false")
	}
}

// gap-4: ext.Strings() (split/substring/...) is available to guards.
func TestGuardStringSplitExtension(t *testing.T) {
	program, err := CompileGuard(`event.payload.text.split(",")[1] == "b"`, fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	event := map[string]any{"payload": map[string]any{"text": "a,b,c"}}
	if !EvalGuard(program, nil, nil, event) {
		t.Fatal("expected split()[1] == \"b\"")
	}
}

func TestGuardSubstringExtension(t *testing.T) {
	program, err := CompileGuard(`event.payload.text.substring(0, 3) == "abc"`, fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	event := map[string]any{"payload": map[string]any{"text": "abcdef"}}
	if !EvalGuard(program, nil, nil, event) {
		t.Fatal("expected substring(0,3) == \"abc\"")
	}
}

// gap-4: json_parse decodes an AI structured-output string so a guard can branch on real fields.
func TestGuardJsonParse(t *testing.T) {
	program, err := CompileGuard(`json_parse(event.payload.body).status == "ok"`, fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	event := map[string]any{"payload": map[string]any{"body": `{"status":"ok"}`}}
	if !EvalGuard(program, nil, nil, event) {
		t.Fatal("expected json_parse(body).status == \"ok\"")
	}
}

func TestGuardJsonParseInvalidIsNotTrue(t *testing.T) {
	program, err := CompileGuard(`json_parse(event.payload.body).status == "ok"`, fixedClock)
	if err != nil {
		t.Fatalf("CompileGuard: %v", err)
	}
	event := map[string]any{"payload": map[string]any{"body": "not json"}}
	if EvalGuard(program, nil, nil, event) {
		t.Fatal("invalid JSON must make the guard non-true, not crash")
	}
}
