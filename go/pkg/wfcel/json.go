package wfcel

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"strconv"
	"strings"

	"github.com/google/cel-go/common/types"
	"github.com/google/cel-go/common/types/ref"
	"github.com/google/cel-go/common/types/traits"
)

// DecodeJSON reads JSON the way a WF reads every value it holds (an instance's input and state, an
// event's payload and context, json_parse): an integer literal is an int64 (a CEL int) and any
// other number a float64 (a CEL double). EncodeJSON writes a double with its decimal point, so a
// value keeps its type live and after a restart.
func DecodeJSON(data []byte) (any, error) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		return nil, err
	}
	if _, err := decoder.Token(); err != io.EOF {
		return nil, fmt.Errorf("invalid JSON: data after the top-level value")
	}
	return normalizeNumbers(value), nil
}

// DecodeJSONObject is DecodeJSON for a value that must be a JSON object.
func DecodeJSONObject(data []byte) (map[string]any, error) {
	value, err := DecodeJSON(data)
	if err != nil {
		return nil, err
	}
	object, ok := value.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("not a JSON object")
	}
	return object, nil
}

// EncodeJSON writes value as DecodeJSON reads it back: a float64 keeps its decimal point (7.0, not
// 7), so it stays a double. A NaN or an infinity is not JSON, and is an error. <, > and & are
// written as they are, as a frame carries them (the Go SDK's MarshalMessage): json.Marshal wrote
// them as <, > and & in every payload and context a WF sent.
func EncodeJSON(value any) ([]byte, error) {
	keyed, err := keepDoubles(value)
	if err != nil {
		return nil, err
	}
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(keyed); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// ValueToJSON is the plain JSON value a CEL result is kept as (a set_variable): an int is an int64,
// a double a float64, a uint an int64 when it fits (a float64 otherwise), a timestamp or a duration
// its CEL string, a list a []any and a map with string keys a map[string]any. What JSON cannot
// hold (bytes, a map with other keys, NaN, an error) is an error, never a value the store cannot
// write.
func ValueToJSON(val ref.Val) (any, error) {
	switch typed := val.(type) {
	case types.Null:
		return nil, nil
	case types.Bool:
		return bool(typed), nil
	case types.Int:
		return int64(typed), nil
	case types.Uint:
		if uint64(typed) <= math.MaxInt64 {
			return int64(typed), nil
		}
		return float64(typed), nil
	case types.Double:
		f := float64(typed)
		if math.IsNaN(f) || math.IsInf(f, 0) {
			return nil, fmt.Errorf("%v is not a JSON number", f)
		}
		return f, nil
	case types.String:
		return string(typed), nil
	case types.Timestamp, types.Duration:
		if text, ok := val.ConvertToType(types.StringType).(types.String); ok {
			return string(text), nil
		}
	case traits.Mapper:
		out := map[string]any{}
		for it := typed.Iterator(); it.HasNext() == types.True; {
			key := it.Next()
			name, ok := key.(types.String)
			if !ok {
				return nil, fmt.Errorf("a map key must be a string to be kept as JSON, not %s", key.Type().TypeName())
			}
			child, err := ValueToJSON(typed.Get(key))
			if err != nil {
				return nil, fmt.Errorf("%s: %w", string(name), err)
			}
			out[string(name)] = child
		}
		return out, nil
	case traits.Lister:
		out := []any{}
		for it := typed.Iterator(); it.HasNext() == types.True; {
			child, err := ValueToJSON(it.Next())
			if err != nil {
				return nil, fmt.Errorf("[%d]: %w", len(out), err)
			}
			out = append(out, child)
		}
		return out, nil
	}
	if err, ok := val.(*types.Err); ok {
		return nil, err
	}
	return nil, fmt.Errorf("a %s value cannot be kept as JSON", val.Type().TypeName())
}

func normalizeNumbers(value any) any {
	switch typed := value.(type) {
	case json.Number:
		if i, err := typed.Int64(); err == nil {
			return i
		}
		if f, err := typed.Float64(); err == nil {
			return f
		}
		return typed.String()
	case map[string]any:
		for key, child := range typed {
			typed[key] = normalizeNumbers(child)
		}
		return typed
	case []any:
		for i, child := range typed {
			typed[i] = normalizeNumbers(child)
		}
		return typed
	default:
		return value
	}
}

// keepDoubles copies value with every float64 as the json.Number EncodeJSON writes for it.
func keepDoubles(value any) (any, error) {
	switch typed := value.(type) {
	case float64:
		literal, err := doubleLiteral(typed)
		if err != nil {
			return nil, err
		}
		return json.Number(literal), nil
	case map[string]any:
		out := make(map[string]any, len(typed))
		for key, child := range typed {
			kept, err := keepDoubles(child)
			if err != nil {
				return nil, fmt.Errorf("%s: %w", key, err)
			}
			out[key] = kept
		}
		return out, nil
	case []any:
		out := make([]any, len(typed))
		for i, child := range typed {
			kept, err := keepDoubles(child)
			if err != nil {
				return nil, fmt.Errorf("[%d]: %w", i, err)
			}
			out[i] = kept
		}
		return out, nil
	default:
		return value, nil
	}
}

// doubleLiteral is f as encoding/json writes it, with ".0" when that reads as an integer.
func doubleLiteral(f float64) (string, error) {
	if math.IsNaN(f) || math.IsInf(f, 0) {
		return "", fmt.Errorf("%v is not a JSON number", f)
	}
	format := byte('f')
	if abs := math.Abs(f); abs != 0 && (abs < 1e-6 || abs >= 1e21) {
		format = 'e'
	}
	literal := strconv.FormatFloat(f, format, -1, 64)
	if !strings.ContainsAny(literal, ".eE") {
		literal += ".0"
	}
	return literal, nil
}
