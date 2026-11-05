package sdk

import (
	"encoding/json"
	"os"
	"strings"
	"sync"
	"time"
)

// Who wrote a message's text (docs/cognition-hands-v1.md): meta.context.writer, the writer's ILK and
// name. Whoever writes the text names it: an IO node the person, an agent itself, a WF itself for its
// own text; a forward keeps it. As it sends, a node fills a missing name from the identity memory of
// its hive (NodeSender.SendTraced). The Rust SDK does the same (fluxbee_sdk::message_writer).

// WriterKey is the writer's key in meta.context.
const WriterKey = "writer"

// envSelfIlkID is the variable the orchestrator sets to a dynamic node's own ILK (AI.*, IO.*, WF.*)
// when it spawns it.
const envSelfIlkID = "FLUXBEE_NODE_ILK_ID"

const (
	// ilkFactsTTL is how long a node keeps what the identity memory said of an ILK: a person who
	// registers in the middle of a conversation gets their name in their messages within it.
	ilkFactsTTL = time.Minute
	// ilkFactsMax is the most ILKs a node keeps facts of; past it, it forgets them all and reads
	// again.
	ilkFactsMax = 4096
)

// Writer is who wrote a message's text: their ILK, and their name. A node with no ILK of its own
// names only its name; a person the identity has no name for, only their ILK.
type Writer struct {
	ILK  string `json:"ilk,omitempty"`
	Name string `json:"name,omitempty"`
}

// ReadSelfIlkFromEnv is this node's own ILK, as the orchestrator gave it (FLUXBEE_NODE_ILK_ID), or
// "" when it gave none.
func ReadSelfIlkFromEnv() string {
	value := strings.TrimSpace(os.Getenv(envSelfIlkID))
	if !strings.HasPrefix(value, "ilk:") {
		return ""
	}
	return value
}

// WriterOf is the writer a context names, when it names one with an ILK or a name.
func WriterOf(context json.RawMessage) (Writer, bool) {
	fields, ok := contextFields(context)
	if !ok || len(fields[WriterKey]) == 0 {
		return Writer{}, false
	}
	var writer Writer
	if json.Unmarshal(fields[WriterKey], &writer) != nil {
		return Writer{}, false
	}
	writer.ILK, writer.Name = strings.TrimSpace(writer.ILK), strings.TrimSpace(writer.Name)
	return writer, writer.ILK != "" || writer.Name != ""
}

// WithWriter is context with writer as its writer, in place of any other, the context made when it
// is missing. Its other keys go out as they came, byte for byte. A context that is not an object is
// not changed (false).
func WithWriter(context json.RawMessage, writer Writer) (json.RawMessage, bool) {
	fields, ok := contextFields(context)
	if !ok {
		return context, false
	}
	raw, err := encodeTraceJSON(writer)
	if err != nil {
		return context, false
	}
	fields[WriterKey] = raw
	out, err := encodeTraceJSON(fields)
	if err != nil {
		return context, false
	}
	return out, true
}

// IlkFacts is what the identity memory says of an ILK: its type (human, agent or system) and its
// display name, a registered person's name or a node's name.
type IlkFacts struct {
	Type string
	Name string
}

// fillWriterName fills the name of msg's writer, when it names an ILK and no name, with what facts
// says of that ILK: a solution message only, and only when the identity has a name for it.
func fillWriterName(msg *Message, facts func(ilk string) (IlkFacts, bool)) {
	if !IsSolutionKind(msg.Meta.MsgType) {
		return
	}
	writer, ok := WriterOf(msg.Meta.Context)
	if !ok || writer.Name != "" || writer.ILK == "" {
		return
	}
	known, ok := facts(writer.ILK)
	if !ok || known.Name == "" {
		return
	}
	writer.Name = known.Name
	if context, ok := WithWriter(msg.Meta.Context, writer); ok {
		msg.Meta.Context = context
	}
}

var ilkFactsCache = struct {
	sync.Mutex
	known map[string]ilkFactsRead
}{known: map[string]ilkFactsRead{}}

type ilkFactsRead struct {
	at    time.Time
	facts IlkFacts
	found bool
}

// IlkFactsOf is what the identity memory of hiveID says of ilk, as this node read it within the last
// ilkFactsTTL: false when the identity has no such active ILK, or cannot be read.
func IlkFactsOf(hiveID, ilk string) (IlkFacts, bool) {
	hiveID, ilk = strings.TrimSpace(hiveID), strings.TrimSpace(ilk)
	if hiveID == "" || ilk == "" {
		return IlkFacts{}, false
	}
	key := hiveID + "\x00" + strings.ToLower(ilk)
	ilkFactsCache.Lock()
	read, cached := ilkFactsCache.known[key]
	ilkFactsCache.Unlock()
	if cached && time.Since(read.at) < ilkFactsTTL {
		return read.facts, read.found
	}
	facts, found, err := LookupIlk(hiveID, ilk)
	if err != nil {
		found = false
	}
	ilkFactsCache.Lock()
	if len(ilkFactsCache.known) >= ilkFactsMax {
		ilkFactsCache.known = map[string]ilkFactsRead{}
	}
	ilkFactsCache.known[key] = ilkFactsRead{at: time.Now(), facts: facts, found: found}
	ilkFactsCache.Unlock()
	return facts, found
}

// hiveOf is the hive of a node's full name (AI.sales@worker1), or "".
func hiveOf(nodeName string) string {
	at := strings.LastIndex(nodeName, "@")
	if at < 0 {
		return ""
	}
	return strings.TrimSpace(nodeName[at+1:])
}
