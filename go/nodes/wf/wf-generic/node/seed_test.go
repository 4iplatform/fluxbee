package node

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// Archi learns to write a WF from its handbook (docs/archi/handbook_fluxbee.md §4.2), one small case
// per mechanism, and from one example in its cookbook seeds, sales.chat: the basic loop, where a
// person's messages go to an agent with a menu and its answers go back to the person. The example
// runs here on the engine, as Archi would deploy it.

const (
	seedPersonIO = "IO.slack.ventas@motherbee"
	seedSales    = "AI.sales@worker1"
	seedWFNode   = "WF.sales.chat@motherbee"
	seedThread   = "thread:seed-1"
)

// seedDefinition is the WF definition of a cookbook seed's entry, pattern_key, at path in the shape
// the entry keeps it.
func seedDefinition(t *testing.T, cookbook, patternKey string, path ...string) string {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "..", "..", "seeds", "cookbook", cookbook))
	if err != nil {
		t.Fatal(err)
	}
	var entries []map[string]any
	if err := json.Unmarshal(raw, &entries); err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if entry["pattern_key"] != patternKey {
			continue
		}
		var value any = entry
		for _, key := range path {
			switch node := value.(type) {
			case map[string]any:
				value = node[key]
			case []any:
				value = node[0]
				if m, ok := value.(map[string]any); ok {
					value = m[key]
				}
			}
		}
		data, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		return string(data)
	}
	t.Fatalf("%s has no %s", cookbook, patternKey)
	return ""
}

type seedConversation struct {
	t     *testing.T
	timer *fakeSYTimer
	rt    *NodeRuntime
	disp  *mockDispatcher
	turn  int
}

func newSeedConversation(t *testing.T) *seedConversation {
	t.Helper()
	design := seedDefinition(t, "design_cookbook_v1.json", "wf_node_with_workflow", "successful_shape", "desired_state", "wf_deployments", "definition")
	artifact := seedDefinition(t, "artifact_cookbook_v1.json", "workflow_definition", "successful_shape", "artifact", "definition")
	if design != artifact {
		t.Fatal("the design and artifact seeds hold different WF examples")
	}
	c := &seedConversation{t: t, timer: &fakeSYTimer{owner: seedWFNode, pending: map[string]time.Time{}}}
	c.rt, c.disp = newTestRuntime(t, "", design, c.timer)
	c.rt.NodeName, c.disp.l2name = seedWFNode, seedWFNode
	if err := c.rt.Recover(context.Background(), RecoverOptions{OwnerL2Name: seedWFNode}); err != nil {
		t.Fatal(err)
	}
	return c
}

func (c *seedConversation) dispatch(msg sdk.Message) {
	c.t.Helper()
	if err := Dispatch(context.Background(), msg, c.rt); err != nil {
		c.t.Fatal(err)
	}
}

// say is the person's text, from their IO node, on the conversation's thread.
func (c *seedConversation) say(text string) {
	c.t.Helper()
	c.turn++
	thread := seedThread
	c.dispatch(from(seedPersonIO, sdk.Message{
		Routing: sdk.Routing{Src: "io-uuid", TraceID: fmt.Sprintf("trace-seed-%d", c.turn)},
		Meta:    sdk.Meta{MsgType: "user", ThreadID: &thread},
		Payload: json.RawMessage(fmt.Sprintf(`{"type":"text","content":%q}`, text)),
	}))
}

// asked is the last message sales got, and the names of its menu's options.
func (c *seedConversation) asked() (sdk.Message, string) {
	c.t.Helper()
	msg := sentTo(c.t, c.disp, seedSales)
	menu, _ := wfBlock(c.t, msg)["menu"].(map[string]any)
	options, _ := menu["options"].([]any)
	names := ""
	for _, option := range options {
		names += fmt.Sprint(option.(map[string]any)["name"]) + " "
	}
	return msg, names
}

// answer is sales' answer to request: reply, with the option answer.
func (c *seedConversation) answer(request sdk.Message, reply string) {
	c.t.Helper()
	evaluation := map[string]any{"reply": reply, "option": "answer", "params": map[string]any{}}
	c.dispatch(agentAnswer(c.t, seedSales, request, fmt.Sprintf(`{"type":"text","content":%q}`, reply), evaluation))
}

// person checks what the person got since the count was since: want, in order.
func (c *seedConversation) person(since int, want ...string) {
	c.t.Helper()
	var got []string
	for _, msg := range c.disp.sent {
		if msg.Routing.Dst.Value() != seedPersonIO {
			continue
		}
		var payload map[string]any
		_ = json.Unmarshal(msg.Payload, &payload)
		got = append(got, fmt.Sprint(payload["content"]))
	}
	if fmt.Sprint(got[since:]) != fmt.Sprint(want) {
		c.t.Fatalf("the person got %q, want %q", got[since:], want)
	}
}

func (c *seedConversation) inState(state string) string {
	c.t.Helper()
	inst, ok := c.rt.Registry.GetByThread(seedThread)
	if !ok {
		c.t.Fatalf("no instance runs on %s", seedThread)
	}
	row := mustGetInstance(c.t, c.rt, inst.InstanceID)
	if row.CurrentState != state {
		c.t.Fatalf("the instance is in %s, want %s", row.CurrentState, state)
	}
	return row.InstanceID
}

func (c *seedConversation) fire(instanceID, key string) {
	c.t.Helper()
	ref := timerClientRef(instanceID, key)
	at, ok := c.timer.pending[ref]
	if !ok {
		c.t.Fatalf("%s is not pending: %v", key, c.timer.pending)
	}
	delete(c.timer.pending, ref)
	c.dispatch(timerFiredFor(c.t, seedWFNode, instanceID, key, at))
}

// The seeds' sales.chat on the engine: each of the person's messages reaches sales with the menu
// answer, the first one included; sales' answers reach the person in whatever order they come, two at
// a time included; an empty reply is never forwarded; an error gets the person an apology; the
// conversation closes on its quiet time.
func TestTheSeedExampleIsTheBasicLoop(t *testing.T) {
	c := newSeedConversation(t)

	// 1. The person writes: sales gets the message with the menu, and its answer reaches the person.
	c.say("Hola, ¿cuánto sale el plan Pro?")
	id := c.inState("talking")
	first, options := c.asked()
	if options != "answer " {
		t.Fatalf("sales' menu: %q", options)
	}
	var firstPayload map[string]any
	_ = json.Unmarshal(first.Payload, &firstPayload)
	if firstPayload["content"] != "Hola, ¿cuánto sale el plan Pro?" {
		t.Fatalf("sales gets the person's message as it came: %v", firstPayload)
	}
	c.answer(first, "100 dólares por licencia por mes.")
	c.person(0, "100 dólares por licencia por mes.")
	c.inState("talking")

	// 2. The person writes twice before sales answers: both answers reach the person.
	c.say("¿Y el Basic?")
	one, _ := c.asked()
	c.say("¿Hay descuento por volumen?")
	two, options := c.asked()
	if options != "answer " {
		t.Fatalf("sales' menu: %q", options)
	}
	c.answer(two, "Desde 20 licencias, consultame.")
	c.answer(one, "40 dólares.")
	c.person(1, "Desde 20 licencias, consultame.", "40 dólares.")

	// 3. An empty reply reaches no one.
	c.say("Gracias")
	thanks, _ := c.asked()
	c.answer(thanks, "")
	c.person(3)

	// 4. An error: the person gets the WF's apology.
	c.say("¿Me mandás el link de pago?")
	broken, _ := c.asked()
	c.dispatch(agentAnswer(t, seedSales, broken, `{"type":"error","code":"invalid_structured_output","message":"not_json"}`, nil))
	c.person(3, "Sorry, something went wrong on our side. Could you write that again?")
	c.inState("talking")

	// 5. The conversation closes after its quiet time.
	c.fire(id, "idle")
	if _, ok := c.rt.Registry.GetByThread(seedThread); ok {
		t.Fatal("the instance closes on idle")
	}
	if row := mustGetInstance(t, c.rt, id); row.CurrentState != "closed" {
		t.Fatalf("the instance: %+v", row)
	}
}
