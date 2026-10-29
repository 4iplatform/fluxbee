package wfcel

import (
	"math"
	"reflect"
	"strings"
	"testing"

	"github.com/google/cel-go/common/types"
)

// A JSON integer is an int64 and any other number a float64, and EncodeJSON writes them back so
// DecodeJSON reads the same types: a value keeps its type through the store (FINDINGS A-118).
func TestNumbersKeepTheirTypeThroughEncodeAndDecode(t *testing.T) {
	decoded, err := DecodeJSONObject([]byte(`{"n": 3, "x": 2.0, "big": 1e400, "e": 1e2, "nested": {"m": [1, 2.5, -0.0]}}`))
	if err != nil {
		t.Fatal(err)
	}
	want := map[string]any{
		"n":      int64(3),
		"x":      float64(2),
		"big":    "1e400",
		"e":      float64(100),
		"nested": map[string]any{"m": []any{int64(1), 2.5, math.Copysign(0, -1)}},
	}
	if !reflect.DeepEqual(decoded, want) {
		t.Fatalf("decoded = %#v", decoded)
	}
	encoded, err := EncodeJSON(decoded)
	if err != nil {
		t.Fatal(err)
	}
	for _, literal := range []string{`"x":2.0`, `"e":100.0`, `"n":3`, `[1,2.5,-0.0]`} {
		if !strings.Contains(string(encoded), literal) {
			t.Fatalf("%s lacks %s", encoded, literal)
		}
	}
	again, err := DecodeJSONObject(encoded)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(again, decoded) {
		t.Fatalf("after a round trip %#v, want %#v", again, decoded)
	}
	if _, err := EncodeJSON(map[string]any{"bad": math.NaN()}); err == nil {
		t.Fatal("NaN is not JSON")
	}
	if _, err := DecodeJSON([]byte(`{"a":1} {"b":2}`)); err == nil {
		t.Fatal("data after the value is not one JSON value")
	}
	if _, err := DecodeJSONObject([]byte(`[1]`)); err == nil {
		t.Fatal("an array is not an object")
	}
}

// A CEL result is kept as plain JSON: a map literal (which cel-go holds as map[ref.Val]ref.Val, and
// which panicked the store before 0.1.90) becomes a map[string]any, and what JSON cannot hold is an
// error, never a value the store cannot write.
func TestValueToJSONKeepsPlainJSON(t *testing.T) {
	eval := func(expr string) any {
		t.Helper()
		program, err := CompileGuard(expr, fixedClock)
		if err != nil {
			t.Fatalf("%s: %v", expr, err)
		}
		val, _, err := program.Eval(map[string]any{
			"input": map[string]any{},
			"state": map[string]any{},
			"event": map[string]any{"payload": map[string]any{"content": `{"score": 8, "ratio": 0.5}`}},
		})
		if err != nil {
			t.Fatalf("%s: %v", expr, err)
		}
		plain, err := ValueToJSON(val)
		if err != nil {
			return err
		}
		return plain
	}
	cases := []struct {
		expr string
		want any
	}{
		{`{"a": 1, "b": [1, 2.5, "x", null, true]}`, map[string]any{"a": int64(1), "b": []any{int64(1), 2.5, "x", nil, true}}},
		{`3.0`, float64(3)},
		{`7u`, int64(7)},
		{`json_parse(event.payload.content)`, map[string]any{"score": int64(8), "ratio": 0.5}},
		{`timestamp("2026-10-28T00:00:00Z")`, "2026-10-28T00:00:00Z"},
		{`duration("90s")`, "90s"},
	}
	for _, c := range cases {
		if got := eval(c.expr); !reflect.DeepEqual(got, c.want) {
			t.Fatalf("%s = %#v, want %#v", c.expr, got, c.want)
		}
	}
	for _, expr := range []string{`{1: "a"}`, `b"bytes"`, `[0.0 / 0.0]`} {
		if _, ok := eval(expr).(error); !ok {
			t.Fatalf("%s must not be kept as JSON", expr)
		}
	}
	if _, err := ValueToJSON(types.NewErr("boom")); err == nil {
		t.Fatal("a CEL error is not a value")
	}
}

// json_parse reads numbers as the event does: an integer is a CEL int, so a structured answer's
// {"score": 8} compares with an int literal.
func TestJSONParseReadsAnIntegerAsAnInt(t *testing.T) {
	program, err := CompileGuard(`json_parse(event.payload.content).score >= 7 && json_parse(event.payload.content).ratio < 0.75`, fixedClock)
	if err != nil {
		t.Fatal(err)
	}
	event := map[string]any{"payload": map[string]any{"content": `{"score": 8, "ratio": 0.5}`}}
	if !EvalGuard(program, nil, nil, event) {
		t.Fatal("expected score >= 7 with an integer score")
	}
}
