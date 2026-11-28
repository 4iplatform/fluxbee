package sdk

import (
	"encoding/binary"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
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

// A virtual person reads as a person with Virtual set; the bit changes no type.
func TestLookupIlkReadsTheVirtualFlag(t *testing.T) {
	laura, ana := uuid.New(), uuid.New()
	path := writeIdentityRegion(t, []testIlk{
		{id: laura, kind: 0, name: "Laura", active: true, virtual: true},
		{id: ana, kind: 0, name: "Ana", active: true},
	})
	if facts, ok, err := lookupIlkAt(path, "ilk:"+laura.String()); err != nil || !ok || facts != (IlkFacts{Type: "human", Name: "Laura", Virtual: true}) {
		t.Fatalf("laura: %+v %v %v", facts, ok, err)
	}
	if facts, ok, err := lookupIlkAt(path, "ilk:"+ana.String()); err != nil || !ok || facts != (IlkFacts{Type: "human", Name: "Ana"}) {
		t.Fatalf("ana: %+v %v %v", facts, ok, err)
	}
	if ilkFlagVirtual&0x001f != 0 {
		t.Fatalf("the virtual bit collides with the generic flags: %#x", ilkFlagVirtual)
	}
}

// An ILK that works for a person reads as that person's ILK and its role; one that works for
// nobody, or whose role byte no writer of this version writes, reads as "".
func TestLookupIlkReadsWhomAnIlkWorksFor(t *testing.T) {
	laura := uuid.MustParse("5a5a5a5a-5a5a-45a5-85a5-5a5a5a5a5a5a")
	conscious, channel, agent, alone, odd := uuid.New(), uuid.New(), uuid.New(), uuid.New(), uuid.New()
	path := writeIdentityRegion(t, []testIlk{
		{id: conscious, kind: 1, name: "AI.ed4qm7k2xa@worker1", active: true, worksFor: laura, workRole: ilkWorkRoleConscious},
		{id: channel, kind: 1, name: "IO.wapp.ed4qm7k2xa@worker1", active: true, worksFor: laura, workRole: ilkWorkRoleChannel},
		{id: agent, kind: 1, name: "AI.helper@worker1", active: true, worksFor: laura, workRole: ilkWorkRoleAgent},
		{id: alone, kind: 1, name: "IO.slack@worker1", active: true},
		{id: odd, kind: 1, name: "IO.odd@worker1", active: true, worksFor: laura, workRole: 9},
	})
	for _, want := range []struct {
		id   uuid.UUID
		role string
	}{{conscious, "conscious"}, {channel, "channel"}, {agent, "agent"}} {
		facts, ok, err := lookupIlkAt(path, "ilk:"+want.id.String())
		if err != nil || !ok || facts.WorksFor != "ilk:5a5a5a5a-5a5a-45a5-85a5-5a5a5a5a5a5a" || facts.WorkRole != want.role || facts.Type != "agent" {
			t.Fatalf("%s: %+v %v %v", want.role, facts, ok, err)
		}
	}
	if facts, ok, err := lookupIlkAt(path, "ilk:"+alone.String()); err != nil || !ok || facts != (IlkFacts{Type: "agent", Name: "IO.slack@worker1"}) {
		t.Fatalf("alone: %+v %v %v", facts, ok, err)
	}
	if facts, ok, err := lookupIlkAt(path, "ilk:"+odd.String()); err != nil || !ok || facts.WorkRole != "" {
		t.Fatalf("odd: %+v %v %v", facts, ok, err)
	}
}

// A node's ILK is found by the node's name (its handler_node), with its facts: its tenant, whom it
// works for and in what role; the stage 3 roles read by name, and a capability of every ED of its
// tenant works for nobody. Only a node that works for a person as its conscious is that person's
// conscious (docs/digital-executive-v1-stage-3.md §4).
func TestLookupIlkByNodeAndTheConsciousOfAPerson(t *testing.T) {
	laura := uuid.MustParse("5a5a5a5a-5a5a-45a5-85a5-5a5a5a5a5a5a")
	ana := uuid.MustParse("6b6b6b6b-6b6b-46b6-86b6-6b6b6b6b6b6b")
	acme := uuid.MustParse("7c7c7c7c-7c7c-47c7-87c7-7c7c7c7c7c7c")
	conscious, channel, connector, door, doorForAll, gone := uuid.New(), uuid.New(), uuid.New(), uuid.New(), uuid.New(), uuid.New()
	path := writeIdentityRegion(t, []testIlk{
		{id: laura, kind: 0, name: "Laura", active: true, virtual: true, tenant: acme},
		{id: conscious, kind: 1, name: "AI.ed4qm7k2xa@worker1", node: "AI.ed4qm7k2xa@worker1", active: true, tenant: acme, worksFor: laura, workRole: ilkWorkRoleConscious},
		{id: channel, kind: 1, name: "IO.wapp.ed4qm7k2xa@worker1", node: "IO.wapp.ed4qm7k2xa@worker1", active: true, tenant: acme, worksFor: laura, workRole: ilkWorkRoleChannel},
		{id: connector, kind: 1, name: "IO.crm.ed4qm7k2xa@worker1", node: "IO.crm.ed4qm7k2xa@worker1", active: true, tenant: acme, worksFor: laura, workRole: ilkWorkRoleConnector},
		{id: door, kind: 1, name: "WF.billing.ed4qm7k2xa@worker1", node: "WF.billing.ed4qm7k2xa@worker1", active: true, tenant: acme, worksFor: laura, workRole: ilkWorkRoleCapability},
		{id: doorForAll, kind: 1, node: "WF.billing.acme@worker1", active: true, tenant: acme, workRole: ilkWorkRoleCapability},
		{id: gone, kind: 1, node: "AI.gone@worker1", active: false, tenant: acme, worksFor: laura, workRole: ilkWorkRoleConscious},
	})
	ilk, facts, ok, err := lookupIlkByNodeAt(path, " AI.ed4qm7k2xa@worker1 ")
	want := IlkFacts{Type: "agent", Name: "AI.ed4qm7k2xa@worker1", WorksFor: "ilk:" + laura.String(), WorkRole: "conscious", TenantID: "tnt:" + acme.String()}
	if err != nil || !ok || ilk != "ilk:"+conscious.String() || facts != want {
		t.Fatalf("conscious: %q %+v %v %v", ilk, facts, ok, err)
	}
	for node, role := range map[string]string{
		"IO.crm.ed4qm7k2xa@worker1":     "connector",
		"WF.billing.ed4qm7k2xa@worker1": "capability",
	} {
		if _, facts, ok, err := lookupIlkByNodeAt(path, node); err != nil || !ok || facts.WorkRole != role || facts.WorksFor != "ilk:"+laura.String() {
			t.Fatalf("%s: %+v %v %v", node, facts, ok, err)
		}
	}
	ilk, facts, ok, err = lookupIlkByNodeAt(path, "WF.billing.acme@worker1")
	if err != nil || !ok || ilk != "ilk:"+doorForAll.String() || facts != (IlkFacts{Type: "agent", WorkRole: "capability", TenantID: "tnt:" + acme.String()}) {
		t.Fatalf("a door for every ED: %q %+v %v %v", ilk, facts, ok, err)
	}
	// A person's tenant, which a door of every ED checks against its own.
	if facts, ok, err := lookupIlkAt(path, "ilk:"+laura.String()); err != nil || !ok || facts.TenantID != "tnt:"+acme.String() || !facts.Virtual {
		t.Fatalf("laura: %+v %v %v", facts, ok, err)
	}
	for _, node := range []string{"AI.gone@worker1", "AI.nobody@worker1", "", "ai.ed4qm7k2xa@worker1"} {
		if _, _, ok, err := lookupIlkByNodeAt(path, node); err != nil || ok {
			t.Fatalf("%q: found %v %v", node, ok, err)
		}
	}

	for _, check := range []struct {
		node, person string
		want         bool
	}{
		{"AI.ed4qm7k2xa@worker1", "ilk:" + laura.String(), true},
		{"AI.ed4qm7k2xa@worker1", "ILK:" + strings.ToUpper(laura.String()), true},
		{"AI.ed4qm7k2xa@worker1", "ilk:" + ana.String(), false},
		{"AI.ed4qm7k2xa@worker1", "", false},
		{"IO.wapp.ed4qm7k2xa@worker1", "ilk:" + laura.String(), false},
		{"WF.billing.ed4qm7k2xa@worker1", "ilk:" + laura.String(), false},
		{"AI.gone@worker1", "ilk:" + laura.String(), false},
		{"AI.nobody@worker1", "ilk:" + laura.String(), false},
	} {
		if got, err := isConsciousOfAt(path, check.node, check.person); err != nil || got != check.want {
			t.Fatalf("%s of %s: %v %v, want %v", check.node, check.person, got, err, check.want)
		}
	}
	// No region: nobody is anybody's conscious, and nothing fails.
	if got, err := isConsciousOfAt(filepath.Join(t.TempDir(), "absent"), "AI.ed4qm7k2xa@worker1", "ilk:"+laura.String()); err != nil || got {
		t.Fatalf("absent region: %v %v", got, err)
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
	id       uuid.UUID
	kind     byte
	name     string
	active   bool
	virtual  bool
	worksFor uuid.UUID
	workRole byte
	// node is the entry's handler_node, a node's L2 name; tenant its tenant_id.
	node   string
	tenant uuid.UUID
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
			flags := ilkFlagActive
			if ilk.virtual {
				flags |= ilkFlagVirtual
			}
			binary.LittleEndian.PutUint16(entry[ilkOffFlags:], flags)
		}
		copy(entry[ilkOffDisplayName:ilkOffDisplayName+128], ilk.name)
		copy(entry[ilkOffHandlerNode:ilkOffHandlerNode+128], ilk.node)
		copy(entry[ilkOffTenantID:ilkOffTenantID+16], ilk.tenant[:])
		copy(entry[ilkOffWorksFor:ilkOffWorksFor+16], ilk.worksFor[:])
		entry[ilkOffWorkRole] = ilk.workRole
	}
	path := filepath.Join(t.TempDir(), "jsr-identity-test")
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}
