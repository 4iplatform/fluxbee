package node

import "fmt"

// Resolve recursively walks payload and replaces any {"$ref": "root.field..."} object
// with the value found at that path in the provided context maps.
//
// Roots:
//
//	input  → the workflow instance's input map
//	state  → the workflow instance's state variable map
//	event  → the triggering event map
//
// If a $ref path cannot be resolved (missing key), Resolve returns an error.
func Resolve(payload any, input, state, event map[string]any) (any, error) {
	return resolve(payload, input, state, event)
}

func resolve(v any, input, state, event map[string]any) (any, error) {
	switch typed := v.(type) {
	case map[string]any:
		// {"$ref": "root.field"} — single-key object with $ref
		if ref, ok := typed["$ref"]; ok && len(typed) == 1 {
			refPath, ok := ref.(string)
			if !ok {
				return nil, fmt.Errorf("$ref must be a string")
			}
			return lookupRef(refPath, input, state, event)
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
			return nil, fmt.Errorf("$ref %q: intermediate value is not an object", fullPath)
		}
		val, exists := asMap[key]
		if !exists {
			return nil, fmt.Errorf("$ref %q: key %q not found", fullPath, key)
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
