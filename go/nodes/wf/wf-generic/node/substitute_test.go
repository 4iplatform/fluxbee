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

// A JSON Schema's own "$ref" (it holds "#", "/" or ":") is a literal, not a $ref.
func TestResolveAJSONSchemaRefIsALiteral(t *testing.T) {
	for _, literal := range []map[string]any{
		{"$ref": "#/definitions/item", "description": "a JSON Schema's own reference"},
		{"$ref": "https://example.com/item.json"},
	} {
		got, err := Resolve(literal, nil, nil, nil)
		if err != nil || !reflect.DeepEqual(got, literal) {
			t.Fatalf("%v = %#v, %v", literal, got, err)
		}
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

// 0.1.90: {"$ref": path, "default": value} takes the literal value when the path leads to no value
// (a key absent, or a value on the way that is not an object) or holds null, as an IO node writes an
// absent field; {"$ref": path} alone gives the null.
func TestResolveRefWithDefault(t *testing.T) {
	event := map[string]any{"payload": map[string]any{"text": "hello", "empty": nil}}
	cases := []struct {
		ref  map[string]any
		want any
	}{
		{map[string]any{"$ref": "event.context", "default": map[string]any{}}, map[string]any{}},
		{map[string]any{"$ref": "event.payload.text.lang", "default": "en"}, "en"},
		{map[string]any{"$ref": "event.payload.text", "default": "x"}, "hello"},
		{map[string]any{"$ref": "event.payload.empty", "default": "x"}, "x"},
		{map[string]any{"$ref": "event.payload.empty"}, nil},
		{map[string]any{"$ref": "input.missing", "default": nil}, nil},
	}
	for _, c := range cases {
		got, err := Resolve(c.ref, map[string]any{}, nil, event)
		if err != nil || !reflect.DeepEqual(got, c.want) {
			t.Fatalf("%v = %#v, %v; want %#v", c.ref, got, err, c.want)
		}
	}
	// Without a default, a missing field is still an error.
	if _, err := Resolve(map[string]any{"$ref": "event.context"}, nil, nil, event); err == nil {
		t.Fatal("a missing field without a default is an error")
	}
}
