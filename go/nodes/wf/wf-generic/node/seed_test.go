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

// Archi learns to write a WF from its handbook (docs/archi/handbook_fluxbee.md §4.2) and from one
// example in its cookbook seeds, sales.consult: an agent that answers the person or consults the
// supervisors, asked with a menu. The example runs here on the engine, as Archi would deploy it.

const (
	seedPersonIO    = "IO.slack.ventas@motherbee"
	seedSales       = "AI.sales@worker1"
	seedSupervisors = "IO.slack.supervisores@motherbee"
	seedWFNode      = "WF.sales-consult@motherbee"
	seedThread      = "thread:seed-1"
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

// say is text from sender, the person's IO node or the supervisors', on the conversation's thread:
// io.slack gives a supervisor's reply in the thread of the WF's post the thread it recorded for it.
func (c *seedConversation) say(sender, text string) {
	c.t.Helper()
	c.turn++
	thread := seedThread
	c.dispatch(from(sender, sdk.Message{
		Routing: sdk.Routing{Src: "io-uuid", TraceID: fmt.Sprintf("trace-seed-%d", c.turn)},
		Meta:    sdk.Meta{MsgType: "user", ThreadID: &thread},
		Payload: json.RawMessage(fmt.Sprintf(`{"type":"text","content":%q}`, text)),
	}))
}

// asked is the last message sales got, with the names of its menu's options.
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

// choose is sales' answer to request: reply, and its evaluation.
func (c *seedConversation) choose(request sdk.Message, reply, option string, params map[string]any) {
	c.t.Helper()
	if params == nil {
		params = map[string]any{}
	}
	evaluation := map[string]any{"reply": reply, "option": option, "params": params}
	c.dispatch(agentAnswer(c.t, seedSales, request, fmt.Sprintf(`{"type":"text","content":%q}`, reply), evaluation))
}

// to is what node got, the payloads' content in order.
func (c *seedConversation) to(node string) []string {
	var got []string
	for _, msg := range c.disp.sent {
		if msg.Routing.Dst.Value() != node {
			continue
		}
		var payload map[string]any
		_ = json.Unmarshal(msg.Payload, &payload)
		got = append(got, fmt.Sprint(payload["content"]))
	}
	return got
}

// person checks what the person got since the count was since: want, in order.
func (c *seedConversation) person(since int, want ...string) {
	c.t.Helper()
	got := c.to(seedPersonIO)
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
	if _, ok := c.timer.pending[ref]; !ok {
		c.t.Fatalf("%s is not pending: %v", key, c.timer.pending)
	}
	delete(c.timer.pending, ref)
	c.dispatch(timerFiredFor(c.t, seedWFNode, instanceID, key))
}

// The seeds' sales.consult on the engine: sales' answers reach the person in whatever state they
// arrive, two at a time included; a consultation posts the question and waits; a supervisor's
// message that decides nothing (wait) never reaches the person, a decision does, and so does one
// that comes after the wait; an empty reply is never forwarded; an error gets the person an apology;
// the wait and the conversation end on their timers.
func TestTheSeedExampleConsultsTheSupervisors(t *testing.T) {
	c := newSeedConversation(t)

	// 1. The person writes: sales gets the message with the menu, and its answer reaches the person.
	c.say(seedPersonIO, "Hola, ¿cuánto sale el plan Pro?")
	id := c.inState("talking")
	first, options := c.asked()
	if options != "answer consult " {
		t.Fatalf("sales' menu: %q", options)
	}
	c.choose(first, "100 dólares por licencia por mes.", "answer", nil)
	c.person(0, "100 dólares por licencia por mes.")
	c.inState("talking")

	// 2. The person writes twice before sales answers: both answers reach the person.
	c.say(seedPersonIO, "¿Y el Basic?")
	one, _ := c.asked()
	c.say(seedPersonIO, "¿Hay descuento por volumen?")
	two, _ := c.asked()
	c.choose(one, "40 dólares.", "answer", nil)
	c.choose(two, "Desde 20 licencias, consultame.", "answer", nil)
	c.person(1, "40 dólares.", "Desde 20 licencias, consultame.")

	// 3. Sales consults: its reply reaches the person, the question the supervisors.
	c.say(seedPersonIO, "Quiero 20 Pro con 20% de descuento")
	ask, _ := c.asked()
	c.choose(ask, "Lo consulto y te aviso.", "consult", map[string]any{"question": "¿20% en 20 Pro?"})
	c.inState("consulting")
	c.person(3, "Lo consulto y te aviso.")
	if got := c.to(seedSupervisors); fmt.Sprint(got) != "[¿20% en 20 Pro?]" {
		t.Fatalf("the supervisors get the question: %q", got)
	}

	// 4. A supervisor's message that decides nothing: sales says wait, and the person gets nothing.
	c.say(seedSupervisors, "@ana, ¿este cliente pagó tarde?")
	remark, options := c.asked()
	if options != "decided wait " || fmt.Sprint(wfBlock(t, remark)["vars"]) != "map[question:¿20% en 20 Pro?]" {
		t.Fatalf("sales gets the supervisor's message, with the question: %q %v", options, wfBlock(t, remark)["vars"])
	}
	c.choose(remark, "", "wait", nil)
	c.inState("consulting")
	c.person(4)

	// 5. The person writes, and the supervisor decides before sales answers it: the decision and the
	// answer both reach the person, in the order sales gives them.
	c.say(seedPersonIO, "¿Novedades?")
	meanwhile, options := c.asked()
	if options != "answer " {
		t.Fatalf("the person's message while consulting: %q", options)
	}
	c.say(seedSupervisors, "Aprobado hasta 15%.")
	decision, _ := c.asked()
	c.choose(decision, "Te aprobaron 15% de descuento.", "decided", nil)
	c.inState("talking")
	c.choose(meanwhile, "Todavía no, ya te aviso.", "answer", nil)
	c.person(4, "Te aprobaron 15% de descuento.", "Todavía no, ya te aviso.")

	// 6. A supervisor writes after the decision: sales gets it, and its decision reaches the person.
	c.say(seedSupervisors, "Corrijo: 12%.")
	late, options := c.asked()
	if options != "decided wait " {
		t.Fatalf("a late supervisor's message: %q", options)
	}
	c.choose(late, "Corrección: el descuento aprobado es 12%.", "decided", nil)
	c.inState("talking")
	c.person(6, "Corrección: el descuento aprobado es 12%.")

	// 7. An error: the person gets the WF's apology.
	c.say(seedPersonIO, "¿Me mandás el link de pago?")
	broken, _ := c.asked()
	c.dispatch(agentAnswer(t, seedSales, broken, `{"type":"error","code":"invalid_structured_output","message":"not_json"}`, nil))
	c.person(7, "Sorry, something went wrong on our side. Could you write that again?")

	// 8. A consult with an empty reply that no supervisor decides: the person gets nothing then, and
	// sales tells them when the wait ends.
	c.say(seedPersonIO, "¿Y 25% en 40?")
	quiet, _ := c.asked()
	c.choose(quiet, "", "consult", map[string]any{"question": "¿25% en 40 Pro?"})
	c.inState("consulting")
	c.person(8)
	c.fire(id, "consult_timeout")
	c.inState("talking")
	timeout, options := c.asked()
	if derefString(timeout.Meta.Msg) != "CONSULT_TIMEOUT" || options != "answer " {
		t.Fatalf("sales hears that no supervisor decided: %+v %q", timeout.Meta, options)
	}
	c.choose(timeout, "Todavía no me confirmaron el 25%.", "answer", nil)
	c.person(8, "Todavía no me confirmaron el 25%.")

	// 9. The conversation closes after its idle time.
	c.fire(id, "idle")
	if _, ok := c.rt.Registry.GetByThread(seedThread); ok {
		t.Fatal("the instance closes on idle")
	}
	if row := mustGetInstance(t, c.rt, id); row.CurrentState != "closed" {
		t.Fatalf("the instance: %+v", row)
	}
}
