package sdk

import (
	"encoding/binary"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/uuid"
)

// A writer is set beside a context's other keys, which go out byte for byte, read back, and replaced
// (docs/cognition-hands-v1.md §4).
func TestAWriterIsSetReadAndReplaced(t *testing.T) {
	context := json.RawMessage(`{"trace":[{"t":"x","from":"IO.slack@motherbee"}],"io":{"text":"a < b"}}`)
	out, ok := WithWriter(context, Writer{ILK: "ilk:ana"})
	if !ok {
		t.Fatal("not an object")
	}
	if writer, ok := WriterOf(out); !ok || writer != (Writer{ILK: "ilk:ana"}) {
		t.Fatalf("writer: %+v %v", writer, ok)
	}
	var fields map[string]json.RawMessage
	_ = json.Unmarshal(out, &fields)
	if string(fields["io"]) != `{"text":"a < b"}` || string(fields["trace"]) != `[{"t":"x","from":"IO.slack@motherbee"}]` {
		t.Fatalf("the other keys changed: %s", out)
	}
	out, _ = WithWriter(out, Writer{ILK: "ilk:wf", Name: "WF.soporte@worker1"})
	if writer, _ := WriterOf(out); writer != (Writer{ILK: "ilk:wf", Name: "WF.soporte@worker1"}) {
		t.Fatalf("replaced: %+v", writer)
	}
	if made, ok := WithWriter(nil, Writer{Name: "WF.soporte@worker1"}); !ok || string(made) != `{"writer":{"name":"WF.soporte@worker1"}}` {
		t.Fatalf("made: %s", made)
	}
	if _, ok := WithWriter(json.RawMessage(`"text"`), Writer{ILK: "ilk:ana"}); ok {
		t.Fatal("a context that is not an object was changed")
	}
	for _, none := range []string{`{}`, `{"writer":{"ilk":" "}}`, `{"writer":"ilk:ana"}`, `null`} {
		if writer, ok := WriterOf(json.RawMessage(none)); ok {
			t.Fatalf("%s: %+v", none, writer)
		}
	}
}

// As it sends, a node fills a missing name from the identity; a name the message has stays, and a
// system message reads nothing.
func TestAMissingNameIsFilledFromTheIdentity(t *testing.T) {
	known := func(ilk string) (IlkFacts, bool) {
		switch ilk {
		case "ilk:ana":
			return IlkFacts{Type: "human", Name: "Ana Pérez"}, true
		case "ilk:temp":
			return IlkFacts{Type: "human"}, true
		}
		return IlkFacts{}, false
	}
	msg := Message{Meta: Meta{MsgType: "user", Context: json.RawMessage(`{"writer":{"ilk":"ilk:ana"}}`)}}
	fillWriterName(&msg, known)
	if writer, _ := WriterOf(msg.Meta.Context); writer != (Writer{ILK: "ilk:ana", Name: "Ana Pérez"}) {
		t.Fatalf("filled: %+v", writer)
	}
	for _, context := range []string{
		`{"writer":{"ilk":"ilk:ana","name":"Ana"}}`,
		`{"writer":{"ilk":"ilk:temp"}}`,
		`{"writer":{"ilk":"ilk:unknown"}}`,
		`{"writer":{"name":"WF.soporte@worker1"}}`,
	} {
		msg := Message{Meta: Meta{MsgType: "user", Context: json.RawMessage(context)}}
		fillWriterName(&msg, known)
		if string(msg.Meta.Context) != context {
			t.Fatalf("%s became %s", context, msg.Meta.Context)
		}
	}
	system := Message{Meta: Meta{MsgType: SYSTEMKind, Context: json.RawMessage(`{"writer":{"ilk":"ilk:ana"}}`)}}
	fillWriterName(&system, func(string) (IlkFacts, bool) {
		t.Fatal("no identity read for a system message")
		return IlkFacts{}, false
	})
}

// The identity memory says an active ILK's type and display name, and a display name that is only
// the ILK is no name; an inactive one and an unknown id are misses, and so is a region that is not
// there.
func TestLookupIlkReadsTypeAndNameFromTheIdentityMemory(t *testing.T) {
	ana, wf, gone, temp := uuid.New(), uuid.New(), uuid.New(), uuid.New()
	path := writeIdentityRegion(t, []testIlk{
		{id: ana, kind: 0, name: "Ana Pérez", active: true},
		{id: wf, kind: 1, name: "WF.soporte@worker1", active: true},
		{id: gone, kind: 0, name: "Gone", active: false},
		// SY.identity writes a person with no name under their own ILK (found in 0.1.101's gate).
		{id: temp, kind: 0, name: "ilk:" + temp.String(), active: true},
	})
	if facts, ok, _ := lookupIlkAt(path, "ilk:"+temp.String()); !ok || facts != (IlkFacts{Type: "human"}) {
		t.Fatalf("a person named by their own ILK has no name: %+v %v", facts, ok)
	}
	if facts, ok, err := lookupIlkAt(path, "ilk:"+ana.String()); err != nil || !ok || facts != (IlkFacts{Type: "human", Name: "Ana Pérez"}) {
		t.Fatalf("ana: %+v %v %v", facts, ok, err)
	}
	if facts, ok, _ := lookupIlkAt(path, "ilk:"+wf.String()); !ok || facts != (IlkFacts{Type: "agent", Name: "WF.soporte@worker1"}) {
		t.Fatalf("wf: %+v %v", facts, ok)
	}
	for _, miss := range []string{"ilk:" + gone.String(), "ilk:" + uuid.New().String(), "ilk:not-a-uuid", ""} {
		if facts, ok, err := lookupIlkAt(path, miss); ok || err != nil {
			t.Fatalf("%s: %+v %v %v", miss, facts, ok, err)
		}
	}
	if _, ok, err := lookupIlkAt(filepath.Join(t.TempDir(), "none"), "ilk:"+ana.String()); ok || err != nil {
		t.Fatalf("no region: %v %v", ok, err)
	}
}

func TestReadSelfIlkFromEnv(t *testing.T) {
	t.Setenv(envSelfIlkID, " ilk:wf ")
	if got := ReadSelfIlkFromEnv(); got != "ilk:wf" {
		t.Fatalf("got %q", got)
	}
	t.Setenv(envSelfIlkID, "wf")
	if got := ReadSelfIlkFromEnv(); got != "" {
		t.Fatalf("got %q", got)
	}
	if hiveOf("WF.soporte@worker1") != "worker1" || hiveOf("WF.soporte") != "" {
		t.Fatal("hiveOf")
	}
}

type testIlk struct {
	id     uuid.UUID
	kind   byte
	name   string
	active bool
}

// writeIdentityRegion writes an identity region of this SDK's version with one tenant slot and
// ilks, as SY.identity lays it out (src/shm/mod.rs).
func writeIdentityRegion(t *testing.T, ilks []testIlk) string {
	t.Helper()
	const maxTenants = 1
	ilkOffset := identityAlignUp(identityAlignUp(identityHeaderSize, identityRegionAlign)+tenantEntrySize*maxTenants, identityRegionAlign)
	data := make([]byte, ilkOffset+len(ilks)*ilkEntrySize)
	binary.LittleEndian.PutUint32(data[hdrOffMagic:], identityMagic)
	binary.LittleEndian.PutUint32(data[hdrOffVersion:], identityVersionCurrent)
	binary.LittleEndian.PutUint32(data[hdrOffIlkCount:], uint32(len(ilks)))
	binary.LittleEndian.PutUint32(data[hdrOffMaxIlks:], uint32(len(ilks)))
	binary.LittleEndian.PutUint32(data[hdrOffMaxTenants:], maxTenants)
	for i, ilk := range ilks {
		entry := data[ilkOffset+i*ilkEntrySize:]
		copy(entry[ilkOffIlkID:], ilk.id[:])
		entry[ilkOffType] = ilk.kind
		if ilk.active {
			binary.LittleEndian.PutUint16(entry[ilkOffFlags:], ilkFlagActive)
		}
		copy(entry[ilkOffDisplayName:ilkOffDisplayName+128], ilk.name)
	}
	path := filepath.Join(t.TempDir(), "jsr-identity-test")
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}
