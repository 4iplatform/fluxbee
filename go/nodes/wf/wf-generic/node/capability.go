package node

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"strings"
	"unicode/utf8"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

// A capability's door (wf-orchestration-v1 §5.6, docs/digital-executive-v1-stage-3.md §5 and §6.1):
// a WF whose start_from names the ED's conscious, or the consciouses of its tenant's EDs. The
// conscious asks it with a CAPABILITY_REQUEST, a system message it waits on by its trace (the Rust
// SDK's send_system_rpc); the door starts an instance for each request, which answers it once with
// reply (CAPABILITY_REQUEST_RESPONSE, on the request's trace), and, when the reply said pending, gives
// the result later with notify, a user message to the conscious on the person's conversation.
const (
	MsgCapabilityRequest         = "CAPABILITY_REQUEST"
	MsgCapabilityRequestResponse = "CAPABILITY_REQUEST_RESPONSE"

	// contextCapabilityResult is the key of a notify's result in its message's meta.context.
	contextCapabilityResult = "capability_result"

	// The codes the door answers a request it starts no instance for with, status error: a payload
	// that is no request, an input its input_schema refuses, and a sender that is no conscious it
	// takes requests from (errCodeUnauthorized). An engine that cannot serve answers why, as its
	// control verbs do (node_not_configured, store_unavailable).
	errCodeInvalidRequest = "INVALID_REQUEST"
	errCodeInvalidInput   = "INVALID_INPUT"

	// maxResultText is how much of a result a notify's text shows, the rest cut with a mark: the
	// whole result is in its meta.context.capability_result.
	maxResultText = 8 * 1024
)

// capabilityRequest is the request that started a door's instance, which the store keeps with it
// (request_json): what its reply answers, and what its notify names.
type capabilityRequest struct {
	RequestID string `json:"request_id"`
	// EDILK is the ED the request is for, its conscious's speaks_as, checked in identity.
	EDILK string `json:"ed_ilk"`
	// Sender is the conscious that asked, by its router-stamped name: a notify goes to it, and only
	// to it. SenderUUID and TraceID are where the reply goes, as the response to the request, which
	// the conscious waits on by its trace.
	Sender     string `json:"sender"`
	SenderUUID string `json:"sender_uuid"`
	TraceID    string `json:"trace_id"`
	// Conversation is the person's conversation as the request carried it ({thread_id, person_ilk,
	// io_node, reply_target}), which a notify gives back as received.
	Conversation json.RawMessage `json:"conversation,omitempty"`
	// Replied is the status of the reply that answered the request, "" before one did: a request is
	// answered once, and a notify comes after a reply pending.
	Replied string `json:"replied,omitempty"`
}

// capabilityRequestPayload is a CAPABILITY_REQUEST's payload.
type capabilityRequestPayload struct {
	RequestID    string          `json:"request_id"`
	EDILK        string          `json:"ed_ilk"`
	Input        json.RawMessage `json:"input"`
	Conversation json.RawMessage `json:"conversation"`
}

// isCapabilityRequest reports whether msg is a CAPABILITY_REQUEST.
func isCapabilityRequest(msg sdk.Message) bool {
	return sdk.IsSystemKind(msg.Meta.MsgType) && rawMsgName(msg) == MsgCapabilityRequest
}

// requestRefusal is why the door starts no instance for a request: the code and the message its
// answer carries.
type requestRefusal struct {
	code, message string
}

// takeCapabilityRequest takes a CAPABILITY_REQUEST (Dispatch): it starts an instance of the current
// definition, as a start_from message does, once admitRequest admits it; otherwise it answers it,
// status error, with why. While the engine waits for its store, a request it would admit is answered
// store_unavailable; without a config in use, node_not_configured.
func (rt *NodeRuntime) takeCapabilityRequest(ctx context.Context, msg sdk.Message) error {
	def := rt.Def
	if def == nil && rt.storeDown != nil {
		def = rt.storeDown.def
	}
	if def == nil {
		return rt.refuseRequest(msg, requestRefusal{errCodeNodeNotConfigured, rt.Plane.notConfiguredReason()})
	}
	req, input, refusal := rt.admitRequest(def, msg)
	if refusal != nil {
		return rt.refuseRequest(msg, *refusal)
	}
	if rt.Def == nil {
		return rt.refuseRequest(msg, requestRefusal{errCodeStoreUnavailable, rt.storeDown.cause.Message})
	}
	instanceID := newInstanceID()
	inboundID, err := rt.journal(ctx, instanceID, inboundCreate, msg, "")
	if err != nil {
		return err
	}
	return rt.runCreation(ctx, msg, instanceID, input, inboundID, req)
}

// admitRequest is the request msg makes for an instance of def, and its input, or why it makes none
// (docs/digital-executive-v1-stage-3.md §5; the stage's wire contract §4):
//   - its payload is a request: a request_id and an ed_ilk (INVALID_REQUEST otherwise);
//   - its sender is a conscious def takes requests from (TakesRequestsFrom), and identity says it is
//     ed_ilk's conscious, for an ED this door serves (checkConscious; UNAUTHORIZED otherwise);
//   - its input is a JSON object, or none, that def's input_schema accepts (INVALID_INPUT otherwise).
func (rt *NodeRuntime) admitRequest(def *WorkflowDefinition, msg sdk.Message) (*capabilityRequest, map[string]any, *requestRefusal) {
	var payload capabilityRequestPayload
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		return nil, nil, &requestRefusal{errCodeInvalidRequest, fmt.Sprintf("the payload is no %s: %v", MsgCapabilityRequest, err)}
	}
	payload.RequestID, payload.EDILK = strings.TrimSpace(payload.RequestID), strings.TrimSpace(payload.EDILK)
	switch {
	case payload.RequestID == "":
		return nil, nil, &requestRefusal{errCodeInvalidRequest, "request_id is required"}
	case payload.EDILK == "":
		return nil, nil, &requestRefusal{errCodeInvalidRequest, "ed_ilk is required"}
	}
	conversation := bytes.TrimSpace(payload.Conversation)
	if len(conversation) > 0 && !bytes.Equal(conversation, []byte("null")) {
		if _, err := wfcel.DecodeJSONObject(conversation); err != nil {
			return nil, nil, &requestRefusal{errCodeInvalidRequest, "conversation is not a JSON object"}
		}
	} else {
		conversation = nil
	}
	sender := msg.SourceL2Name()
	if !def.TakesRequestsFrom(sender) {
		return nil, nil, &requestRefusal{errCodeUnauthorized, fmt.Sprintf("%s takes capability requests only from the consciouses its start_from takes, not from %q", rt.NodeName, sender)}
	}
	if _, err := rt.checkConscious(sender, payload.EDILK); err != nil {
		return nil, nil, &requestRefusal{errCodeUnauthorized, err.Error()}
	}
	input := map[string]any{}
	if raw := bytes.TrimSpace(payload.Input); len(raw) > 0 && !bytes.Equal(raw, []byte("null")) {
		decoded, err := wfcel.DecodeJSONObject(raw)
		if err != nil {
			return nil, nil, &requestRefusal{errCodeInvalidInput, "input is not a JSON object"}
		}
		input = decoded
	}
	if err := validateInputPayload(def, input); err != nil {
		return nil, nil, &requestRefusal{errCodeInvalidInput, err.Error()}
	}
	return &capabilityRequest{
		RequestID:    payload.RequestID,
		EDILK:        payload.EDILK,
		Sender:       sender,
		SenderUUID:   msg.Routing.Src,
		TraceID:      msg.Routing.TraceID,
		Conversation: json.RawMessage(conversation),
	}, input, nil
}

// checkConscious checks in identity that sender, the router-stamped name of a request's sender, is
// the conscious of an ED this door serves, and returns that ED (the stage's wire contract §4):
//   - sender's node ILK (the one whose handler_node is sender) works for an ED as its conscious, and
//     for edILK when it is not "" (a request names its ED; WF_HELP names none, and the ED is the one
//     sender works for);
//   - a door of one ED, whose own ILK works for that ED, serves that ED only;
//   - a door for all of its tenant's EDs, whose own ILK works for nobody, serves a person of its
//     tenant.
func (rt *NodeRuntime) checkConscious(sender, edILK string) (string, error) {
	id := rt.identity()
	_, facts, found, err := id.ilkByNode(sender)
	switch {
	case err != nil:
		return "", fmt.Errorf("identity could not be read: %v", err)
	case !found:
		return "", fmt.Errorf("%s has no ILK in identity", sender)
	case facts.WorkRole != workRoleConscious || facts.WorksFor == "":
		return "", fmt.Errorf("%s is no ED's conscious in identity", sender)
	case edILK != "" && !strings.EqualFold(facts.WorksFor, edILK):
		return "", fmt.Errorf("%s is the conscious of %s, not of %s", sender, facts.WorksFor, edILK)
	}
	ed := facts.WorksFor
	var door ilkFacts
	if selfILK := strings.TrimSpace(rt.ActCtx.SelfILK); selfILK != "" {
		if door, found, err = id.ilk(selfILK); err != nil || !found {
			return "", fmt.Errorf("this capability's own ILK %s is not active in identity (%v)", selfILK, err)
		}
	} else if _, door, found, err = id.ilkByNode(rt.NodeName); err != nil || !found {
		return "", fmt.Errorf("this capability's own ILK (%s) is not in identity (%v)", rt.NodeName, err)
	}
	if door.WorksFor != "" {
		if !strings.EqualFold(door.WorksFor, ed) {
			return "", fmt.Errorf("this capability is %s's, not %s's", door.WorksFor, ed)
		}
		return ed, nil
	}
	person, found, err := id.ilk(ed)
	switch {
	case err != nil || !found:
		return "", fmt.Errorf("%s is not active in identity (%v)", ed, err)
	case person.Type != "human":
		return "", fmt.Errorf("%s is no person", ed)
	case person.TenantID == "" || !strings.EqualFold(person.TenantID, door.TenantID):
		return "", fmt.Errorf("%s is not of this capability's tenant (%s)", ed, door.TenantID)
	}
	return ed, nil
}

// fromAConscious reports whether sender, the router-stamped name of a message's sender, is a conscious
// def takes requests from (docs/digital-executive-v1-stage-3.md §5): an agent its start_from names,
// or, under the consciouses marker, an agent whose ILK identity names a conscious. Such a sender
// starts and drives an instance only with a CAPABILITY_REQUEST: any other message of it, the answer
// of a conscious whose SDK took no capability_result (its ED not confirmed then, or the door not
// one of its ED's capabilities in identity, which it then answers as an ordinary message back to its
// sender), is dropped and never answered (dispatchWorkflowEvent), so A-118's loop cannot start from
// the conscious's side either. Under the marker a capability's own agents are no consciouses: their
// answers go on as any agent's.
func (rt *NodeRuntime) fromAConscious(def *WorkflowDefinition, sender string) bool {
	if !def.TakesRequestsFrom(sender) {
		return false
	}
	for _, starter := range def.StartFrom {
		if starter.Consciouses == "" && starter.Node == sender {
			return true
		}
	}
	_, facts, found, err := rt.identity().ilkByNode(sender)
	if err != nil {
		log.Printf("wf: identity could not be read for %q: %v", sender, err)
	}
	return found && facts.WorkRole == workRoleConscious
}

// identity is what the door reads identity through: the runtime's, or the identity SHM of the node's
// hive.
func (rt *NodeRuntime) identity() identityView {
	if rt.Identity != nil {
		return rt.Identity
	}
	return shmIdentity{hive: nodeHive(rt.NodeName)}
}

// helpAskedByConscious reports whether sender, the router-stamped name of a WF_HELP's sender, is a
// conscious the definition takes requests from (TakesRequestsFrom, then checkConscious): the help is
// a capability's interface, which its conscious reads (the stage's wire contract §3).
func (rt *NodeRuntime) helpAskedByConscious(sender string) bool {
	def := rt.Def
	if def == nil && rt.storeDown != nil {
		def = rt.storeDown.def
	}
	if !def.TakesRequestsFrom(sender) {
		return false
	}
	if _, err := rt.checkConscious(sender, ""); err != nil {
		log.Printf("wf: %s from %q refused: %v", MsgWFHelp, sender, err)
		return false
	}
	return true
}

// refuseRequest answers msg, a CAPABILITY_REQUEST, status error, for refusal: the response to it, on
// its trace, which its sender waits on.
func (rt *NodeRuntime) refuseRequest(msg sdk.Message, refusal requestRefusal) error {
	var named struct {
		RequestID string `json:"request_id"`
	}
	_ = json.Unmarshal(msg.Payload, &named)
	log.Printf("wf: %s from %q refused (request_id=%q trace_id=%s): %s: %s",
		MsgCapabilityRequest, msg.SourceL2Name(), named.RequestID, msg.Routing.TraceID, refusal.code, refusal.message)
	if rt.ActCtx.Dispatcher == nil {
		return nil
	}
	answer, err := sdk.BuildSystemResponse(&msg, rt.NodeUUID, MsgCapabilityRequestResponse, map[string]any{
		"request_id": strings.TrimSpace(named.RequestID),
		"status":     wfcel.AnswerError,
		"error_code": refusal.code,
		"message":    refusal.message,
	}, sdk.SystemEnvelopeOptions{})
	if err != nil {
		return fmt.Errorf("%s not sent: %w", MsgCapabilityRequestResponse, err)
	}
	if err := rt.ActCtx.Dispatcher.SendMsg(answer, sdk.TraceNote{}); err != nil {
		return fmt.Errorf("%s not sent: %w", MsgCapabilityRequestResponse, err)
	}
	return nil
}

// alreadyRepliedError is a reply to a request already answered: it is not sent, whatever its
// on_error, and the transition goes on (runActions), the instance's log and the node's saying so.
type alreadyRepliedError struct {
	requestID, status string
}

func (e alreadyRepliedError) Error() string {
	return fmt.Sprintf("reply refused: request %s was answered already (%s), and a request is answered once", e.requestID, e.status)
}

// execReply answers the request that started the instance (wf-orchestration-v1 §5.6): a
// CAPABILITY_REQUEST_RESPONSE, a system message to the conscious that asked, on the request's trace,
// {request_id, status, result?, error_code?, message?}. A request is answered once: a second reply is
// refused (alreadyRepliedError). The request goes as answered with the instance's commit.
func execReply(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if actx.Dispatcher == nil {
		return fmt.Errorf("reply: no dispatcher configured")
	}
	req := inst.request
	if req == nil {
		return fmt.Errorf("reply: the instance was not started by a capability request")
	}
	if req.Replied != "" {
		return alreadyRepliedError{requestID: req.RequestID, status: req.Replied}
	}
	answer, err := answerOf(action, inst, event)
	if err != nil {
		return fmt.Errorf("reply: %w", err)
	}
	answer["request_id"] = req.RequestID
	raw, err := wfcel.EncodeJSON(answer)
	if err != nil {
		return fmt.Errorf("reply: marshal: %w", err)
	}
	to := req.SenderUUID
	if to == "" {
		to = req.Sender
	}
	// The response to the request, as the SDK builds one: to its sender, on its trace.
	asked := sdk.Message{Routing: sdk.Routing{Src: to, TTL: 16, TraceID: req.TraceID}}
	msg, err := sdk.BuildSystemResponse(&asked, actx.Dispatcher.NodeUUID(), MsgCapabilityRequestResponse, json.RawMessage(raw), sdk.SystemEnvelopeOptions{})
	if err != nil {
		return fmt.Errorf("reply: %w", err)
	}
	if err := sendWithRetry(ctx, actx, "reply", msg, sdk.TraceNote{}); err != nil {
		return err
	}
	req.Replied = action.Status
	return nil
}

// execNotify gives the conscious that asked the result of its request, after a reply pending
// (wf-orchestration-v1 §5.6; docs/digital-executive-v1-stage-3.md §6.1): a user message to it, and
// only to it, on the person's conversation (meta.thread_id, the conversation's thread_id), whose
// meta.context.capability_result is {request_id, capability (the door's node name), status, result?,
// error_code?, message?, conversation (as the request carried it)}, and whose payload is a short text
// rendering the result for the model. The conscious answers the person through the IO node the
// conversation names, never the WF.
func execNotify(ctx context.Context, action ActionDefinition, inst *WFInstance, event sdk.Message, actx ActionContext) error {
	if actx.Dispatcher == nil {
		return fmt.Errorf("notify: no dispatcher configured")
	}
	req := inst.request
	switch {
	case req == nil:
		return fmt.Errorf("notify: the instance was not started by a capability request")
	case req.Replied != wfcel.AnswerPending:
		return fmt.Errorf("notify: a notify gives the result after a reply pending, and request %s was %s", req.RequestID, repliedWords(req.Replied))
	}
	thread, err := req.thread()
	if err != nil {
		return fmt.Errorf("notify: %w", err)
	}
	answer, err := answerOf(action, inst, event)
	if err != nil {
		return fmt.Errorf("notify: %w", err)
	}
	capability := actx.Dispatcher.NodeL2Name()
	result := map[string]any{"request_id": req.RequestID, "capability": capability}
	for key, value := range answer {
		result[key] = value
	}
	if req.Conversation != nil {
		result["conversation"] = req.Conversation
	}
	metaContext := map[string]any{contextCapabilityResult: result}
	inst.withCycleTrace(metaContext)
	metaContext[sdk.WriterKey] = actx.writer()
	contextRaw, err := wfcel.EncodeJSON(metaContext)
	if err != nil {
		return fmt.Errorf("notify: marshal meta.context: %w", err)
	}
	text, err := renderCapabilityResult(capability, req.RequestID, answer)
	if err != nil {
		return fmt.Errorf("notify: %w", err)
	}
	payload, err := wfcel.EncodeJSON(map[string]any{"type": "text", "content": text})
	if err != nil {
		return fmt.Errorf("notify: marshal payload: %w", err)
	}
	msg := sdk.Message{
		Routing: sdk.Routing{
			Src:     actx.Dispatcher.NodeUUID(),
			Dst:     sdk.UnicastDestination(req.Sender),
			TTL:     16,
			TraceID: transitionTraceID(inst, event),
		},
		Meta: sdk.Meta{
			MsgType:  wfMsgType,
			ThreadID: &thread,
			Context:  contextRaw,
		},
		Payload: payload,
	}
	return sendWithRetry(ctx, actx, "notify", msg, traceNote("notify", actx))
}

// repliedWords says how a request was answered, for an error.
func repliedWords(status string) string {
	if status == "" {
		return "not answered yet"
	}
	return "answered " + status
}

// thread is the person's thread the request's conversation names, the one a notify goes on.
func (req *capabilityRequest) thread() (string, error) {
	var conversation struct {
		ThreadID string `json:"thread_id"`
	}
	if req.Conversation != nil {
		if err := json.Unmarshal(req.Conversation, &conversation); err != nil {
			return "", fmt.Errorf("the request's conversation: %w", err)
		}
	}
	if strings.TrimSpace(conversation.ThreadID) == "" {
		return "", fmt.Errorf("request %s carried no conversation.thread_id, the person's thread a notify goes on", req.RequestID)
	}
	return conversation.ThreadID, nil
}

// answerOf is what a reply or a notify answers: its status, its result, $ref-resolved as a payload,
// its error_code and its message, each when it has one.
func answerOf(action ActionDefinition, inst *WFInstance, event sdk.Message) (map[string]any, error) {
	answer := map[string]any{"status": action.Status}
	if action.Result != nil {
		result, err := Resolve(action.Result, inst.Input, inst.StateVars, messageToMap(event))
		if err != nil {
			return nil, fmt.Errorf("resolve result: %w", err)
		}
		answer["result"] = result
	}
	if action.ErrorCode != "" {
		answer["error_code"] = action.ErrorCode
	}
	if action.Message != "" {
		answer["message"] = action.Message
	}
	return answer, nil
}

// renderCapabilityResult is a notify's text: what the capability says of the request, for the model,
// with its result as JSON, cut at maxResultText.
func renderCapabilityResult(capability, requestID string, answer map[string]any) (string, error) {
	var text strings.Builder
	if answer["status"] == wfcel.AnswerError {
		fmt.Fprintf(&text, "The capability %s could not complete your request %s: error %v.", capability, requestID, answer["error_code"])
	} else {
		fmt.Fprintf(&text, "The capability %s completed your request %s.", capability, requestID)
	}
	if message, ok := answer["message"].(string); ok {
		text.WriteString(" " + message)
	}
	if result, ok := answer["result"]; ok {
		raw, err := wfcel.EncodeJSON(result)
		if err != nil {
			return "", fmt.Errorf("marshal result: %w", err)
		}
		shown := string(raw)
		if len(shown) > maxResultText {
			cut := maxResultText
			for cut > 0 && !utf8.RuneStart(shown[cut]) {
				cut--
			}
			shown = shown[:cut] + " [cut]"
		}
		text.WriteString("\nResult: " + shown)
	}
	return text.String(), nil
}

// requestersSend refuses a send_message or a forward to the conscious whose request started the
// instance (wf-orchestration-v1 §5.6): only reply and notify reach it. The language refuses a send to
// a conscious start_from names; with the consciouses marker the one that asked is known only at run
// time.
func (inst *WFInstance) requestersSend(verb, target string) error {
	if inst.request == nil || !strings.EqualFold(strings.TrimSpace(target), inst.request.Sender) {
		return nil
	}
	return fmt.Errorf("%s: %s is the conscious whose request started the instance: only reply and notify reach it (the WF↔AI loop, FINDINGS A-118)", verb, target)
}
