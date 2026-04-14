package node

import (
	"reflect"
	"testing"
)

func TestResolvePassesThroughLiterals(t *testing.T) {
	payload := map[string]any{
		"note": "literal string",
		"num":  42,
	}
	got, err := Resolve(payload, nil, nil, nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !reflect.DeepEqual(got, payload) {
		t.Fatalf("expected %v, got %v", payload, got)
	}
}

func TestResolveRefFromInput(t *testing.T) {
	payload := map[string]any{
		"customer_id": map[string]any{"$ref": "input.customer_id"},
	}
	input := map[string]any{"customer_id": "cust-123"}
	got, err := Resolve(payload, input, nil, nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	m := got.(map[string]any)
	if m["customer_id"] != "cust-123" {
		t.Fatalf("expected cust-123, got %v", m["customer_id"])
	}
}

func TestResolveRefFromState(t *testing.T) {
	payload := map[string]any{"$ref": "state.validated_at"}
	state := map[string]any{"validated_at": int64(1776100000000)}
	got, err := Resolve(payload, nil, state, nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got != int64(1776100000000) {
		t.Fatalf("expected 1776100000000, got %v", got)
	}
}

func TestResolveRefFromEvent(t *testing.T) {
	payload := map[string]any{"$ref": "event.payload.order_id"}
	event := map[string]any{
		"payload": map[string]any{"order_id": "ord-99"},
	}
	got, err := Resolve(payload, nil, nil, event)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got != "ord-99" {
		t.Fatalf("expected ord-99, got %v", got)
	}
}

func TestResolveNestedMap(t *testing.T) {
	payload := map[string]any{
		"outer": map[string]any{
			"inner": map[string]any{"$ref": "input.x"},
		},
	}
	input := map[string]any{"x": "hello"}
	got, err := Resolve(payload, input, nil, nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	outer := got.(map[string]any)["outer"].(map[string]any)
	if outer["inner"] != "hello" {
		t.Fatalf("expected hello, got %v", outer["inner"])
	}
}

func TestResolveSlice(t *testing.T) {
	payload := []any{
		map[string]any{"$ref": "input.a"},
		"literal",
		map[string]any{"$ref": "input.b"},
	}
	input := map[string]any{"a": 1, "b": 2}
	got, err := Resolve(payload, input, nil, nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	sl := got.([]any)
	if sl[0] != 1 || sl[1] != "literal" || sl[2] != 2 {
		t.Fatalf("unexpected slice: %v", sl)
	}
}

func TestResolveMissingKeyReturnsError(t *testing.T) {
	payload := map[string]any{"$ref": "input.missing"}
	_, err := Resolve(payload, map[string]any{}, nil, nil)
	if err == nil {
		t.Fatal("expected error for missing key")
	}
}

func TestResolveUnknownRootReturnsError(t *testing.T) {
	payload := map[string]any{"$ref": "ctx.something"}
	_, err := Resolve(payload, nil, nil, nil)
	if err == nil {
		t.Fatal("expected error for unknown root")
	}
}

func TestResolveNilPayload(t *testing.T) {
	got, err := Resolve(nil, nil, nil, nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got != nil {
		t.Fatalf("expected nil, got %v", got)
	}
}
