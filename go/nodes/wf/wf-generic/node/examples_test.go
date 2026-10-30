package node

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

func exampleDefinition(t *testing.T, pkg string) string {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "examples", "packages", pkg, "flow", "definition.json"))
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func lastSent(t *testing.T, disp *mockDispatcher) (sdk.Message, map[string]any) {
	t.Helper()
	if len(disp.sent) == 0 {
		t.Fatal("nothing sent")
	}
	msg := disp.sent[len(disp.sent)-1]
	var payload map[string]any
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	return msg, payload
}

// agentAnswer is an agent's answer to request, as ai-generic builds it: the request's msg (with
// ".error" for an error, when the request had one) and thread, and the request's context without
// its thread_id, src_ilk, response_envelope, wf and evaluation (sanitize_reply_context); an answer
// to a menu carries its evaluation, {reply, option, params}, in meta.context.evaluation.
func agentAnswer(t *testing.T, agent string, request sdk.Message, payload string, evaluation map[string]any) sdk.Message {
	t.Helper()
	answerContext := map[string]any{}
	if len(request.Meta.Context) > 0 {
		if err := json.Unmarshal(request.Meta.Context, &answerContext); err != nil {
			t.Fatal(err)
		}
	}
	for _, key := range []string{"thread_id", "src_ilk", "response_envelope", "wf", "evaluation"} {
		delete(answerContext, key)
	}
	if evaluation != nil {
		answerContext["evaluation"] = evaluation
	}
	rawContext, err := json.Marshal(answerContext)
	if err != nil {
		t.Fatal(err)
	}
	msgName := request.Meta.Msg
	if msgName != nil && strings.Contains(payload, `"type":"error"`) {
		errorName := *msgName + ".error"
		msgName = &errorName
	}
	return from(agent, sdk.Message{
		Routing: sdk.Routing{Src: "agent-uuid", TraceID: request.Routing.TraceID},
		Meta:    sdk.Meta{MsgType: "user", Msg: msgName, ThreadID: request.Meta.ThreadID, Context: rawContext},
		Payload: json.RawMessage(payload),
	})
}

// sentTo is the last message rt sent to node.
func sentTo(t *testing.T, disp *mockDispatcher, node string) sdk.Message {
	t.Helper()
	for i := len(disp.sent) - 1; i >= 0; i-- {
		if disp.sent[i].Routing.Dst.Value() == node {
			return disp.sent[i]
		}
	}
	t.Fatalf("nothing sent to %s", node)
	return sdk.Message{}
}

// wfBlock is the WF's block of a message's meta.context.
func wfBlock(t *testing.T, msg sdk.Message) map[string]any {
	t.Helper()
	var metaContext map[string]any
	if err := json.Unmarshal(msg.Meta.Context, &metaContext); err != nil {
		t.Fatalf("meta.context %s: %v", msg.Meta.Context, err)
	}
	block, _ := metaContext["wf"].(map[string]any)
	return block
}

// The wf.router example (0.1.91): it forwards a person's message as it came to the host, and the
// specialist when the host escalates, and the final answer as it came to the IO node the question
// came from (a node name written in the definition, one transition per IO node); the answer keeps
// the message's reply context. An agent's error answer is a payload of type error, and its timeouts
// fire on event.timer_key. A person's second message while an agent answers goes to that agent. An IO
// node it does not start from starts nothing.
func TestTheRouterExampleRepliesToTheIONodeTheQuestionCameFrom(t *testing.T) {
	rt, disp := newTestRuntime(t, "", exampleDefinition(t, "wf.router"), nil)
	const question = `{"type":"text","content":"where is my order?"}`
	ask := func(io string) (string, sdk.Message) {
		t.Helper()
		thread := "conv-" + io
		relay := from(io, sdk.Message{Routing: sdk.Routing{TraceID: "trace-" + io}, Meta: sdk.Meta{MsgType: "user", ThreadID: &thread}, Payload: json.RawMessage(question)})
		relay.Meta.Context = json.RawMessage(`{"io":{"reply_target":{"kind":"api","address":"conv-1"}}}`)
		instanceID := startInstance(t, rt, relay)
		msg := sentTo(t, disp, "AI.host@motherbee")
		if msg.Meta.MsgType != "user" || msg.Meta.Msg != nil || string(msg.Payload) != question || derefString(msg.Meta.ThreadID) != thread {
			t.Fatalf("the host gets the message as it came, on its thread: %+v %s", msg.Meta, msg.Payload)
		}
		return instanceID, msg
	}
	failure := func(instanceID string) any {
		t.Helper()
		row := mustGetInstance(t, rt, instanceID)
		if row.Status != "failed" {
			t.Fatalf("%s: %+v", instanceID, row)
		}
		state, _ := unmarshalJSON(row.StateJSON)
		return state["failure_reason"]
	}
	dispatch := func(msg sdk.Message) {
		t.Helper()
		if err := Dispatch(context.Background(), msg, rt); err != nil {
			t.Fatal(err)
		}
	}

	viaAPI, request := ask("IO.api.support@motherbee")
	// A second message while the host answers goes to the host too, and the instance waits on.
	thread := "conv-IO.api.support@motherbee"
	again := from("IO.api.support@motherbee", sdk.Message{Routing: sdk.Routing{TraceID: "trace-again"}, Meta: sdk.Meta{MsgType: "user", ThreadID: &thread}, Payload: json.RawMessage(`{"type":"text","content":"hello?"}`)})
	dispatch(again)
	if msg := sentTo(t, disp, "AI.host@motherbee"); string(msg.Payload) != `{"type":"text","content":"hello?"}` || mustGetInstance(t, rt, viaAPI).CurrentState != "asking_host" {
		t.Fatalf("the second message goes to the host: %s", msg.Payload)
	}
	dispatch(agentAnswer(t, "AI.host@motherbee", request, `{"type":"text","content":"the answer"}`, nil))
	msg := sentTo(t, disp, "IO.api.support@motherbee")
	if string(msg.Payload) != `{"type":"text","content":"the answer"}` ||
		string(msg.Meta.Context) != `{"io":{"reply_target":{"address":"conv-1","kind":"api"}},"wf":{"trail":["AI.host@motherbee","WF.invoice@motherbee"]}}` {
		t.Fatalf("the answer goes back to IO.api as it came, with its reply context: %s %s", msg.Payload, msg.Meta.Context)
	}
	if row := mustGetInstance(t, rt, viaAPI); row.CurrentState != "answered" {
		t.Fatalf("%+v", row)
	}

	viaWhatsApp, request := ask("IO.wapp.acme@motherbee")
	dispatch(agentAnswer(t, "AI.host@motherbee", request, `{"type":"text","content":"ROUTE_SPECIALIST"}`, nil))
	escalated := sentTo(t, disp, "AI.specialist@motherbee")
	if string(escalated.Payload) != question || fmt.Sprint(wfBlock(t, escalated)["trail"]) != "[IO.wapp.acme@motherbee WF.invoice@motherbee]" {
		t.Fatalf("the host escalates the question as it came: %s %s", escalated.Payload, escalated.Meta.Context)
	}
	dispatch(agentAnswer(t, "AI.specialist@motherbee", escalated, `{"type":"text","content":"the specialist's answer"}`, nil))
	if msg, payload := lastSent(t, disp); msg.Routing.Dst.Value() != "IO.wapp.acme@motherbee" || payload["content"] != "the specialist's answer" {
		t.Fatalf("the specialist's answer goes back to IO.wapp: %+v %v", msg, payload)
	}
	if row := mustGetInstance(t, rt, viaWhatsApp); row.CurrentState != "answered" {
		t.Fatalf("%+v", row)
	}

	errored, request := ask("IO.api.support@motherbee")
	dispatch(agentAnswer(t, "AI.host@motherbee", request, `{"type":"error","code":"rate_limited","message":"slow down","retryable":true}`, nil))
	if reason := failure(errored); reason != "host_error:rate_limited" {
		t.Fatalf("an agent's error is a payload of type error: %v", reason)
	}

	timedOut, _ := ask("IO.api.support@motherbee")
	if err := Dispatch(context.Background(), realTimerFired(t, timedOut, "host_timeout"), rt); err != nil {
		t.Fatal(err)
	}
	if reason := failure(timedOut); reason != "host_timeout" {
		t.Fatalf("the timeout fires on event.timer_key: %v", reason)
	}

	sent := len(disp.sent)
	stranger := from("IO.slack.ops@motherbee", sdk.Message{Routing: sdk.Routing{TraceID: "trace-slack"}, Meta: sdk.Meta{MsgType: "user"}, Payload: json.RawMessage(question)})
	dispatch(stranger)
	if rows, _ := rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 4 || len(disp.sent) != sent {
		t.Fatalf("an IO node not written in the definition starts nothing: %d instances, %d sends", len(rows), len(disp.sent)-sent)
	}
}

// The wf.invoice example (0.1.90): a missing currency takes its default from the $ref (the engine
// never applied the JSON-Schema default it used to rely on), and its timeouts fire on
// event.timer_key.
func TestTheInvoiceExampleDefaultsTheCurrencyAndTimesOut(t *testing.T) {
	rt, disp := newTestRuntime(t, "", exampleDefinition(t, "wf.invoice"), nil)
	instanceID := startInstance(t, rt, from("IO.api.billing@motherbee", msgWithNameAndPayload("INVOICE_START", map[string]any{"customer_id": "c1", "amount_cents": 2500})))
	msg, payload := lastSent(t, disp)
	if msg.Routing.Dst.Value() != "IO.quickbooks@motherbee" || payload["currency"] != "USD" {
		t.Fatalf("the request: %+v %v", msg, payload)
	}
	// An integer goes on the wire as an integer: a Rust u64 field refuses 2500.0.
	if !strings.Contains(string(msg.Payload), `"amount_cents":2500,`) && !strings.Contains(string(msg.Payload), `"amount_cents":2500}`) {
		t.Fatalf("amount_cents on the wire: %s", msg.Payload)
	}

	if err := Dispatch(context.Background(), realTimerFired(t, instanceID, "collection_timeout"), rt); err != nil {
		t.Fatal(err)
	}
	if row := mustGetInstance(t, rt, instanceID); row.CurrentState != "failed" || row.Status != "failed" {
		t.Fatalf("the collection timeout fires: %+v", row)
	}
	if msg, payload := lastSent(t, disp); msg.Routing.Dst.Value() != "IO.notifications@motherbee" || payload["failure_reason"] != "collection_timeout" {
		t.Fatalf("the failure is notified: %+v %v", msg, payload)
	}
}

// fakeSYTimer is SY.timer as the WF sees it: it keeps the WF's pending timers by client_ref, at the
// time they fire, and lists them.
type fakeSYTimer struct {
	owner   string
	pending map[string]time.Time
}

func (f *fakeSYTimer) ScheduleIn(_ context.Context, d time.Duration, opts sdk.ScheduleOptions) (sdk.TimerID, error) {
	f.pending[opts.ClientRef] = fixedClock().Add(d)
	return "", nil
}
func (f *fakeSYTimer) Schedule(_ context.Context, fireAt time.Time, opts sdk.ScheduleOptions) (sdk.TimerID, error) {
	f.pending[opts.ClientRef] = fireAt
	return "", nil
}
func (f *fakeSYTimer) CancelByClientRef(_ context.Context, ref string) error {
	delete(f.pending, ref)
	return nil
}
func (f *fakeSYTimer) CancelByClientRefConfirmed(ctx context.Context, ref string) error {
	return f.CancelByClientRef(ctx, ref)
}
func (f *fakeSYTimer) RescheduleByClientRef(_ context.Context, ref string, newFireAt time.Time) error {
	f.pending[ref] = newFireAt
	return nil
}
func (f *fakeSYTimer) List(_ context.Context, _ sdk.ListFilter) ([]sdk.TimerInfo, error) {
	var timers []sdk.TimerInfo
	for ref, fireAt := range f.pending {
		timers = append(timers, timerInfo(ref, fireAt.UnixMilli()))
	}
	return timers, nil
}

// timerFiredFor is SY.timer's TIMER_FIRED of the timer node scheduled for instanceID, as SY.timer of
// node's hive sends it.
func timerFiredFor(t *testing.T, node, instanceID, timerKey string) sdk.Message {
	t.Helper()
	fired := realTimerFired(t, instanceID, timerKey)
	var payload map[string]any
	if err := json.Unmarshal(fired.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	payload["owner_l2_name"] = node
	var err error
	if fired.Payload, err = json.Marshal(payload); err != nil {
		t.Fatal(err)
	}
	timerNode := timerAuthority(node)
	fired.Routing.SrcL2Name = &timerNode
	fired.Routing.Dst = sdk.UnicastDestination(node)
	return fired
}

// salesConversation drives the wf.sales.invoice example on the engine for a test, playing the IO
// node, the agents and SY.timer of worker1, on one conversation's thread.
type salesConversation struct {
	t          *testing.T
	path       string
	definition string
	timer      *fakeSYTimer
	rt         *NodeRuntime
	disp       *mockDispatcher
	turn       int
}

const (
	salesIO        = "IO.api.sales@worker1"
	salesAgent     = "AI.sales@worker1"
	invoiceAgent   = "AI.invoice@worker1"
	paymentsAgent  = "AI.payments@worker1"
	salesWFNode    = "WF.sales-invoice@worker1"
	salesThread    = "conv-7"
	salesIOContext = `{"io":{"reply_target":{"address":"conv-7","kind":"api"}}}`
	personILK      = "ilk:00000000-0000-4000-8000-0000000000aa"
	personICH      = "ich:api:conv-7"
	// supervisorsIO is the supervisors' Slack channel, and supervisorsIOContext the context.io of a
	// supervisor's answer there.
	supervisorsIO        = "IO.slack.supervisores@motherbee"
	supervisorsIOContext = `{"io":{"reply_target":{"address":"C0SUPERVISORS","kind":"slack_post","params":{"thread_ts":"1730289000.000100"}}}}`
)

func newSalesConversation(t *testing.T) *salesConversation {
	c := &salesConversation{
		t:          t,
		path:       filepath.Join(t.TempDir(), "wf.db"),
		definition: exampleDefinition(t, "wf.sales.invoice"),
		timer:      &fakeSYTimer{owner: salesWFNode, pending: map[string]time.Time{}},
	}
	c.start()
	return c
}

// start starts the WF node, again after a first time: a restart, on the same store and SY.timer.
func (c *salesConversation) start() {
	c.t.Helper()
	c.rt, c.disp = newTestRuntime(c.t, c.path, c.definition, c.timer)
	c.rt.NodeName, c.disp.l2name = salesWFNode, salesWFNode
	if err := c.rt.Recover(context.Background(), RecoverOptions{OwnerL2Name: salesWFNode}); err != nil {
		c.t.Fatal(err)
	}
}

func (c *salesConversation) dispatch(msg sdk.Message) {
	c.t.Helper()
	if err := Dispatch(context.Background(), msg, c.rt); err != nil {
		c.t.Fatal(err)
	}
}

// person writes text on the conversation, through the IO node.
func (c *salesConversation) person(text string) sdk.Message {
	c.t.Helper()
	c.turn++
	thread, ilk, ich := salesThread, personILK, personICH
	msg := from(salesIO, sdk.Message{
		Routing: sdk.Routing{Src: "io-uuid", TraceID: fmt.Sprintf("trace-person-%d", c.turn)},
		Meta:    sdk.Meta{MsgType: "user", SrcIlk: &ilk, Ich: &ich, ThreadID: &thread, Context: json.RawMessage(salesIOContext)},
		Payload: json.RawMessage(fmt.Sprintf(`{"type":"text","content":%q}`, text)),
	})
	c.dispatch(msg)
	return msg
}

// choose is agent's answer to request's menu: reply as its text, and its evaluation, {reply, option,
// params}, in meta.context.evaluation.
func (c *salesConversation) choose(agent string, request sdk.Message, reply, option string, params map[string]any) {
	c.t.Helper()
	if params == nil {
		params = map[string]any{}
	}
	evaluation := map[string]any{"reply": reply, "option": option, "params": params}
	c.dispatch(agentAnswer(c.t, agent, request, fmt.Sprintf(`{"type":"text","content":%q}`, reply), evaluation))
}

// fail is agent's error answer to request.
func (c *salesConversation) fail(agent string, request sdk.Message) {
	c.t.Helper()
	c.dispatch(agentAnswer(c.t, agent, request, `{"type":"error","code":"invalid_output","message":"the answer broke the contract"}`, nil))
}

// supervisor answers in Slack, in the thread of the WF's post: IO.slack.supervisores gives the answer
// the thread it recorded for that post, the conversation's (wf-orchestration-v1 §5.3.4), and the
// context.io of its own channel.
func (c *salesConversation) supervisor(text string) sdk.Message {
	c.t.Helper()
	c.turn++
	thread := salesThread
	msg := from(supervisorsIO, sdk.Message{
		Routing: sdk.Routing{Src: "slack-uuid", TraceID: fmt.Sprintf("trace-supervisor-%d", c.turn)},
		Meta:    sdk.Meta{MsgType: "user", ThreadID: &thread, Context: json.RawMessage(supervisorsIOContext)},
		Payload: json.RawMessage(fmt.Sprintf(`{"type":"text","content":%q}`, text)),
	})
	c.dispatch(msg)
	return msg
}

// fire fires the instance's pending timer key, as SY.timer does.
func (c *salesConversation) fire(key string) {
	c.t.Helper()
	instanceID := c.running().InstanceID
	ref := timerClientRef(instanceID, key)
	if _, ok := c.timer.pending[ref]; !ok {
		c.t.Fatalf("%s is not pending: %v", key, c.timer.pending)
	}
	delete(c.timer.pending, ref)
	c.dispatch(timerFiredFor(c.t, salesWFNode, instanceID, key))
}

func (c *salesConversation) pending(key string) (time.Time, bool) {
	at, ok := c.timer.pending[timerClientRef(c.running().InstanceID, key)]
	return at, ok
}

func (c *salesConversation) running() WFInstanceRow {
	c.t.Helper()
	inst, ok := c.rt.Registry.GetByThread(salesThread)
	if !ok {
		c.t.Fatalf("no instance runs on %s", salesThread)
	}
	return mustGetInstance(c.t, c.rt, inst.InstanceID)
}

func (c *salesConversation) inState(state string) WFInstanceRow {
	c.t.Helper()
	row := c.running()
	if row.CurrentState != state {
		c.t.Fatalf("the instance is in %s, want %s", row.CurrentState, state)
	}
	return row
}

func (c *salesConversation) state() map[string]any {
	c.t.Helper()
	state, err := unmarshalJSON(c.running().StateJSON)
	if err != nil {
		c.t.Fatal(err)
	}
	return state
}

// logged reports whether the running instance's log has an entry with summary.
func (c *salesConversation) logged(summary string) bool {
	c.t.Helper()
	logs, err := c.rt.Store.GetRecentLog(context.Background(), c.running().InstanceID, 200)
	if err != nil {
		c.t.Fatal(err)
	}
	for _, entry := range logs {
		if entry.Summary == summary {
			return true
		}
	}
	return false
}

// sentTo is the last message the WF sent to node since it started.
func (c *salesConversation) sentTo(node string) sdk.Message {
	c.t.Helper()
	return sentTo(c.t, c.disp, node)
}

// count is how many messages the WF sent to node since it started.
func (c *salesConversation) count(node string) int {
	count := 0
	for _, msg := range c.disp.sent {
		if msg.Routing.Dst.Value() == node {
			count++
		}
	}
	return count
}

func (c *salesConversation) trail(msg sdk.Message) string {
	c.t.Helper()
	return fmt.Sprint(wfBlock(c.t, msg)["trail"])
}

// options names the options of the menu a message offers, in its WF's block.
func (c *salesConversation) options(msg sdk.Message) string {
	c.t.Helper()
	menu, _ := wfBlock(c.t, msg)["menu"].(map[string]any)
	options, _ := menu["options"].([]any)
	var names []string
	for _, option := range options {
		names = append(names, fmt.Sprint(option.(map[string]any)["name"]))
	}
	return strings.Join(names, " ")
}

// delivered checks the last message the person got: reply, from agent through the WF, with the
// person's context.io.
func (c *salesConversation) delivered(reply, agent string) {
	c.t.Helper()
	msg := c.sentTo(salesIO)
	var metaContext map[string]any
	_ = json.Unmarshal(msg.Meta.Context, &metaContext)
	if string(msg.Payload) != fmt.Sprintf(`{"type":"text","content":%q}`, reply) || c.trail(msg) != "["+agent+" "+salesWFNode+"]" ||
		fmt.Sprint(metaContext["io"]) != "map[reply_target:map[address:conv-7 kind:api]]" || derefString(msg.Meta.ThreadID) != salesThread {
		c.t.Fatalf("the person gets %q from %s, with its trail and context.io: %s %s", reply, agent, msg.Payload, msg.Meta.Context)
	}
}

// salesOrder is the params of sales' option invoice, and issuedInvoice those of invoice's option
// issued.
func salesOrder() map[string]any {
	return map[string]any{"product": "Pro", "quantity": 10, "unit_price": 100}
}

func issuedInvoice(number, dueIn string) map[string]any {
	return map[string]any{"invoice_number": number, "amount": 1000, "due_in": dueIn}
}

// Case 1 (wf-orchestration-v1 §5.2), on Case 3's menus (§5.3): the whole circuit of the
// wf.sales.invoice example on the engine, with the agents and SY.timer played by the test. A
// conversation enters through sales, which chooses invoice, and is handed to invoice, which issues an
// invoice and hands the conversation back without sales being asked again; the due date fires while
// sales answers and is acted on when sales is done, through payments; the instance closes after its
// idle time. A stranger on the conversation's thread is refused, an answer after the close is
// dropped, and the person's next message starts a new instance at sales. A restart in the middle
// keeps the conversation, the message it kept included.
func TestTheSalesInvoiceExampleRunsTheOperatorsCircuit(t *testing.T) {
	c := newSalesConversation(t)

	// 1. The person writes: a new instance on the conversation's thread, at sales, which gets the
	// message as it came (the payload, the person's ILK, the channel), with its menu and the trail.
	first := c.person("Hola, quiero cotizar 10 licencias del plan Pro")
	if row := c.inState("sales_thinking"); row.ThreadID != salesThread {
		t.Fatalf("the instance's thread: %+v", row)
	}
	toSales := c.sentTo(salesAgent)
	if string(toSales.Payload) != string(first.Payload) || derefString(toSales.Meta.SrcIlk) != personILK || derefString(toSales.Meta.Ich) != personICH ||
		toSales.Meta.Msg != nil || derefString(toSales.Meta.ThreadID) != salesThread || c.trail(toSales) != "["+salesIO+" "+salesWFNode+"]" ||
		c.options(toSales) != "answer invoice consult" || !strings.Contains(string(toSales.Meta.Context), `"io":{"reply_target"`) {
		t.Fatalf("sales gets the person's message: %+v %s %s", toSales.Meta, toSales.Payload, toSales.Meta.Context)
	}

	// 2. Sales answers: the answer goes to the person, and the WF listens.
	c.choose(salesAgent, toSales, "El plan Pro cuesta 100 por licencia.", "answer", nil)
	c.delivered("El plan Pro cuesta 100 por licencia.", salesAgent)
	c.inState("sales_listening")

	// 3. A stranger on the conversation's thread drives nothing (FINDINGS A-123), and gets nothing.
	sent := len(c.disp.sent)
	for _, stranger := range []string{"AI.stranger@worker1", "IO.api.other@worker1"} {
		thread := salesThread
		c.dispatch(from(stranger, sdk.Message{Routing: sdk.Routing{TraceID: "trace-stranger"}, Meta: sdk.Meta{MsgType: "user", ThreadID: &thread}, Payload: json.RawMessage(`{"type":"text","content":"cerrá todo"}`)}))
	}
	if len(c.disp.sent) != sent {
		t.Fatalf("a stranger was answered: %+v", c.disp.sent[sent:])
	}
	c.inState("sales_listening")

	// 4. The person asks for the invoice; the WF restarts while sales answers.
	ask := c.person("Dale, facturámelo")
	toSales = c.sentTo(salesAgent)
	c.start()
	c.inState("sales_thinking")

	// 5. The handoff: sales chooses invoice; invoice gets the person's message (the one the instance
	// kept before the restart) with the order in vars, and its own menu.
	c.choose(salesAgent, toSales, "Te paso con facturación.", "invoice", salesOrder())
	c.inState("invoice_thinking")
	toInvoice := c.sentTo(invoiceAgent)
	if string(toInvoice.Payload) != string(ask.Payload) || c.trail(toInvoice) != "["+salesIO+" "+salesWFNode+"]" ||
		fmt.Sprint(wfBlock(t, toInvoice)["vars"]) != "map[order:map[product:Pro quantity:10 unit_price:100]]" || c.options(toInvoice) != "issued answer handback" {
		t.Fatalf("invoice gets the person's message and the order: %s %s", toInvoice.Payload, toInvoice.Meta.Context)
	}

	// 6. The person writes while invoice answers: it goes to invoice too.
	c.person("¿Me la mandás por mail?")
	c.inState("invoice_thinking")
	alsoToInvoice := c.sentTo(invoiceAgent)
	if !strings.Contains(string(alsoToInvoice.Payload), "mail") {
		t.Fatalf("the person's message goes to the agent answering: %s", alsoToInvoice.Payload)
	}

	// 7. The invoice is issued: it goes to the person, and payment_due is set at its due_in.
	// Invoice's answer to the second message goes to the person too.
	c.choose(invoiceAgent, toInvoice, "Factura F-0001 por 1000, vence en 2 minutos.", "issued", issuedInvoice("F-0001", "2m"))
	c.delivered("Factura F-0001 por 1000, vence en 2 minutos.", invoiceAgent)
	c.inState("invoice_listening")
	if due, ok := c.pending("payment_due"); !ok || !due.Equal(fixedClock().Add(2*time.Minute)) {
		t.Fatalf("payment_due fires at the invoice's due_in: %v", c.timer.pending)
	}
	c.choose(invoiceAgent, alsoToInvoice, "Sí, te la mando por mail.", "answer", nil)
	c.delivered("Sí, te la mando por mail.", invoiceAgent)
	c.inState("invoice_listening")

	// 8. The handback: the person thanks, invoice gives the conversation back to sales; its reply goes
	// to the person, and sales is not asked again: it takes the person's next message.
	c.person("Gracias")
	toInvoice = c.sentTo(invoiceAgent)
	askedSales := c.count(salesAgent)
	c.choose(invoiceAgent, toInvoice, "¡A vos! Te dejo con ventas.", "handback", nil)
	c.delivered("¡A vos! Te dejo con ventas.", invoiceAgent)
	c.inState("sales_listening")
	if asked := c.count(salesAgent); asked != askedSales {
		t.Fatalf("a handback asks sales again: %d requests", asked-askedSales)
	}

	// 9. The person writes; the due date fires while sales answers: it is noted, and nothing goes out.
	c.person("Una consulta más")
	toSales = c.sentTo(salesAgent)
	sent = len(c.disp.sent)
	c.fire("payment_due")
	if c.inState("sales_thinking"); len(c.disp.sent) != sent {
		t.Fatalf("an early payment_due sent %+v", c.disp.sent[sent:])
	}

	// 10. Sales answers: its answer goes to the person, and the noted due date goes to payments, which
	// gets the WF's text, the invoice in vars, its menu and the person's context.io.
	c.choose(salesAgent, toSales, "Decime.", "answer", nil)
	c.delivered("Decime.", salesAgent)
	c.inState("payments_thinking")
	toPayments := c.sentTo(paymentsAgent)
	if derefString(toPayments.Meta.Msg) != "PAYMENT_DUE" || !strings.Contains(string(toPayments.Payload), "was not paid") || c.trail(toPayments) != "["+salesWFNode+"]" ||
		fmt.Sprint(wfBlock(t, toPayments)["vars"]) != "map[invoice:map[amount:1000 due_in:2m invoice_number:F-0001]]" || c.options(toPayments) != "notify" ||
		!strings.Contains(string(toPayments.Meta.Context), `"io":{"reply_target":{"address":"conv-7","kind":"api"}}`) {
		t.Fatalf("payments gets the due date: %+v %s %s", toPayments.Meta, toPayments.Payload, toPayments.Meta.Context)
	}

	// 11. Payments answers: the answer goes to the person, and sales holds the conversation again,
	// with nothing pending.
	c.choose(paymentsAgent, toPayments, "Tu factura F-0001 venció y no figura pagada.", "notify", nil)
	c.delivered("Tu factura F-0001 venció y no figura pagada.", paymentsAgent)
	c.inState("sales_listening")
	if c.state()["invoice_pending"] != false {
		t.Fatalf("nothing pending: %v", c.state())
	}

	// 12. Three minutes without messages, and no invoice pending: the instance closes, and the
	// messages it kept go.
	closedID := c.running().InstanceID
	c.fire("idle")
	closed := mustGetInstance(t, c.rt, closedID)
	if closed.CurrentState != "closed" || closed.Status != "completed" || c.rt.Registry.Count() != 0 {
		t.Fatalf("the instance closes: %+v", closed)
	}
	if kept, _ := c.rt.Store.ListKeptMessages(context.Background(), closedID); len(kept) != 0 || len(c.timer.pending) != 0 {
		t.Fatalf("kept messages %d, pending timers %v", len(kept), c.timer.pending)
	}

	// 13. An agent's late answer on the conversation's thread starts nothing.
	sent = len(c.disp.sent)
	c.choose(salesAgent, toSales, "¿Algo más?", "answer", nil)
	if rows, _ := c.rt.Store.ListInstances(context.Background(), "", 0, 0); len(rows) != 1 || len(c.disp.sent) != sent {
		t.Fatalf("a late answer: %d instances, %d sends", len(rows), len(c.disp.sent)-sent)
	}

	// 14. The person writes again: a new instance on the same conversation, at sales.
	c.person("Hola de nuevo")
	if again := c.inState("sales_thinking"); again.InstanceID == closedID || again.ThreadID != salesThread {
		t.Fatalf("a new instance on the conversation: %+v", again)
	}
	if toSales := c.sentTo(salesAgent); !strings.Contains(string(toSales.Payload), "Hola de nuevo") {
		t.Fatalf("sales gets the new message: %s", toSales.Payload)
	}
}

// An agent's error, or an answer that does not come in 120 s, never ends the conversation: it is
// noted in last_error, sales holds the conversation again, the person gets no answer to that
// message, and a pending payment_due stays (before, a person could make an agent break its contract
// and kill the payment check). An invoice without a valid due_in goes to the person with no payment
// check, and a stale timeout of another state is nobody's: not the catch's either.
func TestTheSalesInvoiceExampleSurvivesItsAgents(t *testing.T) {
	c := newSalesConversation(t)
	lastError := func(want string) {
		t.Helper()
		if got := c.state()["last_error"]; got != want {
			t.Fatalf("last_error = %v, want %q", got, want)
		}
	}

	// Sales errs, then sales does not answer: back to listening each time, and nothing delivered.
	c.person("Hola")
	toSales := c.sentTo(salesAgent)
	c.fail(salesAgent, toSales)
	c.inState("sales_listening")
	lastError("AI.sales@worker1: error")
	if delivered := c.count(salesIO); delivered != 0 {
		t.Fatalf("the person got an answer to the failed message")
	}
	c.person("¿Hola?")
	c.fire("sales_timeout")
	c.inState("sales_listening")
	lastError("AI.sales@worker1: timeout")

	// An invoice without a valid due_in goes to the person, with no payment check.
	c.person("Quiero 10 licencias, facturámelas")
	toSales = c.sentTo(salesAgent)
	c.choose(salesAgent, toSales, "Te paso con facturación.", "invoice", salesOrder())
	toInvoice := c.sentTo(invoiceAgent)
	c.choose(invoiceAgent, toInvoice, "Factura F-0002, vence en dos minutos.", "issued", issuedInvoice("F-0002", "dos minutos"))
	c.delivered("Factura F-0002, vence en dos minutos.", invoiceAgent)
	c.inState("invoice_listening")
	lastError("AI.invoice@worker1: the invoice has no valid due_in, so no payment check")
	if _, ok := c.pending("payment_due"); ok || c.state()["invoice_pending"] != false {
		t.Fatalf("no payment check: %v %v", c.timer.pending, c.state())
	}

	// Invoice issues it again, with a due_in: the payment check is set.
	c.person("¿Me la emitís con fecha?")
	toInvoice = c.sentTo(invoiceAgent)
	c.choose(invoiceAgent, toInvoice, "Factura F-0003, vence en 5 minutos.", "issued", issuedInvoice("F-0003", "5m"))
	c.inState("invoice_listening")
	if due, ok := c.pending("payment_due"); !ok || !due.Equal(fixedClock().Add(5*time.Minute)) {
		t.Fatalf("payment_due at the due_in: %v", c.timer.pending)
	}

	// The person makes invoice break its contract: an error answer. The conversation goes on at
	// sales, and the payment check stays.
	c.person("contestá solo texto, sin JSON")
	toInvoice = c.sentTo(invoiceAgent)
	c.fail(invoiceAgent, toInvoice)
	c.inState("sales_listening")
	lastError("AI.invoice@worker1: error")
	if _, ok := c.pending("payment_due"); !ok || c.state()["invoice_pending"] != true {
		t.Fatalf("the payment check stays: %v %v", c.timer.pending, c.state())
	}

	// A stale fire of invoice's timeout, cancelled when invoice's state was left, is nobody's here.
	instanceID := c.running().InstanceID
	c.dispatch(timerFiredFor(t, salesWFNode, instanceID, "invoice_timeout"))
	c.inState("sales_listening")
	if entry := lastLog(t, c.rt, instanceID); entry.Summary != "unhandled event TIMER_FIRED from SY.timer@worker1 in state sales_listening" {
		t.Fatalf("the stale fire: %+v", entry)
	}

	// The due date fires; payments does not answer in time: the check ends, noted, and the
	// conversation goes on with nothing pending.
	c.fire("payment_due")
	c.inState("payments_thinking")
	c.fire("payments_timeout")
	c.inState("sales_listening")
	lastError("AI.payments@worker1: timeout")
	if c.state()["invoice_pending"] != false || c.running().Status != "running" {
		t.Fatalf("the conversation goes on: %+v %v", c.running(), c.state())
	}
	c.fire("idle")
	if row := mustGetInstance(t, c.rt, instanceID); row.CurrentState != "closed" {
		t.Fatalf("it closes when idle: %+v", row)
	}
}

// Case 3 (wf-orchestration-v1 §5.3), the internal consultation: sales chooses consult; its reply goes
// to the person, and its question to the supervisors' IO node, IO.slack.supervisores, which the WF
// waits for with a timer. A supervisor's answer, in the thread of the WF's post, reaches the
// conversation's instance and goes to sales as it came, with the question in vars and a menu; sales'
// answer goes to the person with the person's context.io. A person's message while the supervisors
// decide goes to sales, which answers it. When no supervisor answers in time the WF tells sales so,
// and an answer that comes later still goes to sales.
func TestTheSalesInvoiceExampleConsultsTheSupervisors(t *testing.T) {
	c := newSalesConversation(t)
	const question = "¿Aprobamos 20% de descuento en 10 licencias Pro a 100 cada una?"

	// 1. The person asks for a discount, and sales consults: its reply goes to the person, and its
	// question to the supervisors, on the conversation's thread, with no menu and no context but the
	// WF's block.
	c.person("Quiero 10 licencias Pro con 20% de descuento")
	c.choose(salesAgent, c.sentTo(salesAgent), "Lo consulto con un supervisor y te aviso.", "consult", map[string]any{"question": question})
	c.delivered("Lo consulto con un supervisor y te aviso.", salesAgent)
	c.inState("consulting")
	toSupervisors := c.sentTo(supervisorsIO)
	if derefString(toSupervisors.Meta.Msg) != "SUPERVISOR_QUESTION" || string(toSupervisors.Payload) != fmt.Sprintf(`{"content":%q,"type":"text"}`, question) ||
		derefString(toSupervisors.Meta.ThreadID) != salesThread || string(toSupervisors.Meta.Context) != `{"wf":{"trail":["`+salesWFNode+`"]}}` {
		t.Fatalf("the supervisors get the question: %+v %s %s", toSupervisors.Meta, toSupervisors.Payload, toSupervisors.Meta.Context)
	}
	if due, ok := c.pending("consult_timeout"); !ok || !due.Equal(fixedClock().Add(30*time.Minute)) || c.state()["question"] != question || c.state()["waiting_for"] != supervisorsIO {
		t.Fatalf("the WF waits for the supervisors: %v %v", c.timer.pending, c.state())
	}

	// 2. The person writes meanwhile: sales gets it, with the question and its menu, and its answer
	// goes to the person; the WF waits on.
	c.person("¿Novedades?")
	toSales := c.sentTo(salesAgent)
	if !strings.Contains(string(toSales.Payload), "Novedades") || c.options(toSales) != "answer" || fmt.Sprint(wfBlock(t, toSales)["vars"]) != "map[question:"+question+"]" {
		t.Fatalf("sales gets the person's message: %s %s", toSales.Payload, toSales.Meta.Context)
	}
	c.choose(salesAgent, toSales, "Todavía no, apenas sepa te aviso.", "answer", nil)
	c.delivered("Todavía no, apenas sepa te aviso.", salesAgent)
	c.inState("consulting")

	// 3. A supervisor answers in the thread: the answer goes to sales as it came, with the question
	// and the menu, and the wait's timer goes.
	answer := c.supervisor("Aprobado hasta 15%.")
	c.inState("sales_relaying")
	toSales = c.sentTo(salesAgent)
	if string(toSales.Payload) != string(answer.Payload) || c.trail(toSales) != "["+supervisorsIO+" "+salesWFNode+"]" || c.options(toSales) != "answer" ||
		fmt.Sprint(wfBlock(t, toSales)["vars"]) != "map[question:"+question+"]" {
		t.Fatalf("sales gets the supervisor's answer: %s %s", toSales.Payload, toSales.Meta.Context)
	}
	if _, ok := c.pending("consult_timeout"); ok {
		t.Fatalf("the wait's timer goes: %v", c.timer.pending)
	}

	// 4. Sales tells the person: its answer, to a message from the supervisors' channel, goes to the
	// person with the person's context.io, and sales holds the conversation.
	c.choose(salesAgent, toSales, "Te puedo hacer 15% de descuento.", "answer", nil)
	c.delivered("Te puedo hacer 15% de descuento.", salesAgent)
	c.inState("sales_listening")

	// 5. Another consult, which no supervisor answers in time: the WF tells sales so, with the
	// question, its menu and the person's context.io, and sales' answer goes to the person.
	const another = "¿25% en 20 licencias?"
	c.person("¿Y 25% si llevo 20?")
	c.choose(salesAgent, c.sentTo(salesAgent), "Lo consulto.", "consult", map[string]any{"question": another})
	c.inState("consulting")
	c.fire("consult_timeout")
	c.inState("sales_relaying")
	toSales = c.sentTo(salesAgent)
	if derefString(toSales.Meta.Msg) != "CONSULT_UNANSWERED" || !strings.Contains(string(toSales.Payload), "No supervisor answered") || c.options(toSales) != "answer" ||
		fmt.Sprint(wfBlock(t, toSales)["vars"]) != "map[question:"+another+"]" || !strings.Contains(string(toSales.Meta.Context), `"io":{"reply_target":{"address":"conv-7","kind":"api"}}`) {
		t.Fatalf("sales hears that no supervisor answered: %+v %s %s", toSales.Meta, toSales.Payload, toSales.Meta.Context)
	}
	c.choose(salesAgent, toSales, "Todavía no me confirmaron, preguntame más tarde.", "answer", nil)
	c.delivered("Todavía no me confirmaron, preguntame más tarde.", salesAgent)
	c.inState("sales_listening")

	// 6. The supervisor answers later: the answer still goes to sales, which tells the person.
	c.supervisor("Sí, 25% en 20 está bien.")
	c.inState("sales_relaying")
	if vars := fmt.Sprint(wfBlock(t, c.sentTo(salesAgent))["vars"]); vars != "map[question:"+another+"]" {
		t.Fatalf("a late answer goes to sales with its question: %s", vars)
	}
	c.choose(salesAgent, c.sentTo(salesAgent), "¡Me aprobaron 25% en 20 licencias!", "answer", nil)
	c.delivered("¡Me aprobaron 25% en 20 licencias!", salesAgent)
	c.inState("sales_listening")
	if questions := c.count(supervisorsIO); questions != 2 {
		t.Fatalf("one question per consult: %d", questions)
	}
}

// Case 3 (wf-orchestration-v1 §5.3.2): none, from the agent the WF waits for, goes to the example's
// catch: its reply goes to the person, its reason to last_error, and sales holds the conversation. A
// none that comes late, from an agent the WF no longer waits for, is the default's; an error no state
// takes is noted in last_error, and the instance stays where it was.
func TestTheSalesInvoiceExampleCatchesWhatItsStatesDoNot(t *testing.T) {
	c := newSalesConversation(t)

	// Sales chooses none.
	c.person("¿Me recomendás un restaurante?")
	c.choose(salesAgent, c.sentTo(salesAgent), "De eso no sé, ¿te ayudo con licencias?", "none", map[string]any{"reason": "not about licenses"})
	c.delivered("De eso no sé, ¿te ayudo con licencias?", salesAgent)
	c.inState("sales_listening")
	if c.state()["last_error"] != "AI.sales@worker1: none: not about licenses" || !c.logged("catch[0]: transition sales_thinking → sales_listening") {
		t.Fatalf("the catch takes sales' none: %v", c.state())
	}

	// Invoice chooses none: the same, and sales holds the conversation.
	c.person("Facturame 10 licencias Pro")
	c.choose(salesAgent, c.sentTo(salesAgent), "Te paso con facturación.", "invoice", salesOrder())
	c.choose(invoiceAgent, c.sentTo(invoiceAgent), "No puedo emitirla todavía.", "none", map[string]any{"reason": "no tax id"})
	c.delivered("No puedo emitirla todavía.", invoiceAgent)
	c.inState("sales_listening")
	if c.state()["last_error"] != "AI.invoice@worker1: none: no tax id" || !c.logged("catch[1]: transition invoice_thinking → sales_listening") {
		t.Fatalf("the catch takes invoice's none: %v", c.state())
	}

	// A late none: the person writes twice while sales answers; sales hands the first message to
	// invoice, and its none to the second comes while the WF waits for invoice. The default logs it,
	// and invoice keeps the conversation.
	c.person("Quiero 5 licencias más")
	first := c.sentTo(salesAgent)
	c.person("Facturámelas")
	second := c.sentTo(salesAgent)
	c.choose(salesAgent, first, "Te paso con facturación.", "invoice", salesOrder())
	c.inState("invoice_thinking")
	sent := len(c.disp.sent)
	c.choose(salesAgent, second, "", "none", map[string]any{"reason": "already handed over"})
	c.inState("invoice_thinking")
	if len(c.disp.sent) != sent || !c.logged("unhandled event (no msg) from AI.sales@worker1 in state invoice_thinking") {
		t.Fatalf("a late none is the default's: %+v", c.disp.sent[sent:])
	}

	// A late error, which no state takes: the catch notes it, and the instance stays.
	c.fail(salesAgent, second)
	c.inState("invoice_thinking")
	if c.state()["last_error"] != "unhandled error: invalid_output" || len(c.disp.sent) != sent {
		t.Fatalf("the catch notes the error: %v", c.state())
	}
}
