package node

import (
	"errors"
	"fmt"

	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

// errRefMissing marks a $ref whose path leads to no value: a key is absent, or a value on the way is
// not an object.
var errRefMissing = errors.New("not found")

// Resolve recursively walks payload and replaces any {"$ref": "root.field..."} object
// with the value found at that path in the provided context maps.
//
// Roots:
//
//	input  → the workflow instance's input map
//	state  → the workflow instance's state variable map
//	event  → the triggering event map
//
// A $ref whose path leads to no value is an error, which fails the action. The form
// {"$ref": "root.field...", "default": <value>} takes the literal value instead, and also when the
// path holds null (an IO node writes an absent field as null); {"$ref": ...} alone gives the null.
// Only an object whose "$ref" is a dot path is a $ref (wfcel.AsRef; validation refuses a root other
// than input., state. or event.): any other object, a JSON Schema's own "$ref" included, is a
// literal.
func Resolve(payload any, input, state, event map[string]any) (any, error) {
	return resolve(payload, input, state, event)
}

func resolve(v any, input, state, event map[string]any) (any, error) {
	switch typed := v.(type) {
	case map[string]any:
		if ref, ok := wfcel.AsRef(typed); ok {
			value, err := lookupRef(ref.Path, input, state, event)
			if ref.HasDefault && (errors.Is(err, errRefMissing) || (err == nil && value == nil)) {
				return ref.Default, nil
			}
			return value, err
		}
		// Regular map — recurse into values
		out := make(map[string]any, len(typed))
		for k, child := range typed {
			resolved, err := resolve(child, input, state, event)
			if err != nil {
				return nil, err
			}
			out[k] = resolved
		}
		return out, nil

	case []any:
		out := make([]any, len(typed))
		for i, child := range typed {
			resolved, err := resolve(child, input, state, event)
			if err != nil {
				return nil, err
			}
			out[i] = resolved
		}
		return out, nil

	default:
		return v, nil
	}
}

func lookupRef(path string, input, state, event map[string]any) (any, error) {
	parts := splitDotPath(path)
	if len(parts) < 2 {
		return nil, fmt.Errorf("$ref %q: must have at least root and one field", path)
	}
	var root map[string]any
	switch parts[0] {
	case "input":
		root = input
	case "state":
		root = state
	case "event":
		root = event
	default:
		return nil, fmt.Errorf("$ref %q: unknown root %q", path, parts[0])
	}
	return walkMap(root, parts[1:], path)
}

func walkMap(m map[string]any, keys []string, fullPath string) (any, error) {
	var cur any = m
	for _, key := range keys {
		asMap, ok := cur.(map[string]any)
		if !ok {
			return nil, fmt.Errorf("$ref %q: intermediate value is not an object: %w", fullPath, errRefMissing)
		}
		val, exists := asMap[key]
		if !exists {
			return nil, fmt.Errorf("$ref %q: key %q %w", fullPath, key, errRefMissing)
		}
		cur = val
	}
	return cur, nil
}

// splitDotPath splits "a.b.c" → ["a", "b", "c"] without importing strings.
func splitDotPath(path string) []string {
	var parts []string
	start := 0
	for i := 0; i < len(path); i++ {
		if path[i] == '.' {
			parts = append(parts, path[start:i])
			start = i + 1
		}
	}
	parts = append(parts, path[start:])
	return parts
}
