package sdk

import (
	"bytes"
	"encoding/json"
	"strings"
	"time"

	"github.com/google/uuid"
)

// The trace of a message's cycle (docs/message-trace-v1.md): meta.context.trace, one entry for each
// node a solution message went through, oldest first. Each node writes its own entry as it sends
// (NodeSender.SendTraced): when, from which node, to which, and what it did. The Rust SDK writes
// the same (fluxbee_sdk::message_trace).

const (
	// TraceKey is the trace's key in meta.context.
	TraceKey = "trace"
	// MaxActWords is the most words an entry's act keeps (§3, decision 13).
	MaxActWords = 8
	// MaxActWordChars is the most characters an act word keeps (§3, decision 13).
	MaxActWordChars = 32
)

// TraceEntry is a node's entry in a trace: when it sent the message, its own name, the destination
// by name, and the actions it took.
type TraceEntry struct {
	T    string   `json:"t"`
	From string   `json:"from"`
	To   string   `json:"to,omitempty"`
	Act  []string `json:"act,omitempty"`
}

// TraceNote is what a node adds to the entry its SDK writes as it sends (§5): the destination by
// name, when the message goes to a UUID (a reply), and the actions it took.
type TraceNote struct {
	To  string
	Act []string
}

// IsSolutionKind reports whether a msg_type is the solution's: any kind but system, admin and
// query_response. The router publishes these as turns, and they carry the trace of their cycle.
func IsSolutionKind(msgType string) bool {
	return !IsSystemKind(msgType) && msgType != "admin" && msgType != "query_response"
}

// WriteTraceEntry writes from's entry on msg as it sends it, with note: the destination is note.To,
// else the message's when it names a node. A message that is not the solution's, or a sender with
// no name, gets none, and so does a context that is not a JSON object (a node's own).
func WriteTraceEntry(msg *Message, from string, note TraceNote) {
	from = strings.TrimSpace(from)
	if from == "" || !IsSolutionKind(msg.Meta.MsgType) {
		return
	}
	to := strings.TrimSpace(note.To)
	if to == "" {
		to = traceDestinationName(msg.Routing.Dst)
	}
	entry := TraceEntry{
		T:    time.Now().UTC().Format("2006-01-02T15:04:05.000Z"),
		From: from,
		To:   to,
		Act:  traceActWords(note.Act),
	}
	if context, ok := appendTraceEntry(msg.Meta.Context, entry); ok {
		msg.Meta.Context = context
	}
}

// TraceOf is a context's trace as it came, or nil when it has none.
func TraceOf(context json.RawMessage) json.RawMessage {
	fields, ok := contextFields(context)
	if !ok {
		return nil
	}
	var list []json.RawMessage
	if json.Unmarshal(fields[TraceKey], &list) != nil || len(list) == 0 {
		return nil
	}
	return fields[TraceKey]
}

// TraceEntries is a context's trace, oldest first. An entry that does not read as one is skipped.
func TraceEntries(context json.RawMessage) []TraceEntry {
	trace := TraceOf(context)
	if trace == nil {
		return nil
	}
	var list []json.RawMessage
	_ = json.Unmarshal(trace, &list)
	entries := make([]TraceEntry, 0, len(list))
	for _, raw := range list {
		var entry TraceEntry
		if json.Unmarshal(raw, &entry) == nil && entry.From != "" {
			entries = append(entries, entry)
		}
	}
	return entries
}

// appendTraceEntry is context with entry at the end of its trace, the context and the list made
// when they are missing, and a trace that is not a list replaced. The context's other keys go out
// as they came, byte for byte. A context that is not an object is not changed (false).
func appendTraceEntry(context json.RawMessage, entry TraceEntry) (json.RawMessage, bool) {
	fields, ok := contextFields(context)
	if !ok {
		return context, false
	}
	var list []json.RawMessage
	if json.Unmarshal(fields[TraceKey], &list) != nil {
		list = nil
	}
	raw, err := encodeTraceJSON(entry)
	if err != nil {
		return context, false
	}
	trace, err := encodeTraceJSON(append(list, raw))
	if err != nil {
		return context, false
	}
	fields[TraceKey] = trace
	out, err := encodeTraceJSON(fields)
	if err != nil {
		return context, false
	}
	return out, true
}

// contextFields is a context's keys with their values as they came; an absent or null context is
// an empty one. A context that is not an object reads false.
func contextFields(context json.RawMessage) (map[string]json.RawMessage, bool) {
	trimmed := bytes.TrimSpace(context)
	if len(trimmed) == 0 || bytes.Equal(trimmed, []byte("null")) {
		return map[string]json.RawMessage{}, true
	}
	var fields map[string]json.RawMessage
	if json.Unmarshal(trimmed, &fields) != nil || fields == nil {
		return nil, false
	}
	return fields, true
}

// encodeTraceJSON marshals v as the SDK sends JSON (MarshalMessage): without HTML escapes, so the
// values that go out as they came keep their bytes.
func encodeTraceJSON(v any) (json.RawMessage, error) {
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// traceDestinationName is a destination's name for an entry: a node's name, or "broadcast". A UUID
// is the node's to name (TraceNote.To), and a message with no destination gets the router's entry
// when it picks one.
func traceDestinationName(dst Destination) string {
	switch {
	case dst.IsBroadcast():
		return "broadcast"
	case dst.IsUnicast():
		value := strings.TrimSpace(dst.Value())
		if _, err := uuid.Parse(value); err == nil {
			return ""
		}
		return value
	default:
		return ""
	}
}

// traceActWords is words as an entry keeps them: each one trimmed and cut at MaxActWordChars
// characters, no blank or repeated one, at most MaxActWords.
func traceActWords(words []string) []string {
	var kept []string
	for _, word := range words {
		word = strings.TrimSpace(word)
		if runes := []rune(word); len(runes) > MaxActWordChars {
			word = string(runes[:MaxActWordChars])
		}
		if word == "" || containsString(kept, word) {
			continue
		}
		kept = append(kept, word)
		if len(kept) == MaxActWords {
			break
		}
	}
	return kept
}
