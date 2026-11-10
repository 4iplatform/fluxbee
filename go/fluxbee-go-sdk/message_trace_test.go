package sdk

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/google/uuid"
)

func traceTestMessage(msgType string, dst Destination, context string) Message {
	msg := Message{
		Routing: Routing{Src: uuid.NewString(), Dst: dst, TTL: 16, TraceID: uuid.NewString()},
		Meta:    Meta{MsgType: msgType},
		Payload: json.RawMessage(`{"type":"text","content":"hello"}`),
	}
	if context != "" {
		msg.Meta.Context = json.RawMessage(context)
	}
	return msg
}

func TestASolutionMessageGetsItsSendersEntryAfterTheOnesItCarries(t *testing.T) {
	carried := `{"io":{"note":"<b>&</b>","big":12345678901234567890},` +
		`"trace":[{"t":"2026-11-04T10:17:03.120Z","from":"IO.slack@motherbee","to":"WF.ventas@worker1"}]}`
	msg := traceTestMessage("user", UnicastDestination("AI.ventas@worker1"), carried)
	WriteTraceEntry(&msg, "WF.ventas@worker1", TraceNote{Act: []string{"forward", "schedule_timer"}})

	entries := TraceEntries(msg.Meta.Context)
	if len(entries) != 2 || entries[0].From != "IO.slack@motherbee" {
		t.Fatalf("entries = %+v", entries)
	}
	entry := entries[1]
	if entry.From != "WF.ventas@worker1" || entry.To != "AI.ventas@worker1" ||
		strings.Join(entry.Act, ",") != "forward,schedule_timer" {
		t.Fatalf("entry = %+v", entry)
	}
	if len(entry.T) != len("2026-11-04T10:17:03.120Z") || !strings.HasSuffix(entry.T, "Z") {
		t.Fatalf("t = %q", entry.T)
	}
	// The context's other keys go out byte for byte: no HTML escapes, no number rounded.
	if !strings.Contains(string(msg.Meta.Context), `"io":{"note":"<b>&</b>","big":12345678901234567890}`) {
		t.Fatalf("context = %s", msg.Meta.Context)
	}
}

func TestAReplyNamesItsRequesterAndAMessageWithNoContextGetsOne(t *testing.T) {
	msg := traceTestMessage("user", UnicastDestination(uuid.NewString()), "")
	WriteTraceEntry(&msg, "AI.ventas@worker1", TraceNote{To: "WF.ventas@worker1", Act: []string{"handover"}})
	entries := TraceEntries(msg.Meta.Context)
	if len(entries) != 1 || entries[0].To != "WF.ventas@worker1" || entries[0].Act[0] != "handover" {
		t.Fatalf("entries = %+v", entries)
	}
}

func TestAUUIDOrNoDestinationLeavesNoToAndABroadcastSaysSo(t *testing.T) {
	cases := []struct {
		dst  Destination
		want string
	}{
		{UnicastDestination(uuid.NewString()), ""},
		{ResolveDestination(), ""},
		{BroadcastDestination(), "broadcast"},
	}
	for _, c := range cases {
		msg := traceTestMessage("user", c.dst, "null")
		WriteTraceEntry(&msg, "WF.ventas@worker1", TraceNote{})
		entries := TraceEntries(msg.Meta.Context)
		if len(entries) != 1 || entries[0].To != c.want {
			t.Fatalf("dst %+v: entries = %+v", c.dst, entries)
		}
	}
}

func TestSystemAdminAndQueryResponsesCarryNoTrace(t *testing.T) {
	for _, kind := range []string{"system", "SYSTEM", "admin", "query_response"} {
		msg := traceTestMessage(kind, UnicastDestination("SY.admin@motherbee"), "")
		WriteTraceEntry(&msg, "WF.ventas@worker1", TraceNote{})
		if msg.Meta.Context != nil {
			t.Fatalf("%s: context = %s", kind, msg.Meta.Context)
		}
	}
	nameless := traceTestMessage("user", BroadcastDestination(), "")
	WriteTraceEntry(&nameless, " ", TraceNote{})
	if nameless.Meta.Context != nil {
		t.Fatalf("a sender with no name: context = %s", nameless.Meta.Context)
	}
}

func TestActWordsAreTrimmedCutDeduplicatedAndAtMostEight(t *testing.T) {
	words := []string{" forward ", "", "forward", strings.Repeat("ñ", 40)}
	for i := 1; i <= 10; i++ {
		words = append(words, "w"+strings.Repeat("x", i))
	}
	kept := traceActWords(words)
	if len(kept) != MaxActWords || kept[0] != "forward" || len([]rune(kept[1])) != MaxActWordChars || kept[2] != "wx" {
		t.Fatalf("kept = %q", kept)
	}
}

func TestAContextThatIsNotAnObjectIsLeftAloneAndABadTraceIsReplaced(t *testing.T) {
	text := traceTestMessage("user", BroadcastDestination(), `"free text"`)
	WriteTraceEntry(&text, "WF.ventas@worker1", TraceNote{})
	if string(text.Meta.Context) != `"free text"` {
		t.Fatalf("context = %s", text.Meta.Context)
	}
	bad := traceTestMessage("user", BroadcastDestination(), `{"trace":"not a list"}`)
	WriteTraceEntry(&bad, "WF.ventas@worker1", TraceNote{})
	if entries := TraceEntries(bad.Meta.Context); len(entries) != 1 {
		t.Fatalf("entries = %+v", entries)
	}
	if TraceOf(json.RawMessage(`{"trace":[]}`)) != nil || TraceOf(nil) != nil {
		t.Fatal("an empty or missing trace is none")
	}
}
